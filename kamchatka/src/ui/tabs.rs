//! The four tab bodies, and nothing else: the chrome around them is in `mod.rs` and the panel
//! that floats over them is in `overlay`.
//!
//! note: Four tabs, each of which gets the whole window, because each of them is a whole view.
//! Every terminal agent in the world has the first one. The second is the point of this program:
//! the *context*, item by item, with what each one costs, whether it is going into the next
//! request, and what the model will actually read of it - because in this runtime that is a list
//! of ordinary values rather than something the harness keeps to itself. The third is the event
//! stream the session log is made of, as it happens. The fourth is the permission policy, which
//! is otherwise only ever seen one call at a time, at the moment it is least convenient to think
//! about - every answer somebody has actually given, and a count of what is still a question. Not
//! a row per undecided thing: `ask` is what this policy does when nobody has told it anything, and
//! a screenful of it buries the one line that says what can happen without stopping.

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
    app::{App, Focus, Going, Speaker, text::waited_since},
    tools::{Careful, Exit},
    ui::{
        markdown::{Chunk, Markdown, chunks, highlighted, markdown, rule, separate},
        table::{draw_table, table},
        text::{clip, columns, fitted, gutter, pad, refit, wrapped},
    },
};

use super::{Scrolled, faint, in_view, quiet};

// ------------------------------------------------------------------------------ the conversation

pub(super) fn draw_chat(frame: &mut Frame, app: &mut App, going: &Going, inner: Rect) -> Scrolled {
    let width = inner.width as usize;
    let mut lines: Vec<Row> = Vec::new();
    // the item the last marked line belonged to, so that a turn and the calls it asked for say
    // once between them why they are not going rather than once each
    let mut marked: Option<ContextId> = None;
    // held by the loop, because the lines borrow their words out of these rather than copying
    // the whole conversation once a frame to show what it was already showing
    let items = app.kernel.items();
    // the turns holding a call that is waiting on an answer, which the projector leaves out of
    // the request and which is not the same thing as being left out; see `held` below
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
    // what the answers came to on the last frame, and what they come to on this one; see `Drawn`
    let mut kept = DRAWN.take().at(width);
    let mut fresh = HashMap::new();
    for said in app.conversation(&items, going) {
        let item = said.item;

        // what the model is no longer shown is drawn so that the eye can tell without reading
        // it. `going.sends_content` rather than the state, for the reason it exists: an item the
        // projector repaired away is `Active` and is not in the request. A line that is nobody's
        // item - a note, an error, something still arriving - is left exactly as it is
        // note: `is_elided` is handled by `App::conversation`, on its own terms, so what is left
        // here is the item a *projector* took out of a request it is otherwise in - a second result
        // for a call that already has one. That is not a decision anybody made and there is no
        // marker for it, so it keeps the rule down its left and the line saying why
        // note: and not one whose call is only waiting to be answered. The projector leaves a
        // turn out while a call of its has no result - it has to, a call with no answer is a
        // request most providers reject - so `sends_content` says no for the whole of the time
        // the permission prompt is open, and the line saying why would sit directly above the
        // call the person is being asked to authorise and describe it as a fault. It is not left
        // out; it is mid-flight, and answering the question that is already on screen is what
        // puts it in
        let held = item.filter(|item| {
            !going.sends_content(item) && !item.state.is_elided() && !deciding.contains(&item.id)
        });

        let speaker = said.speaker;
        // note: an elided item already reads as its marker here - the projector's own words, with
        // the brackets it put round them, which is exactly the text the model reads there. That
        // substitution is `App::conversation`'s, because what an elision *means* is not a rendering
        // decision, and a second client must not be handed the content of an item whose whole point
        // is that the model no longer has it. All that is left here is drawing it with the
        // speaker's own prefix and dimmed, so a `> ` still says whose turn it was and the dimming
        // says there is nothing of it left to read
        let said = said.text;

        if let Some(item) = held {
            if marked != Some(item.id) {
                let (mark, _) = state_mark(item.state);
                lines.push(Row::Drawn(Line::styled(
                    format!("{mark} [{}] {}", item.id.0, withheld_why(item, going)),
                    quiet().italic(),
                )));
                marked = Some(item.id);
            }

            // flattened rather than dimmed on top of itself, and markdown is not rendered here at
            // all: a highlighted block that kept its colours and lost only its brightness still
            // reads as live text at a glance, which is the one thing the mark exists to prevent
            //
            // note: the rule is put on afterwards rather than passed to `wrapped` as a prefix.
            // A prefix is a speaker's, so it belongs to the first row and the rest hang under it -
            // which is right for `> ` and wrong for this, where a long answer would come out with
            // one marked row and the rest reading as ordinary indented text
            for text in wrapped(&said, width.saturating_sub(2), "") {
                lines.push(Row::Drawn(Line::from(vec![
                    Span::styled("╎ ", faint()),
                    Span::styled(text, quiet()),
                ])));
            }
            lines.push(Row::Drawn(Line::default()));
            continue;
        }
        marked = None;

        // the model writes markdown, and a terminal that printed the asterisks would be showing
        // the punctuation instead of the emphasis. Nothing else here is markdown: a tool's output
        // is whatever the tool said, and running it through a renderer would be inventing
        // structure the tool did not put there
        if speaker == Speaker::Model {
            let answer = match fresh.get(said.as_ref()) {
                Some(answer) => answer,
                None => {
                    let (text, answer) = kept
                        .remove_entry(said.as_ref())
                        .unwrap_or_else(|| (said.to_string(), answer(&said, width).into()));
                    fresh.entry(text).or_insert(answer)
                }
            };
            lines.extend((0..answer.len()).map(|at| Row::Kept(Rc::clone(answer), at)));
            lines.push(Row::Drawn(Line::default()));
            continue;
        }

        let (prefix, style) = match speaker {
            Speaker::User => ("> ", Style::default().fg(Color::White).bold()),
            Speaker::Reasoning => ("", quiet().italic()),
            Speaker::Call => ("⟩ ", Style::default().fg(Color::Cyan)),
            Speaker::Result => ("│ ", quiet()),
            Speaker::Note => ("· ", quiet()),
            Speaker::Error => ("! ", Style::default().fg(Color::Red)),
            Speaker::Model => unreachable!("rendered above"),
        };

        // a shell result opens with what the command's exit said, and that line is the one
        // somebody watching a command run is waiting for. Drawn in the colour of what it says,
        // so it answers from across the room and the output under it stays quiet
        //
        // note: the rest is wrapped separately, under a prefix of the same width rather than the
        // speaker's own - `wrapped` puts the prefix on its first row and hangs the others, so
        // handing it the rule a second time would draw a `│` in the middle of one result
        let (head, rest) = match said.split_once('\n') {
            Some((head, rest)) => (head, rest),
            None => (said.as_ref(), ""),
        };
        match (speaker == Speaker::Result)
            .then(|| Exit::of(head))
            .flatten()
        {
            Some(exit) => {
                for text in wrapped(head, width, prefix) {
                    lines.push(Row::Drawn(Line::styled(text, exit_style(exit))));
                }
                if !rest.is_empty() {
                    for text in wrapped(rest, width, &" ".repeat(columns(prefix))) {
                        lines.push(Row::Drawn(Line::styled(text, style)));
                    }
                }
            }
            None => {
                for text in wrapped(&said, width, prefix) {
                    lines.push(Row::Drawn(Line::styled(text, style)));
                }
            }
        }
        lines.push(Row::Drawn(Line::default()));
    }

    DRAWN.set(Drawn {
        width,
        answers: fresh,
    });

    // what the last frame measured is what the scrolling keys are working against
    app.rendered = lines.len();
    app.viewport = inner.height as usize;
    let bottom = lines.len().saturating_sub(inner.height as usize);
    if app.follow {
        app.scroll = bottom;
    }
    let at = app.scroll.min(bottom);
    let total = lines.len();

    frame.render_widget(
        Paragraph::new(Text::from_iter(in_view(lines, at, inner))),
        inner,
    );

    Scrolled {
        position: at,
        total,
        area: inner,
    }
}

/// A row of the chat: one drawn for this frame, or one of an answer's as [`Drawn`] keeps them.
///
/// note: an answer's rows are pointed at rather than copied, and only the ones on screen become
/// lines. Copying every answer's rows into every frame, to show one screenful of them, cost more
/// than drawing the rest of the chat did.
enum Row {
    Drawn(Line<'static>),
    Kept(Rc<[Line<'static>]>, usize),
}

impl From<Row> for Line<'static> {
    fn from(row: Row) -> Self {
        match row {
            Row::Drawn(line) => line,
            Row::Kept(answer, at) => answer[at].clone(),
        }
    }
}

/// The model's answers as the last frame drew them, and the width it drew them at.
///
/// note: kept from one frame to the next because an answer - its markdown, its tables and above
/// all its highlighted code - is the dearest thing on the chat to draw, and on nearly every frame
/// it is the same as it was. Held by the text rather than by the item, so an answer still arriving
/// is drawn afresh as it grows, and dropped when a frame does not draw it.
#[derive(Default)]
struct Drawn {
    width: usize,
    answers: HashMap<String, Rc<[Line<'static>]>>,
}

impl Drawn {
    /// What is kept for a frame of this width: all of it, or nothing if the width changed.
    fn at(self, width: usize) -> HashMap<String, Rc<[Line<'static>]>> {
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
        // a fenced block gets a rule down its left rather than a slab of background,
        // which is the one thing a terminal cannot do without knowing the theme - and its
        // tokens in colours chosen the same way
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

        for line in tui_markdown::from_str_with_options(prose, &markdown()).lines {
            // an *indented* block still arrives this way; the fenced ones never reach here
            match line.style == Markdown.code() {
                true => lines.extend(gutter(&line, width).into_iter().map(owned)),
                false => match rule(&line) {
                    // a horizontal rule, drawn rather than spelled `---`
                    true => lines.push(Line::styled("─".repeat(width), faint())),
                    false => lines.extend(refit(&line, width).into_iter().map(owned)),
                },
            }
        }
    }
    lines
}

/// A line that owns its words, to be kept past the text it was drawn from.
fn owned(line: Line<'_>) -> Line<'static> {
    Line {
        spans: line
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content.into_owned(), span.style))
            .collect(),
        style: line.style,
        alignment: line.alignment,
    }
}

/// The colour of a shell result's first line, which is what somebody reads one for.
///
/// note: the three this program already uses for exactly this question - `allow`/`ask`/`deny` on
/// the permissions tab, and the budget bar as it fills - rather than a fourth vocabulary for a
/// reader to learn. Yellow is the honest middle here: not "a small failure", which nothing in a
/// status line can tell, but *the command never reported* - stopped, killed, or a status that
/// could not be read. See [`Exit`].
fn exit_style(exit: Exit) -> Style {
    match exit {
        Exit::Ok => Style::default().fg(Color::Green),
        Exit::Stopped => Style::default().fg(Color::Yellow),
        Exit::Failed => Style::default().fg(Color::Red),
    }
}

// ---------------------------------------------------------------------------------- the context

/// The context as a table: every item, what kind of thing it is, what it costs, and either the
/// first line of what it says or - if it is not going into the next request - why not.
///
/// note: With the whole window to work in there is room for the last column, and it is the one
/// that matters: a list of labels and numbers tells you an item exists, and this tells you what
/// the model is actually being told.
pub(super) fn draw_context(
    frame: &mut Frame,
    app: &mut App,
    going: &Going,
    area: Rect,
) -> Scrolled {
    let items = app.listed();
    app.drawn = Some(items.iter().map(|item| item.id).collect());
    let held_back = app.kernel.items().len() - items.len();
    if items.is_empty() {
        // the empties are not the same, and each has its own way out of it.
        //
        // note: a search is named first, because it is the thing somebody just did and the thing
        // `esc` undoes - and `f` is named beside it when that is holding rows back too, since a
        // pane naming one of two reasons is a pane somebody clears and finds still empty.
        //
        // note: and no count while a search is on. `held_back` is every item `listed` dropped,
        // and with a search running that is the two filters added together - so the figure would
        // be blaming `f` for rows the query is what hid, which is a number worse than none
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
    // note: as wide as the widest label there is, rather than always the most it may be. Labels
    // are `user`, `shell`, `summary` - a session whose longest is `read` would spend twenty columns
    // on nothing, and the column those twenty belong to is the one saying what an item holds
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
                    // property of what is on the screen and it stops being true when the toggle does
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
                // exactly the ones somebody opens this pane to find
                // note: a `+` where the counter would not price part of what this item holds,
                // because otherwise the two things `0` can mean are the same cell. A picture
                // reads as the cheapest row in the pane and is the most expensive thing in the
                // request, which is the wrong conclusion to invite from a column of numbers
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
    let highlight = match app.focus == Focus::Body {
        true => Style::default().add_modifier(Modifier::REVERSED),
        // note: underlined rather than a dark slab behind it. `Rgb(40, 40, 40)` is a shade of the
        // background this program does not know it has - it reads as barely-there on a dark theme
        // and as a black bar on a light one, which is the same mistake the code blocks avoid
        false => Style::default().add_modifier(Modifier::UNDERLINED),
    };

    frame.render_stateful_widget(
        List::new(rows).highlight_style(highlight),
        inner,
        &mut app.list,
    );

    // read after the render, because that is what settles the offset: the list scrolls itself to
    // keep the selected row on screen, and asking first would measure the frame before this one
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
/// everything it has not been told about. Two of them is two places for `ask` to stop being
/// yellow, on the one screen where the colour is the answer.
fn verdict_word(verdict: Verdict) -> (&'static str, Style) {
    match verdict {
        Verdict::Allow => ("allow", Style::default().fg(Color::Green)),
        Verdict::Ask => ("ask", Style::default().fg(Color::Yellow)),
        Verdict::Deny => ("deny", Style::default().fg(Color::Red)),
    }
}

/// The line a build that can rate commands draws when nothing is rating them.
///
/// note: the feature puts the advisor in the binary and `--advise` is what starts one, so a build
/// with the first and not the second draws a question with no rating, and without this nothing
/// anywhere would account for the absence. That is the failure this program is least for, and the
/// tab that answers "what is deciding" is where somebody wondering will already be.
#[cfg(feature = "shell-advisor")]
fn unrated(app: &App) -> Vec<Line<'static>> {
    match app.advisor.is_none() {
        true => vec![Line::styled(
            "this build can rate the commands it asks about; --advise is what turns that on",
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

/// What the policy will answer about each capability, and which tools that covers.
///
/// note: The permission prompt is the policy's only other appearance, and it shows up one call at
/// a time, at the worst possible moment to think about it. This is the same decisions, all of
/// them, in advance, and changeable - which is also the plainest thing to point at when somebody
/// asks what a replaceable `PermissionPolicy` buys you: a policy is an object with state, not a
/// callback you can only learn about by triggering it.
pub(super) fn draw_permissions(frame: &mut Frame, app: &mut App, area: Rect) -> Scrolled {
    let rows = app.permissions();

    // which policy is in force and what it does with everything the list does not mention, above
    // the list, always
    //
    // note: without it the tab is every answer somebody has given and no account of what is
    // deciding in between - and "which policy is this, and what is it doing?" is the one question a
    // screen of permissions raises. Reading `/seams` for the name and the source for the behaviour
    // is not a screen. Both halves come from the policy itself: `Careful::untold` is the value
    // `stance` falls back to, so this cannot come to describe a policy that has since changed its
    // mind
    // note: at the margin rather than indented under it like a row. It is a statement about the
    // whole tab, and the two columns of indent the rows share put it in the `capability` column -
    // reading as the first and oddest entry in the table rather than as the sentence the table is
    // underneath. It also lines up with the empty-state prose, which starts at the margin
    // note: and, in a build that has the ratings in it, whether one is actually running; see
    // `unrated`
    let (untold, untold_style) = verdict_word(Careful::untold());
    let mut said = vec![Line::from(vec![
        Span::styled(format!("{} ", app.policy_name()), Style::default().bold()),
        Span::styled("· anything it has not been told about: ", quiet()),
        Span::styled(untold, untold_style),
    ])];
    said.extend(unrated(app));

    let [stated, area] = Layout::vertical([
        Constraint::Length(said.len() as u16 + 1),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(said), stated);

    if rows.is_empty() {
        // note: what is *not* here is a row per thing nobody has answered about yet. The policy
        // asks about everything by default, so listing the defaults is listing the absence of
        // decisions - and it would bury the one or two lines that say what this agent can do
        // without stopping. What arrives here is what somebody answered `a` to, or set here
        frame.render_widget(
            Paragraph::new(
                "nothing has been decided yet, which is why this list is empty rather than \
                 permissive.\n\nAnswer a question with `a` and everything it was judged by arrives \
                 here, where it can be changed; `y` and `n` answer that one call and record \
                 nothing. A fresh policy also holds a rule for each of a handful \
                 of paths that are credentials by convention, and those are questions too, so \
                 they are not rows either - the line along the bottom is what counts them. They \
                 begin to earn their keep the moment a capability is answered `always`: the \
                 capability opens, the rules stay where they are, and the strictest thing \
                 consulted wins - so a rule can only ever tighten what a capability allows. Those \
                 rules bind every `fs` operation that is handed a path, and deliberately not \
                 `shell`: a command \
                 names its files inside a string, so what holds a command to a boundary is the \
                 sandbox rather than a rule here.",
            )
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
                    // note: not "the tools it covers". A capability, a server and a path are
                    // each about a set of tools; a whole domain is about a set of capabilities,
                    // and naming its tools instead would tell somebody who wrote `--allow log`
                    // that it covered `log`
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
            // a capability nothing declares is still worth a row, and it should say so rather
            // than look like an oversight - but `network` is not one of them, however it looks: no
            // tool declares it and the shell is judged against it anyway, on what the command says
            let tools = match (row.tools.is_empty(), row.sometimes.is_empty()) {
                (true, true) => "nothing registered needs it".to_owned(),
                (true, false) => format!("{}, {}", row.sometimes.join(", "), row.when),
                (false, true) => row.tools.join(", "),
                (false, false) => format!(
                    "{}; {}, {}",
                    row.tools.join(", "),
                    row.sometimes.join(", "),
                    row.when
                ),
            };

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
    let highlight = match app.focus == Focus::Body {
        true => Style::default().add_modifier(Modifier::REVERSED),
        // underlined, for the reason the context tab's selected row is
        false => Style::default().add_modifier(Modifier::UNDERLINED),
    };

    frame.render_stateful_widget(
        List::new(listed).highlight_style(highlight),
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
/// note: The names are a column of their own rather than run together with what they say, which
/// is what a whole window buys: `permission.requested` is the longest of them, so twenty-two
/// columns line every event up under the last. Anything that still does not fit wraps under the
/// column rather than being cut off - a log whose lines end in an ellipsis in the middle of the
/// interesting part is not a log.
///
/// note: the time of day *and* the gap to the line above. The gap is the one painted yellow,
/// because the question somebody usually brings to a log is which step was slow, and a column of
/// timestamps makes them do the subtraction. But a gap answers no question that starts "when":
/// matching the pane against a server log, a provider's dashboard, a ticket, or a memory of what
/// happened just before lunch all need an absolute time, and none of them can be reached by adding
/// up a column of deltas. Both drop out on a narrow window, the time first.
pub(super) fn draw_trace(frame: &mut Frame, app: &mut App, inner: Rect) -> Scrolled {
    const NAMES: usize = 22;
    /// How wide the gap column is, including the space after it.
    const GAP: usize = 8;
    /// How wide `HH:MM:SS ` is.
    const WHEN: usize = 9;

    let (width, height) = (inner.width as usize, inner.height as usize);
    let column = match width >= NAMES + 20 {
        true => NAMES,
        false => 0,
    };
    // the clock only where there is room for it: a narrow window spends its columns on what
    // happened rather than on when
    let clock = width >= NAMES + 20 + GAP;
    let when = width >= NAMES + 20 + GAP + WHEN;

    let mut lines: Vec<Line> = Vec::new();
    let mut before: Option<std::time::Instant> = None;
    let mut day: Option<String> = None;
    // note: `traced` rather than the whole of `app.trace`, and collected before anything is
    // written back, so the borrow ends before `trace_scroll` is clamped below
    let events = app.traced();
    let found = events.len();
    for event in events {
        // the gap to the line above, blank under a tenth of a second, so the few that took real
        // time are the only ones with anything in the column at all
        // note: `replace` runs either way, so the line after a person's still measures from this
        // one. What is skipped is drawing the figure, not keeping the clock
        let gap = match (clock, before.replace(event.at)) {
            // note: and not where the gap is somebody's. `after_a_person` marks the line that ends
            // a wait for a person - an answered question, a message finally typed - and however
            // long that took it is not a step this program spent any time on. It is also usually
            // the biggest number in the column, so the one figure nobody should act on would be
            // the one the eye goes to first
            (true, Some(previous)) if !event.after_a_person => {
                waited_since(event.at.saturating_duration_since(previous))
            }
            _ => None,
        };
        let gap_span = match (clock, &gap) {
            (false, _) => Vec::new(),
            (true, Some(said)) => vec![Span::styled(
                format!("{said:>7} "),
                match said.ends_with('s') && !said.ends_with("ms") {
                    true => Style::default().fg(Color::Yellow),
                    false => faint(),
                },
            )],
            (true, None) => vec![Span::raw(" ".repeat(GAP))],
        };
        // dimmer than the gap on purpose: the gap is the figure being looked for, the time is the
        // one being looked *up*. A run whose zone could not be determined is shown in UTC and
        // marked, rather than shown as though it were local
        let read = when.then(|| read_off(event.wall)).flatten();
        // note: the date is a rule across the pane rather than a column, and is drawn only where it
        // changes. A session can outlast a day - that is the shape of run this clock is for - and
        // `00:15` against two different Tuesdays says nothing; repeating the date on every line to
        // disambiguate two of them would spend eleven columns on what is the same answer almost
        // every time. It carries the zone as well, which is the one place that is worth saying out
        // loud rather than implying with a colour.
        if let Some(read) = &read
            && day.replace(read.date.clone()).as_ref() != Some(&read.date)
        {
            let zone = match read.local {
                true => "",
                false => " UTC",
            };
            lines.push(Line::styled(format!("── {}{zone}", read.date), faint()));
        }

        let when_span = match &read {
            None => Vec::new(),
            Some(read) => vec![Span::styled(
                format!("{} ", read.time),
                match read.local {
                    true => faint(),
                    false => faint().fg(Color::DarkGray),
                },
            )],
        };
        let stamp = [when_span, gap_span].concat();
        // note: the same two colours the chat tab reads by - a tool call is cyan there and a
        // permission is what the question is drawn in - so a row on the trace and the line it
        // accounts for agree without anybody having to learn a second scheme. `.failed` is
        // matched first on purpose: a call that went wrong is a failure before it is a call.
        //
        // note: named colours rather than the logo's own `#59b8b2`, which is what cyan is
        // standing in for. Every colour in this program is a named one, so a terminal's palette
        // and its background decide how it lands; one hardcoded triple would be the only thing
        // on the screen that ignored both, and it would be the wrong side of legible on a light
        // background.
        let colour = match () {
            _ if event.name.ends_with(".failed") => Color::Red,
            _ if event.name.starts_with("tool") => Color::Cyan,
            _ if event.name.starts_with("permission") => Color::Yellow,
            _ if event.name.is_empty() => Color::Gray,
            _ => Color::White,
        };

        let named = Style::default().fg(colour);
        let said = quiet();
        let under = " ".repeat(match (when, clock) {
            (true, _) => WHEN + GAP,
            (false, true) => GAP,
            (false, false) => 0,
        });
        let indent = " ".repeat(column.max(2));
        // note: what is left after the columns in front of it, rather than the width of the pane.
        // Every line carries the clock and the name in front of it, so a detail wrapped against
        // the whole width would run off the right edge and be clipped - in the one pane whose
        // promise is that a detail wraps rather than being cut
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
        // the two empties are not the same, and only one of them has a way out. A session that
        // has not done anything yet opens on an empty trace, and telling whoever is looking at it
        // to press `esc` to clear a search they never started is an instruction to undo something
        // that is not there - which is how somebody comes to believe the key is broken
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

/// The mark and style an item in this state is drawn with.
///
/// note: shared by the context tab and the chat, so that one screen cannot call an item
/// superseded while the other draws it as though it were still being read.
fn state_mark(state: ContextState) -> (&'static str, Style) {
    match state {
        ContextState::Active => ("·", Style::default()),
        ContextState::Pinned => ("▪", Style::default().fg(Color::Yellow)),
        ContextState::Excluded => ("-", quiet()),
        // in the request, but only as a marker: a mark of its own, because "going" and
        // "not going" is the wrong question about it and either answer would mislead
        ContextState::Elided => ("…", quiet()),
        ContextState::Archived => ("▫", quiet()),
        ContextState::Superseded => ("~", quiet()),
        _ => ("?", quiet()),
    }
}

/// Why this item's content is not going into the next request, in the projector's own words where
/// it has any.
///
/// note: the same answer for the context tab's tail and the chat's mark, out of one place, for
/// the reason [`Going`] keeps its two halves together: an item is either in the request or out of
/// it for a reason, and two screens working that out separately is how they come to disagree.
fn withheld_why(item: &ContextItem, going: &Going) -> String {
    match going.left_out.get(&item.id) {
        // the projector's own words about an item it did not carry
        Some(why) => why.clone(),
        // an elided one it *did* carry, as a marker, so it has nothing to say about it - and what
        // the model reads there is the note, so the note is what belongs on the row
        None => match &item.note {
            Some(note) => format!("{}: {note}", item.state),
            None => item.state.to_string(),
        },
    }
}
