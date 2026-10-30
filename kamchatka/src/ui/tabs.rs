//! The four tab bodies. The chrome around them is in `mod.rs`; the panel that floats over them is
//! in `overlay`.
//!
//! note: each tab gets the whole window because each tab is a whole view; what each one is for is
//! on [the crate root](crate). The fourth lists every answer given to the policy and a count of
//! what is still a question, not a row per undecided thing. `ask` is what the policy does when
//! nobody has told it anything; a screenful of `ask` buries the line that says what can happen
//! without stopping.

use std::{cell::Cell, collections::HashMap, rc::Rc};

use nachalnik::{ContextId, ContextItem, ContextKind, ContextState, Verdict};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{List, ListItem, Paragraph},
};

use tui_markdown::StyleSheet as _;

use crate::{
    app::when::read_off,
    app::{App, Focus, Going, Said, Speaker},
    tools::{Careful, Exit},
    ui::{
        markdown::{Chunk, Markdown, broken, chunks, highlighted, markdown, rule, separate},
        table::{draw_table, table},
        text::{clip, columns, fitted, gutter, pad, refit, wrapped},
    },
};

use super::{GREETING, Scrolled, faint, in_view, quiet};

// ------------------------------------------------------------------------------ the conversation

pub(super) fn draw_chat(frame: &mut Frame, app: &mut App, going: &Going, inner: Rect) -> Scrolled {
    let width = inner.width as usize;
    let viewport = inner.height as usize;
    // read here and written back at the end, since the blocks below borrow the app
    let (follow, mut scroll) = (app.follow, app.scroll);
    // held by the loop, because the lines borrow their words out of these rather than copying
    // the whole conversation once a frame to show what it was already showing
    let items = app.kernel.items();
    // the turns holding a call waiting on an answer. The projector leaves them out of the request;
    // that is not the same as being left out. See `held` below
    let waiting: Vec<_> = app
        .kernel
        .pending_permissions()
        .into_iter()
        .map(|request| request.call)
        .collect();
    let deciding: Vec<ContextId> = items
        .iter()
        .filter(|item| item.calls().any(|call| waiting.contains(&call.id)))
        .map(|item| item.id)
        .collect();
    // see `App::greet` and `App::tell`: lines for this screen alone, above the conversation
    let aside = app
        .greeting
        .then_some(GREETING)
        .into_iter()
        .chain(app.aside.iter().map(String::as_str))
        .map(|text| Said {
            speaker: Speaker::Note,
            text: text.into(),
            item: None,
        });

    // every block of the conversation, in order, with nothing drawn yet
    //
    // note: drawn only where a frame shows it. Drawing every block on every frame cost a frame
    // in proportion to the session: a resize, or the first frame of a resumed session, drew every
    // answer's markdown at the new width, about a third of a second for a thousand turns. See
    // `Heights` for how the rows of the rest are counted.
    let mut blocks: Vec<Block> = Vec::new();
    // the item the last marked line belonged to, so a turn and its calls say once between them why
    // they are not going rather than once each
    let mut marked: Option<ContextId> = None;
    for said in aside.chain(app.conversation(&items, going)) {
        // what the model is no longer shown is drawn so the eye can tell without reading it. It
        // uses `going.sends_content` rather than the state because an item the projector repaired
        // away is `Active` and not in the request. A line with no item - a note, an error,
        // something still arriving - is left as it is.
        //
        // note: `is_elided` is handled by `App::conversation`. What remains here is an item a
        // projector took out of a request it is otherwise in: a second result for a call that
        // already has one, not one whose call is only waiting to be answered. It is not a decision
        // anyone made and has no marker, so it keeps the rule down its left and the line saying
        // why.
        //
        // note: the projector leaves out a turn while its call has no result, because a call with
        // no answer is a request most providers reject. So `sends_content` says no for the whole
        // time the permission prompt is open, and the line saying why would sit above the call the
        // person is asked to authorise and call it a fault. It is mid-flight, not left out;
        // answering the question already on screen puts it in
        let held = said.item.filter(|item| {
            !going.sends_content(item) && !item.state.is_elided() && !deciding.contains(&item.id)
        });
        let mark = match held {
            Some(item) if marked != Some(item.id) => {
                marked = Some(item.id);
                Some(item)
            }
            Some(_) => None,
            None => {
                marked = None;
                None
            }
        };
        // note: an elided item already reads as its marker here: the projector's words in brackets,
        // the text the model reads. The substitution is `App::conversation`'s: what an elision
        // means is not a rendering decision, and a second client must not receive the content of an
        // item whose point is that the model no longer has it. What is left is to draw it with the
        // speaker's prefix and dimmed: `> ` still says whose turn it was, and the dimming says
        // nothing is left to read
        let look = match (held, said.speaker) {
            (Some(_), _) => Look::Held,
            (None, Speaker::Model) => Look::Answer,
            (None, speaker) => Look::Said(speaker),
        };
        blocks.push(Block {
            look,
            text: said.text,
            mark,
        });
    }

    // what the answers came to on the last frame, and what they come to on this one; see `Drawn`
    let mut kept = DRAWN.take().at(width);
    let mut fresh = HashMap::new();
    let counted = HEIGHTS.take();
    let keys: Vec<Key> = blocks.iter().map(Block::key).collect();
    // how many rows each block takes, and whether that is measured or estimated
    let mut heights: Vec<(usize, bool)> = blocks
        .iter()
        .zip(&keys)
        .map(|(block, key)| {
            counted
                .at(width, key)
                .unwrap_or_else(|| (block.estimate(width), false))
        })
        .collect();
    let mut drawn: HashMap<usize, Rc<[Line<'static>]>> = HashMap::new();
    // draws a block, once a frame, and counts it
    let mut draw = |at: usize, heights: &mut Vec<(usize, bool)>| {
        let rows = drawn
            .entry(at)
            .or_insert_with(|| blocks[at].draw(width, going, &mut kept, &mut fresh));
        heights[at] = (rows.len(), true);
        Rc::clone(rows)
    };
    // where the block `at` starts, by what is counted so far
    let start = |at: usize, heights: &[(usize, bool)]| -> usize {
        heights[..at].iter().map(|(rows, _)| rows).sum()
    };

    // where the view is held, when it is not following: the block its top row is in, and how far
    // into it. Found from the block the last frame showed and however far a key has moved it
    // since, counted in this frame's rows; see `Anchor`
    //
    // note: `scroll` at zero is the top whatever moved it there, since that is what `ctrl+home`
    // asks for, and a top reached by counting from a block below it is off by every estimate in
    // between
    let held = match follow {
        true => None,
        false => {
            let top = match ANCHOR.take().and_then(|anchor| anchor.found(&keys)) {
                Some((at, offset, was)) if scroll != 0 => {
                    let anchored = start(at, &heights) + offset;
                    let moved = scroll as i128 - was as i128;
                    (anchored as i128 + moved).max(0) as usize
                }
                _ => scroll,
            };
            // note: not held to the bottom here, which is a count of estimates until the blocks
            // on screen are drawn: a block still being written is estimated from its length, and
            // short lines made that a tenth of what it was, so the view went to the top. The rows
            // are held to the bottom once they are counted, below
            let (mut at, mut begins) = (0, 0);
            while at + 1 < blocks.len() && begins + heights[at].0 <= top {
                begins += heights[at].0;
                at += 1;
            }
            Some((at, top - begins))
        }
    };

    // the blocks on screen first, measured from the end the view is held to: the bottom when it
    // follows, and the first block it shows when it does not
    let first = match follow {
        true => {
            let (mut at, mut rows) = (blocks.len(), 0);
            while at > 0 && rows < viewport {
                at -= 1;
                draw(at, &mut heights);
                rows += heights[at].0;
            }
            at
        }
        false => {
            let (at, offset) = held.unwrap_or_default();
            let start = start(at, &heights);
            let top = start + offset;
            let mut rows = start;
            let mut next = at;
            while next < blocks.len() && rows < top.saturating_add(viewport) {
                draw(next, &mut heights);
                rows += heights[next].0;
                next += 1;
            }
            at
        }
    };

    // and a few of the rest, nearest the view first, so that what is counted converges on what
    // is there within a few frames of a resize
    let mut budget = MEASURED_A_FRAME;
    let (mut above, mut below) = (first, first);
    while budget > 0 && (above > 0 || below < blocks.len()) {
        if above > 0 {
            above -= 1;
            if !heights[above].1 {
                draw(above, &mut heights);
                budget -= 1;
            }
        }
        if below < blocks.len() {
            if !heights[below].1 && budget > 0 {
                draw(below, &mut heights);
                budget -= 1;
            }
            below += 1;
        }
    }

    // what the last frame measured is what the scrolling keys are working against
    let total: usize = heights.iter().map(|(rows, _)| rows).sum();
    let bottom = total.saturating_sub(viewport);
    // held to its block, wherever what was counted above it now puts that block
    if let Some((at, offset)) = held.filter(|_| !blocks.is_empty()) {
        scroll = start(at, &heights) + offset.min(heights[at].0.saturating_sub(1));
    }
    if follow {
        scroll = bottom;
    }
    let at = scroll.min(bottom);

    // the rows on screen, from the block the top row is in
    let (mut block, mut begins) = (0, 0);
    while block + 1 < blocks.len() && begins + heights[block].0 <= at {
        begins += heights[block].0;
        block += 1;
    }
    ANCHOR.set(keys.get(block).map(|&key| Anchor {
        scroll,
        block,
        key,
        offset: at - begins,
    }));
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport);
    let mut skip = at - begins;
    while block < blocks.len() && lines.len() < viewport {
        let rows = draw(block, &mut heights);
        let room = viewport - lines.len();
        lines.extend(rows.iter().skip(skip).take(room).cloned());
        skip = skip.saturating_sub(rows.len());
        block += 1;
    }

    drop(blocks);
    DRAWN.set(Drawn {
        width,
        answers: fresh,
    });
    // what the last frame measured is what the scrolling keys are working against
    app.scroll = scroll;
    app.rendered = total;
    app.estimated = heights.iter().any(|(_, counted)| !counted);
    app.viewport = viewport;
    // note: a counted height wins over an estimate under the same key. Blocks with the same key
    // say the same words and are drawn the same way - every `cargo test` call a session makes -
    // and if the last of them were the one kept, an estimate would replace the height counted
    // for the others, and every one above the view would move it on the next frame
    let mut rows: HashMap<Key, (usize, bool)> = HashMap::with_capacity(keys.len());
    for (key, height) in keys.into_iter().zip(heights) {
        rows.entry(key)
            .and_modify(|kept| {
                if height.1 || !kept.1 {
                    *kept = height;
                }
            })
            .or_insert(height);
    }
    HEIGHTS.set(Heights { width, rows });

    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);

    Scrolled {
        position: at,
        total,
        area: inner,
    }
}

/// How many blocks off the screen a frame draws to measure them, beyond the ones it shows.
///
/// note: enough that a session of a few hundred blocks is counted exactly within a few frames of a
/// resize, and few enough that a frame costs about what a screenful does whatever the session.
const MEASURED_A_FRAME: usize = 32;

/// One block of the chat, before it is drawn: what it says, how it is drawn, and the item whose
/// held run it opens, if it opens one.
struct Block<'a> {
    look: Look,
    text: std::borrow::Cow<'a, str>,
    mark: Option<&'a std::sync::Arc<ContextItem>>,
}

impl Block<'_> {
    /// Its rows: the line saying why it is held where it opens a held run, what it says, and the
    /// blank line under it.
    fn draw(
        &self,
        width: usize,
        going: &Going,
        kept: &mut Kept,
        fresh: &mut Kept,
    ) -> Rc<[Line<'static>]> {
        let said = &self.text;
        let body = match self.look {
            // flattened rather than dimmed on top of itself, and markdown is not rendered here at
            // all: a highlighted block that kept its colours and lost only its brightness still
            // reads as live text at a glance, which the mark exists to prevent
            //
            // note: the rule is applied afterwards, not passed to `wrapped` as a prefix. A prefix
            // marks the first row and hangs the rest under it, which is right for `> ` but wrong
            // here: a long answer would get one marked row and the rest reading as ordinary
            // indented text
            Look::Held => kept_or_drawn(kept, fresh, Look::Held, said, || {
                wrapped(said, width.saturating_sub(2), "")
                    .into_iter()
                    .map(|text| {
                        Line::from(vec![
                            Span::styled("╎ ", faint()),
                            Span::styled(text, quiet()),
                        ])
                    })
                    .collect()
            }),
            // the model writes markdown; printing the asterisks would show punctuation instead of
            // emphasis. Nothing else here is markdown: a tool's output is whatever the tool said,
            // and rendering it would invent structure the tool did not put there
            Look::Answer => kept_or_drawn(kept, fresh, Look::Answer, said, || answer(said, width)),
            Look::Said(speaker) => said_by(speaker, said, width, kept, fresh),
        };

        let mut rows = Vec::with_capacity(body.len() + 2);
        if let Some(item) = self.mark {
            let (mark, _) = state_mark(item.state);
            rows.push(Line::styled(
                format!("{mark} [{}] {}", item.id.0, withheld_why(item, going)),
                quiet().italic(),
            ));
        }
        rows.extend(body.iter().cloned());
        rows.push(Line::default());

        rows.into()
    }

    /// How many rows it would take at this width, without drawing it or reading it: a row for
    /// each stretch of `width` bytes, and the rows around it.
    ///
    /// note: an estimate, for a block no frame has counted at any width: the first frame of a
    /// session, and what arrives off the screen. It decides where the scrollbar sits and how far
    /// the keys can move until the block is drawn. Not read line by line, because a resumed session
    /// would then read every word it holds before showing anything.
    fn estimate(&self, width: usize) -> usize {
        self.text.len().div_ceil(width.max(1)).max(1) + usize::from(self.mark.is_some()) + 1
    }

    /// What its height is kept under; see [`Key`].
    fn key(&self) -> Key {
        use std::hash::{Hash, Hasher};

        let text = self.text.as_bytes();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.look.hash(&mut hasher);
        self.mark.map(|item| item.id).hash(&mut hasher);
        match &self.text {
            // the words of an item or an entry, which stay where they are while it does
            std::borrow::Cow::Borrowed(words) => {
                (words.as_ptr() as usize).hash(&mut hasher);
                text.len().hash(&mut hasher);
                text[..text.len().min(16)].hash(&mut hasher);
                text[text.len().saturating_sub(16)..].hash(&mut hasher);
            }
            // words this frame put together, a call or a marker, which are short
            std::borrow::Cow::Owned(words) => words.hash(&mut hasher),
        }

        Key(hasher.finish())
    }
}

/// What anyone but the model said, under that speaker's prefix.
fn said_by(
    speaker: Speaker,
    said: &str,
    width: usize,
    kept: &mut Kept,
    fresh: &mut Kept,
) -> Rc<[Line<'static>]> {
    let (prefix, style) = match speaker {
        Speaker::User => ("> ", Style::default().fg(Color::White).bold()),
        Speaker::Reasoning => ("", quiet().italic()),
        Speaker::Call => ("⟩ ", Style::default().fg(Color::Cyan)),
        Speaker::Result => ("│ ", quiet()),
        Speaker::Note => ("· ", quiet()),
        Speaker::Error => ("! ", Style::default().fg(Color::Red)),
        Speaker::Model => unreachable!("drawn as an answer"),
    };

    // a shell result opens with what the command's exit said, and that line is the one
    // somebody watching a command run is waiting for. Drawn in the colour of what it says,
    // so it answers from across the room and the output under it stays quiet
    //
    // note: the rest is wrapped separately under a prefix of the same width, not the speaker's
    // own. `wrapped` puts the prefix on its first row and hangs the others, so applying the
    // rule twice would draw a `│` in the middle of one result
    let (head, rest) = match said.split_once('\n') {
        Some((head, rest)) => (head, rest),
        None => (said, ""),
    };
    kept_or_drawn(kept, fresh, Look::Said(speaker), said, || {
        let mut rows = Vec::new();
        match (speaker == Speaker::Result)
            .then(|| Exit::of(head))
            .flatten()
        {
            Some(exit) => {
                for text in wrapped(head, width, prefix) {
                    rows.push(Line::styled(text, exit_style(exit)));
                }
                if !rest.is_empty() {
                    for text in wrapped(rest, width, &" ".repeat(columns(prefix))) {
                        rows.push(Line::styled(text, style));
                    }
                }
            }
            None => {
                for text in wrapped(said, width, prefix) {
                    rows.push(Line::styled(text, style));
                }
            }
        }
        rows
    })
}

/// What a block's height is kept under: how it is drawn, the item whose held run it opens, and
/// where its words are, how long they are and how they start and end.
///
/// note: not the words themselves, which would mean reading every answer in the session on every
/// frame to find out how tall the ones off the screen are. Words an item holds stay where they are
/// for as long as the item does, and a changed item is a new one. Two that land at the same place
/// with the same length, start and end are counted as one height until one is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key(u64);

/// How many rows each block came to on the last frame, whether that was drawn and counted or
/// estimated, and the width it was at.
///
/// note: kept from one frame to the next so that a frame draws only what it shows and still knows
/// how tall the rest is. Every block is kept, the estimated ones too, so that a resize can scale
/// what the frame before it had rather than estimate the whole session again.
#[derive(Default)]
struct Heights {
    width: usize,
    rows: HashMap<Key, (usize, bool)>,
}

impl Heights {
    /// How many rows a block takes at `width`, and whether that was counted: as the last frame
    /// had it at the same width, and scaled from it, as an estimate, at another.
    ///
    /// note: scaled by the ratio of the widths, which is close for prose and a column or two off
    /// for code, and a resize moves a column at a time, so each frame of one scales what the frame
    /// before it counted. The blocks on screen are drawn and counted again either way.
    fn at(&self, width: usize, key: &Key) -> Option<(usize, bool)> {
        let &(rows, counted) = self.rows.get(key)?;
        match self.width == width {
            true => Some((rows, counted)),
            false => Some(((rows * self.width).div_ceil(width.max(1)).max(1), false)),
        }
    }
}

thread_local! {
    static HEIGHTS: Cell<Heights> = Cell::default();
    static ANCHOR: Cell<Option<Anchor>> = const { Cell::new(None) };
}

/// The block the top row of the last frame was in, how far into it, and the `scroll` that frame
/// left, so that the next frame shows the same line wherever what is counted above it puts it.
///
/// note: a row number alone would not do. Above the view, blocks are estimated until a frame draws
/// them, and every one that comes out taller or shorter than its estimate - after a resize, most
/// of them - would move what somebody is reading by as much, frame after frame. A key that moved
/// `scroll` since moves the view by as many rows from where it was held, so a page is half a screen
/// of what is on it rather than of a count that has changed under it.
#[derive(Clone, Copy)]
struct Anchor {
    scroll: usize,
    block: usize,
    key: Key,
    offset: usize,
}

impl Anchor {
    /// Its block in this frame's blocks, the offset into it, and the `scroll` it was taken at: at
    /// the same place if it still has the same key there, or the nearest block that has it, where
    /// something was added or taken away above it.
    fn found(self, keys: &[Key]) -> Option<(usize, usize, usize)> {
        if keys.get(self.block) == Some(&self.key) {
            return Some((self.block, self.offset, self.scroll));
        }
        (1..keys.len())
            .flat_map(|distance| {
                [
                    self.block.checked_sub(distance),
                    Some(self.block + distance),
                ]
            })
            .flatten()
            .find(|at| keys.get(*at) == Some(&self.key))
            .map(|at| (at, self.offset, self.scroll))
    }
}

/// How a line's rows were drawn from its words, so the same words drawn two ways are kept apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Look {
    /// The model's answer, as markdown.
    Answer,
    /// Words the model is no longer shown, under the rule that says so.
    Held,
    /// Anyone else's, under that speaker's prefix.
    Said(Speaker),
}

/// Rows drawn on one frame, by how they were drawn and from what words.
type Kept = HashMap<Look, HashMap<String, Rc<[Line<'static>]>>>;

/// The rows this frame draws for `said`: the ones the last frame drew from the same words the same
/// way, or `draw`'s.
fn kept_or_drawn(
    kept: &mut Kept,
    fresh: &mut Kept,
    look: Look,
    said: &str,
    draw: impl FnOnce() -> Vec<Line<'static>>,
) -> Rc<[Line<'static>]> {
    let fresh = fresh.entry(look).or_default();
    if let Some(rows) = fresh.get(said) {
        return Rc::clone(rows);
    }
    let (text, rows) = kept
        .get_mut(&look)
        .and_then(|kept| kept.remove_entry(said))
        .unwrap_or_else(|| (said.to_owned(), draw().into()));
    Rc::clone(fresh.entry(text).or_insert(rows))
}

/// The chat's lines as the last frame drew them, and the width it drew them at.
///
/// note: kept from one frame to the next because drawing them is expensive - an answer's markdown,
/// tables and highlighted code, and the wrapping of everything else - and on nearly every frame it
/// is unchanged. Rewrapping every turn's reasoning took most of a frame in a long session, on every
/// key and every tick. Held by the text rather than by the item, so a line still arriving is drawn
/// afresh as it grows, and dropped when a frame does not draw it.
#[derive(Default)]
struct Drawn {
    width: usize,
    answers: Kept,
}

impl Drawn {
    /// What is kept for a frame of this width: all of it, or nothing if the width changed.
    fn at(self, width: usize) -> Kept {
        match self.width == width {
            true => self.answers,
            false => HashMap::new(),
        }
    }
}

thread_local! {
    static DRAWN: Cell<Drawn> = Cell::default();
}

/// A model's answer, drawn: its markdown rendered, its tables laid out and its code highlighted.
fn answer(said: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let split = chunks(said);
    let last = split.len().saturating_sub(1);
    for (nth, chunk) in split.into_iter().enumerate() {
        // a fenced block gets a rule down its left rather than a background slab; a terminal cannot
        // show a background slab without knowing the theme. Its tokens are coloured the same way
        let prose = match chunk {
            // a fixed shape, laid out to the window rather than to its contents; see
            // `draw_table`
            Chunk::Table(block) => {
                if let Some(parsed) = table(block) {
                    separate(&mut lines);
                    lines.extend(draw_table(&parsed, width));
                    if nth < last {
                        separate(&mut lines);
                    }
                    continue;
                }
                block
            }
            Chunk::Code { language, body } => {
                // a blank line either side, where the markdown renderer would have put
                // one had it drawn the block
                separate(&mut lines);
                lines.extend(highlighted(language, body, width));
                if nth < last {
                    separate(&mut lines);
                }
                continue;
            }
            Chunk::Prose(prose) => prose,
        };

        // a line the model broke stays broken, as it does on the web page
        let prose = broken(prose);
        for line in tui_markdown::from_str_with_options(&prose, &markdown()).lines {
            // an *indented* block still arrives this way; the fenced ones never reach here
            match line.style == Markdown.code() {
                true => lines.extend(gutter(&line, width)),
                false => match rule(&line) {
                    // a horizontal rule, drawn rather than spelled `---`
                    true => lines.push(Line::styled("─".repeat(width), faint())),
                    false => lines.extend(refit(&line, width)),
                },
            }
        }
    }
    lines
}

/// The colour of a shell result's first line.
///
/// note: the three colours this program already uses for this question - `allow`/`ask`/`deny` on
/// the permissions tab and the budget bar as it fills - rather than a fourth vocabulary to learn.
/// Yellow means the command never reported: stopped, killed, or a status that could not be read,
/// not "a small failure", which a status line cannot tell. See [`Exit`].
fn exit_style(exit: Exit) -> Style {
    match exit {
        Exit::Ok => Style::default().fg(Color::Green),
        Exit::Stopped => Style::default().fg(Color::Yellow),
        Exit::Failed => Style::default().fg(Color::Red),
    }
}

// ---------------------------------------------------------------------------------- the context

/// The context as a table: every item, what kind of thing it is, what it costs, and either the
/// first line of what it says or, if it is not going into the next request, why not.
///
/// note: the whole window leaves room for the last column, the one that matters: a list of labels
/// and numbers tells you an item exists, and this tells you what the model is being told.
pub(super) fn draw_context(
    frame: &mut Frame,
    app: &mut App,
    going: &Going,
    area: Rect,
) -> Scrolled {
    let items = app.listed_by(going);
    app.drawn = Some(items.iter().map(|item| item.id).collect());
    let held_back = app.kernel.items().len() - items.len();
    if items.is_empty() {
        // the empties are not the same, and each has its own way out of it.
        //
        // note: a search is named first because it is what somebody just did and what `esc` undoes;
        // `f` is added when it is also holding rows back, since a pane naming one of two reasons is
        // a pane somebody clears and finds still empty.
        //
        // note: no count while a search is on. `held_back` is every item `listed` dropped; with a
        // search running it is both filters added together, so it would blame `f` for rows the
        // query hid
        let empty = match (app.search.is_some(), app.sending_only, held_back) {
            (true, false, _) => "nothing here matches; esc clears the search".to_owned(),
            (true, true, _) => {
                "nothing here matches; esc clears the search, and `f` is hiding rows as well"
                    .to_owned()
            }
            (false, _, 0) => "nothing here yet".to_owned(),
            (false, _, n) => {
                format!("nothing is being sent; {n} item(s) are hidden, and `f` lists them again")
            }
        };
        frame.render_widget(Paragraph::new(empty).style(quiet()), area);
        return Scrolled::default();
    }

    let [head, inner] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = inner.width as usize;

    // the columns give way from the right as the window narrows, so this works in eighty
    //
    // note: as wide as the widest label there is, rather than always the most it may be. Labels are
    // `user`, `shell`, `summary`, so a session whose longest is `read` would spend twenty columns
    // on nothing, taken from the column saying what an item holds
    let widest = items
        .iter()
        .map(|item| columns(&item.label))
        .max()
        .unwrap_or(0);
    let label = widest.clamp(5, 26).min(width / 3);
    let kind = if width >= 84 { 18 } else { 0 };
    let counted = 4 + 2 + label + 1 + kind + 8 + 7 + 2;
    let says = width.saturating_sub(counted);

    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "  {:<4}{:<label$} {:<kind$}{:>8}{:>7}  {}",
                "id",
                "label",
                match kind {
                    0 => "",
                    _ => "kind",
                },
                "sending",
                "held",
                match (says >= 8, app.sending_only) {
                    (false, _) => String::new(),
                    (true, false) => "what it says, or why it is not being sent".to_owned(),
                    // the count belongs on the header rather than in a note, because it is a
                    // property of what is on the screen and it stops being true when the toggle
                    // does
                    // note: counted here rather than taken from `held_back`, which is every item
                    // `listed` dropped - both filters added together. With a search running as
                    // well, that figure would blame `f` for rows the query hid; the empty pane
                    // above avoids the same miscount
                    (true, true) => format!(
                        "what it says · {} not being sent, hidden by f",
                        app.kernel
                            .items()
                            .iter()
                            .filter(|item| !going.sends_content(item))
                            .count()
                    ),
                }
            ),
            quiet(),
        )),
        head,
    );

    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let (mark, style) = state_mark(item.state);

            // an item that is not going says why, in the projector's own words; one that is
            // shows the first thing the model will read of it. An elided one is on the first
            // side of that: what the model reads is the note, so the note is what to show
            //
            // note: `going.sends_content` rather than the state's own, and `left_out` rather than
            // a reason built here out of the state and the note. An item the projector repaired
            // away is `Active` and is not in the request, so keyed on the state this row would
            // show the content it is not sending; and `Projection::skipped` already carries a
            // reason, one that has an answer for that case too
            let (tail, tail_style) = match going.sends_content(item) {
                false => (withheld_why(item, going), quiet().italic()),
                true => (
                    match item
                        .content
                        .to_text()
                        .lines()
                        .find(|line| !line.trim().is_empty())
                    {
                        Some(first) => first.trim().to_owned(),
                        // a turn that was nothing but tool calls has no text to show, and the
                        // calls are the whole of what it said
                        None => match &item.kind {
                            ContextKind::AssistantMessage { .. } => {
                                let names: Vec<_> =
                                    item.calls().map(|call| call.tool.as_str()).collect();
                                match names.is_empty() {
                                    true => String::new(),
                                    false => format!("asked for {}", names.join(", ")),
                                }
                            }
                            _ => String::new(),
                        },
                    },
                    quiet(),
                ),
            };

            let cost = going.costs.get(&item.id).copied().unwrap_or(0);

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>3} {mark} {} ", item.id.0, pad(&item.label, label)),
                    style,
                ),
                Span::styled(
                    match kind {
                        0 => String::new(),
                        _ => format!("{:<kind$}", clip(item.kind.name(), kind)),
                    },
                    quiet(),
                ),
                // what it costs in the next request, and what it is keeping out of it. For most
                // items the first is everything and the second is blank; the two that differ are
                // the ones somebody opens this pane to find note: a `+` where the counter would not
                // price part of what this item holds, because otherwise the two things `0` can mean
                // are the same cell. A picture would read as the cheapest row in the pane while
                // being the most expensive thing in the request
                Span::styled(
                    match item.uncounted {
                        0 => format!("{:>8}", fitted(cost, 8)),
                        _ => format!("{:>7}+", fitted(cost, 7)),
                    },
                    style,
                ),
                // note: what the request does not carry, rather than the whole of an item that is
                // not in it. The two are the same figure for a row that is wholly out and differ
                // for the ones that are partly in - an elided item, whose marker is going, and an
                // assistant turn whose thinking the endpoint will not take back. Counting only the
                // rows that are wholly out, a model that thought in tens of thousands of tokens
                // would have them on no row of the pane
                Span::styled(
                    format!(
                        "{:>7}  ",
                        match going.held_back(item) {
                            0 => String::new(),
                            held => fitted(held, 7),
                        }
                    ),
                    quiet(),
                ),
                Span::styled(clip(&tail, says), tail_style),
            ]))
        })
        .collect();

    app.selected = app.selected.min(items.len() - 1);
    app.list.select(Some(app.selected));
    frame.render_stateful_widget(
        List::new(rows).highlight_style(highlight(app)),
        inner,
        &mut app.list,
    );

    // read after the render, because that is what settles the offset. The list scrolls itself to
    // keep the selected row on screen; asking first would measure the frame before this one
    Scrolled {
        position: app.list.offset(),
        total: items.len(),
        area: inner,
    }
}

// ------------------------------------------------------------------------------ the permissions

/// One verdict, as the word for it and the colour that word is always in.
///
/// note: shared by the rows and by the line above them saying what the policy answers about
/// everything it has not been told about. Two copies would be two places for `ask` to stop being
/// yellow, on the one screen where the colour is the answer. The word itself is the shared one,
/// because `/permissions` prints these same rows for a caller with no tab and two spellings of
/// `allow` would be the tab and the page disagreeing.
fn verdict_word(verdict: Verdict) -> (&'static str, Style) {
    (
        crate::app::text::verdict_word(verdict),
        match verdict {
            Verdict::Allow => Style::default().fg(Color::Green),
            Verdict::Ask => Style::default().fg(Color::Yellow),
            Verdict::Deny => Style::default().fg(Color::Red),
        },
    )
}

/// The line a build that can rate commands draws when nothing is rating them.
///
/// note: the feature puts the advisor in the binary and `--rate-shell-commands` starts one, so a
/// build with the feature but not the flag draws a question with no rating. Without this nothing
/// would account for the absence, and the tab that answers "what is deciding" is where somebody
/// wondering will already be.
#[cfg(feature = "shell-advisor")]
fn unrated(app: &App) -> Vec<Line<'static>> {
    match app.advisor.is_none() {
        true => vec![Line::styled(
            "this build can rate the commands it asks about; --rate-shell-commands is what turns \
             that on",
            quiet(),
        )],
        false => Vec::new(),
    }
}

/// The same where the ratings are not in the build, which is nothing to say.
#[cfg(not(feature = "shell-advisor"))]
fn unrated(_: &App) -> Vec<Line<'static>> {
    Vec::new()
}

/// How a list marks its selected row: reversed while the keys are on it, underlined while they are
/// not.
///
/// note: underlined rather than a dark slab behind it. `Rgb(40, 40, 40)` is a shade of the
/// background this program does not know it has - it reads as barely-there on a dark theme and as a
/// black bar on a light one, the same mistake the code blocks avoid.
fn highlight(app: &App) -> Style {
    match app.focus == Focus::Body {
        true => Style::default().add_modifier(Modifier::REVERSED),
        false => Style::default().add_modifier(Modifier::UNDERLINED),
    }
}

/// What the policy will answer about each capability, and which tools that covers.
///
/// note: the permission prompt is the policy's only other appearance, and it shows up one call at a
/// time, at the worst moment to think about it. This is the same decisions, all of them, in advance
/// and changeable, which is what a replaceable `PermissionPolicy` buys: a policy is an object with
/// state, not a callback you can only learn about by triggering it.
pub(super) fn draw_permissions(frame: &mut Frame, app: &mut App, area: Rect) -> Scrolled {
    let rows = app.permissions();

    // which policy is in force and what it does with everything the list does not mention, above
    // the list, always
    //
    // note: without it the tab is every answer somebody has given and no account of what is
    // deciding in between, and "which policy is this, and what is it doing?" is the first question
    // a screen of permissions raises. Both halves come from the policy itself: `Careful::untold` is
    // the value `stance` falls back to, so this cannot come to describe a policy that has since
    // changed its mind note: at the margin rather than indented under it like a row. It is a
    // statement about the whole tab, and the two columns of indent the rows share put it in the
    // `capability` column, reading as the first and oddest entry in the table rather than as the
    // sentence the table is underneath. It also lines up with the empty-state prose, which starts
    // at the margin note: and, in a build that has the ratings in it, whether one is actually
    // running; see `unrated`
    let (untold, untold_style) = verdict_word(Careful::untold());
    let mut said = vec![Line::from(vec![
        Span::styled(format!("{} ", app.policy_name()), Style::default().bold()),
        Span::styled("· anything it has not been told about: ", quiet()),
        Span::styled(untold, untold_style),
    ])];
    said.extend(unrated(app));
    // note: not the actions listed under several rows, which need all of those rows. That is the
    // same sentence every time, so GUIDE.md says it rather than the tab; `/permissions` still
    // does. See `App::together`

    let [stated, area] = Layout::vertical([
        Constraint::Length(said.len() as u16 + 1),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(said), stated);

    if rows.is_empty() {
        // note: what is not here is a row per thing nobody has answered about yet. The policy asks
        // about everything by default, so listing the defaults is listing the absence of decisions,
        // and it would bury the one or two lines that say what this agent can do without stopping.
        // What arrives here is what somebody answered `a` to, or set here
        frame.render_widget(
            Paragraph::new(crate::app::text::NOTHING_DECIDED)
                .style(quiet())
                .wrap(ratatui::widgets::Wrap { trim: false }),
            area,
        );
        return Scrolled::default();
    }

    let [head, inner] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = inner.width as usize;
    let capability = 22.min(width / 3);
    let covers = width.saturating_sub(2 + capability + 1 + 10 + 2);

    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "  {:<capability$} {:<10}  {}",
                "capability or path",
                "answer",
                match covers >= 8 {
                    // note: not "the tools it covers". A row names the actions it lets run, and
                    // only a tool with one action by its name
                    true => "what it covers",
                    false => "",
                }
            ),
            quiet(),
        )),
        head,
    );

    let listed: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let (answer, style) = verdict_word(row.verdict);
            // a capability nothing declares is still worth a row, and should say so rather than
            // look like an oversight. `network` is not one of them: no tool declares it, and the
            // shell is judged against it anyway, on what the command says
            let tools = row.covers();

            ListItem::new(Line::from(vec![
                Span::raw(format!("  {} ", pad(&row.subject.to_string(), capability))),
                Span::styled(format!("{answer:<10}  "), style),
                Span::styled(
                    clip(&tools, covers),
                    match row.tools.is_empty() && row.sometimes.is_empty() {
                        true => quiet(),
                        false => Style::default(),
                    },
                ),
            ]))
        })
        .collect();

    app.chosen = app.chosen.min(rows.len() - 1);
    app.grants.select(Some(app.chosen));
    frame.render_stateful_widget(
        List::new(listed).highlight_style(highlight(app)),
        inner,
        &mut app.grants,
    );

    Scrolled {
        position: app.grants.offset(),
        total: rows.len(),
        area: inner,
    }
}

// ------------------------------------------------------------------------------------ the trace

/// Every event, newest at the bottom, in aligned columns.
///
/// note: the names are a column of their own rather than run together with what they say:
/// `permission.requested` is the longest of them, so twenty-two columns line every event up under
/// the last. Anything that still does not fit wraps under the column rather than being cut off,
/// because a log whose lines end in an ellipsis in the middle of the interesting part is useless.
///
/// note: the time of day, and not the time since the line above. Which lines follow each other is
/// not which wait anybody is asking about, so the gap was often a figure about nothing: a client
/// replaced, a question somebody sat reading. The times are there for anyone who wants the
/// difference. They drop out on a narrow window.
pub(super) fn draw_trace(frame: &mut Frame, app: &mut App, inner: Rect) -> Scrolled {
    const NAMES: usize = 22;
    /// How wide `HH:MM:SS ` is.
    const WHEN: usize = 9;

    let (width, height) = (inner.width as usize, inner.height as usize);
    let column = match width >= NAMES + 20 {
        true => NAMES,
        false => 0,
    };
    // the clock only where there is room for it: a narrow window spends its columns on what
    // happened rather than on when
    let when = width >= NAMES + 20 + WHEN;

    let mut lines: Vec<Line> = Vec::new();
    let mut day: Option<String> = None;
    // note: `traced` rather than the whole of `app.trace`, and collected before anything is
    // written back, so the borrow ends before `trace_scroll` is clamped below
    let events = app.traced();
    let found = events.len();
    for event in events {
        // faint, since it is looked up rather than read. A run whose zone could not be determined
        // is shown in UTC and marked, not as though it were local
        let read = when.then(|| read_off(event.wall));
        // note: the date is a rule across the pane, not a column, and is drawn only where it
        // changes. A session can outlast a day, so `00:15` against two different Tuesdays says
        // nothing, and repeating the date on every line would spend eleven columns on the same
        // answer almost every time. The row also carries the zone, which is stated rather than
        // implied by a colour.
        if let Some(read) = &read
            && day.replace(read.date.clone()).as_ref() != Some(&read.date)
        {
            let zone = match read.local {
                true => "",
                false => " UTC",
            };
            // across the whole pane, so it reads as a break between days rather than as a row
            // that has lost its columns
            let label = format!("──── {}{zone} ", read.date);
            let rule = "─".repeat(width.saturating_sub(columns(&label)));
            lines.push(Line::styled(format!("{label}{rule}"), faint()));
        }

        let stamp = match &read {
            None => Vec::new(),
            Some(read) => vec![Span::styled(
                format!("{} ", read.time),
                match read.local {
                    true => faint(),
                    false => faint().fg(Color::DarkGray),
                },
            )],
        };
        // note: these are the two colours the chat tab reads by: a tool call is cyan there, and a
        // permission is the colour of the question, so a trace row and the line it accounts for
        // agree without a second scheme to learn. The policy's events, a rule given to it and the
        // policy being set, are the same yellow, because the policy is what answers a question
        // before it is asked. `.failed` is matched first: a call that went wrong is a failure
        // before it is a call.
        //
        // note: the web page's trace view colours its rows by the same rules; see `traced` in
        // `web/browser.html`. Change both together.
        //
        // note: named colours are used rather than the logo's own `#59b8b2`, which cyan stands in
        // for. Every colour in this program is named, so the terminal's palette and background
        // decide how it lands. One hardcoded triple would be the only thing on screen that ignored
        // both, and would be hard to read on a light background.
        let colour = match () {
            _ if event.name.ends_with(".failed") => Color::Red,
            _ if event.name.starts_with("tool") => Color::Cyan,
            _ if event.name.starts_with("permission") => Color::Yellow,
            _ if event.name.starts_with("policy.") => Color::Yellow,
            _ if event.name.is_empty() => Color::Gray,
            _ => Color::White,
        };

        let named = Style::default().fg(colour);
        let said = quiet();
        let under = " ".repeat(match when {
            true => WHEN,
            false => 0,
        });
        let indent = " ".repeat(column.max(2));
        // note: the width is what is left after the columns in front of the detail, not the width
        // of the pane. Every line carries the clock and the name in front of it, so wrapping
        // against the whole width would run off the right edge and be clipped. This is the pane
        // where a detail wraps rather than being cut
        let room = width.saturating_sub(columns(&under) + indent.len());
        let mut detail = wrapped(&event.detail, room, "").into_iter();
        match (event.name.is_empty(), event.detail.is_empty()) {
            // a continuation: something the event before it had more to say about
            (true, _) => lines
                .extend(detail.map(|line| Line::styled(format!("{under}{indent}{line}"), said))),
            (false, true) => lines.push(Line::from(
                [stamp, vec![Span::styled(event.name.clone(), named)]].concat(),
            )),
            (false, false) => {
                // the first line of the detail sits beside the name, the rest under it
                let first = detail.next().unwrap_or_default();
                lines.push(Line::from(
                    [
                        stamp,
                        vec![
                            Span::styled(format!("{:<column$}", event.name), named),
                            Span::styled(first.trim_start().to_owned(), said),
                        ],
                    ]
                    .concat(),
                ));
                lines.extend(
                    detail.map(|line| Line::styled(format!("{under}{indent}{line}"), said)),
                );
            }
        }
    }

    if found == 0 {
        // the two empties are not the same, and only one of them has a way out. A session that has
        // not done anything yet opens on an empty trace, and telling whoever is looking at it to
        // press `esc` to clear a search they never started would make them believe the key is
        // broken
        let empty = match app.search.is_some() {
            true => "nothing here matches; esc clears the search",
            false => "nothing here yet",
        };
        lines.push(Line::styled(empty.to_owned(), quiet()));
    }

    // it is a log, so it is read from the bottom; `trace_scroll` counts upwards from there
    let bottom = lines.len().saturating_sub(height);
    app.trace_scroll = app.trace_scroll.min(bottom);
    let at = bottom - app.trace_scroll;
    let total = lines.len();

    frame.render_widget(Paragraph::new(in_view(lines, at, inner)), inner);

    Scrolled {
        position: at,
        total,
        area: inner,
    }
}

// ----------------------------------------------------------- what both lists say about an item

/// The mark and style used for an item in this state.
///
/// note: shared by the context tab and the chat, so one screen cannot call an item excluded while
/// the other draws it as still being read.
fn state_mark(state: ContextState) -> (&'static str, Style) {
    match state {
        ContextState::Active => ("·", Style::default()),
        ContextState::Pinned => ("▪", Style::default().fg(Color::Yellow)),
        ContextState::Excluded => ("-", quiet()),
        // in the request, but only as a marker: a mark of its own, because "going" and
        // "not going" is the wrong question about it and either answer would mislead
        ContextState::Elided => ("…", quiet()),
        _ => ("?", quiet()),
    }
}

/// Why this item's content is not going into the next request, in the projector's own words where
/// it has any.
///
/// note: the context tab's tail and the chat's mark use this same answer from one place, for the
/// reason [`Going`] keeps its two halves together: an item is either in the request or out of it
/// for a reason, and two screens working that out separately would come to disagree.
fn withheld_why(item: &ContextItem, going: &Going) -> String {
    match going.left_out.get(&item.id) {
        // the projector's own words about an item it did not carry
        Some(why) => why.clone(),
        // an elided item is carried as a marker, so the projector has nothing to say about it. The
        // model reads the note there, so the note belongs on the row
        None => match &item.note {
            Some(note) => format!("{}: {note}", item.state),
            None => item.state.to_string(),
        },
    }
}
