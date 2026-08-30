//! Drawing. Nothing here decides anything - it reads [`App`] and the kernel and puts what it
//! finds on the screen.
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

use nachalnik::{ContextKind, ContextState, Kernel, State, Verdict};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};

use tui_markdown::StyleSheet as _;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::app::{App, Focus, Overlay, Speaker, Tab};

/// What the keys do, shown by F1.
///
/// note: `pub` so that a test can read it rather than trying to count things on a screen it does
/// not all fit on. That is not a hypothetical convenience: `/seams` was listed in here twice, and
/// the test that draws this panel had no way to notice.
///
/// note: no `\` continuation after the opening quote: it would eat the newline *and* the two
/// spaces indenting the first heading, leaving `THE TABS` flush against the border while every
/// other heading sat under it.
pub const HELP: &str = "  THE TABS
    ctrl+t              the next one
    alt+1 / 2 / 3 / 4   chat / context / trace / permissions
    tab                 move between the prompt and the tab, on the last three

  ANYWHERE
    ctrl+p              the exact request that would be sent next
    f1                  this; also ? on a tab that has the focus
    esc                 close this, or stop what is running
    ctrl+c              stop what is running; again to leave
    ctrl+d              leave

  THE PROMPT, which is under every tab
    enter               send
    alt+enter           a new line
    pgup / pgdn         scroll the conversation
    (a message sent while a turn is running waits for the end of it, and
     then gets a turn of its own)

  THE CONTEXT TAB, when it has the focus
    up / down, j / k    pick an item
    pgup / pgdn         a screenful at a time
    g / G               the first item / the last
    23G                 the item numbered 23
    space               cycle how much of it the model gets: all of it, then
                        a … marker where it was, then nothing, then all of it
    p                   pin it, so that compaction cannot touch it
                        (on a ▫ archived row, either of those sends the whole
                         of an output the model was shown a shortened copy of)
    e                   change what it says; the old one stays, marked ~
    enter               read the whole of what it says
    u / U               undo / redo the last change to the context

  THE TRACE TAB, when it has the focus
    up / down, j / k    read back through it
    pgup / pgdn         a screenful at a time
    g / G               the oldest it still holds / the newest

  THE PERMISSIONS TAB, when it has the focus
    up / down, j / k    pick a capability, or one of the path rules under them
    g / G               the first / the last
    space               cycle it: ask, then allow, then deny
    a / n / r           allow it / never allow it / ask about it again
                        (backspace does what r does)
    (the line along the bottom says what a shell command can reach, and how
     many subjects are not listed here because nobody has answered about them)

  A TOOL IS WAITING TO RUN
    y / n               once / no
    esc                 no
    a                   always, for everything the question names - and for
                        the calls already waiting behind it
    i                   the exact JSON, and the tool's own definition
    d                   drop every call it is waiting on, and tell it why
    (a question that arrives while you are typing waits for you to stop:
     until then your keys go to the prompt, where you aimed them)

  COMMANDS
    /help               this; also /?
    /step [MESSAGE]     one transition of the state machine, and stop
    /continue           run the rest of the turn
    /request            the request that would go next
    /payload            the provider's own rendering of it, byte for byte
    /raw                the provider's own last answer
    /prune SELECTOR     take items out; with no selector, the whole language
    /keep SELECTOR      pin them
    /restore SELECTOR   put them back
    /budget             the estimate, what the last request really cost, and the
                        correction the counter has worked out from the difference
    /seams              what is plugged into each of the runtime's six parts
    /tools              what the model is offered
    /tools drop ID      stop offering one of them, from now on
    /policy             open the permissions tab; also /permissions
    /model [ID]         show or switch the model, and say where it is
    /models [FILTER]    what this endpoint serves, which is what /model takes
    /provider [URL [ID]] show or switch the address the requests go to, and
                        the model with it; also /endpoint. The key is the one
                        this started with
    /params [KEY JSON]  show or set a model parameter
    /save [PATH]        the session log, and a snapshot to resume from
    /quit               also /exit, /q";

/// The selector language, shown by `/prune` with nothing to prune.
///
/// note: Kept beside the help rather than derived from the crate, because `Selector` is a parser
/// and a parser cannot tell you what it would have accepted. It is the same list as the type's
/// own documentation, and the tests check that a few of these really do parse.
pub(crate) const SELECTORS: &str = "  17                      the item with that number
  all                     every item, whatever state it is in

  tool_results            every item from that source; also: files, diagnostics,
                          selections, memories, instructions, system, user, model,
                          compaction
  all:tool_results        the same, spelled out
  source:helix            every item from a source with that name

  kind:assistant_message  every item of that kind
  state:excluded          every item in that state

  file:src/parser.rs      the file with that path
  tool:grep               every result the `grep` tool produced
  tool:grep:latest        the most recent one; also: tool:grep:first
  tool_result:1842        the tool result with that call id
  label:cargo test        every item with exactly that label
  src/parser.rs           anything else is taken as a label

  What it matched is reported before anything is sent, and every change is one
  `u` away from being undone.";

/// How many rows a set of lines occupies once soft-wrapped to `width`.
///
/// note: `TextArea` wraps when it draws and does not report how many rows that came to, so this
/// counts them the same way `WrapMode::WordOrGlyph` breaks them: fill up to the width, go back to
/// the last space if there is one, and split a word that would not fit on a line of its own. It
/// has to agree with the widget about the *number* of rows rather than about where the breaks
/// fall, and the screen test is what holds the two together - if they ever disagree, the box is
/// the wrong size and text goes missing, which is the thing worth failing a build over.
pub fn wrapped_rows(lines: &[String], width: usize) -> usize {
    if width == 0 {
        return lines.len().max(1);
    }

    lines
        .iter()
        .map(|line| rows_for(line, width))
        .sum::<usize>()
        .max(1)
}

/// The rows one logical line takes: fill with whole word-bound chunks, start a new row when the
/// next one will not fit, and split a chunk that will not fit on a row of its own.
fn rows_for(line: &str, width: usize) -> usize {
    let mut closed = 0;
    let mut filled = 0;
    let mut started = false;

    for (_, text) in line.split_word_bound_indices() {
        let mut chunk = text;
        while !chunk.is_empty() {
            let chunk_width = Span::raw(chunk).width();
            if filled + chunk_width <= width {
                filled += chunk_width;
                started = true;
                break;
            }
            // there is something on this row already, so end it and try the chunk on the next
            if started {
                closed += 1;
                filled = 0;
                started = false;
                continue;
            }
            // an empty row and it still does not fit: this is one long word, and it is cut
            let take = prefix_within(chunk, width);
            closed += 1;
            chunk = &chunk[take..];
            filled = 0;
        }
    }

    // the row still being filled counts, and a line with nothing on it is a row of its own
    closed + usize::from(started || closed == 0)
}

/// The longest prefix of `text` that fits in `width` columns, and never nothing - a grapheme
/// wider than the whole box would otherwise loop forever.
fn prefix_within(text: &str, width: usize) -> usize {
    let mut end = 0;
    let mut filled = 0;

    for (offset, grapheme) in text.grapheme_indices(true) {
        let next = filled + Span::raw(grapheme).width();
        if end != 0 && next > width {
            break;
        }
        end = offset + grapheme.len();
        filled = next;
        if filled >= width {
            break;
        }
    }

    end.max(text.chars().next().map_or(1, char::len_utf8))
        .min(text.len())
}

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // editing an item wants more room than composing a message does, and the tab underneath is
    // the one thing that can afford to give it up: the item being edited is on it
    let most = match app.editing.is_some() {
        true => 16,
        false => 8,
    };
    // the *wrapped* height, not the number of lines somebody typed: with soft wrapping on, one
    // long line is several rows, and a box sized to the line count would show the last of them
    // and hide the rest
    let inner = frame.area().width.saturating_sub(2) as usize;
    let input_height = (wrapped_rows(app.input.lines(), inner) as u16).clamp(1, most) + 2;
    let [body, input, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_body(frame, app, body);
    draw_input(frame, app, input);
    draw_status(frame, app, status);

    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, overlay, app);
    }
}

// -------------------------------------------------------------------------------------- the tabs

/// Text that is secondary but still meant to be read.
///
/// note: `Gray`, not `DarkGray`. Nearly everything on these screens that is not the answer itself
/// used to be `DarkGray` - why an item is not being sent, what an event says, the whole of the
/// trace - and `DarkGray` is the terminal's bright *black*: on a good half of the themes people
/// actually use it sits a shade off the background. That is the wrong thing to do to the column
/// this program exists for. Anything a person is meant to read is this.
fn quiet() -> Style {
    Style::default().fg(Color::Gray)
}

/// Chrome that is supposed to stay out of the way: borders, rules, the rule down a code block.
///
/// note: this one is `DarkGray`, and it is the only thing that should be. It is drawing lines,
/// not words.
fn faint() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The window: a strip of tabs, and whichever one is open filling everything under it.
fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Body;

    let mut strip = Vec::new();
    for tab in Tab::ALL {
        if !strip.is_empty() {
            strip.push(Span::styled("│", faint()));
        }
        // the open tab looks open whatever the keys are doing; which half of the window they are
        // talking to is the border's job, and having both say it left `chat` looking shut,
        // because the prompt always has the focus there
        strip.push(Span::styled(
            format!(" {} ", tab.name()),
            match tab == app.tab {
                true => Style::default().fg(Color::Yellow).bold(),
                false => quiet(),
            },
        ));
    }

    let edge = match focused {
        true => Style::default().fg(Color::Yellow),
        false => faint(),
    };
    let block = Block::bordered()
        .title(Line::from(strip))
        .title_bottom(Line::styled(footer(app), quiet()).right_aligned())
        .border_style(edge);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let scrolled = match app.tab {
        Tab::Chat => draw_chat(frame, app, inner),
        Tab::Context => draw_context(frame, app, inner),
        Tab::Trace => draw_trace(frame, app, inner),
        Tab::Permissions => draw_permissions(frame, app, inner),
    };
    scrollbar(frame, area, edge, scrolled);
}

/// How far through its content a tab is, and the rows it drew that content in.
#[derive(Clone, Copy, Default)]
struct Scrolled {
    /// The first row on screen, counted from the top of the content.
    position: usize,
    /// How many rows of content there are in all.
    total: usize,
    /// Where the content was drawn; the bar lines up with these rows and no others.
    area: Rect,
}

/// Draws a scrollbar down the window's right-hand border, when there is anything to scroll.
///
/// note: on the border rather than in a column of its own. A tab that gave up a column would be
/// one character narrower for the whole session in order to say something that is only true some
/// of the time, and the two table tabs spend that column on the thing they exist for - what the
/// model will actually read of an item. The track *is* the border character, so a window with
/// nothing to scroll looks exactly as it did before.
///
/// note: it lines up with the content rather than with the window: the context and permissions
/// tabs spend their first row on a header, and a bar that started above it would be off by one
/// for the whole length of the list.
fn scrollbar(frame: &mut Frame, window: Rect, border: Style, scrolled: Scrolled) {
    let viewport = scrolled.area.height as usize;
    if viewport == 0 || scrolled.total <= viewport {
        return;
    }

    let bar = Rect {
        x: window.right().saturating_sub(1),
        y: scrolled.area.y,
        width: 1,
        height: scrolled.area.height,
    };
    // note: `content_length` is the number of *scroll positions*, not the number of rows, and the
    // difference is the whole reason the thumb used to stop short of the bottom. Ratatui places
    // the thumb over `0..content_length` and adds the viewport back on at the far end, so passing
    // the row count says the last position is "the final row alone at the top" - a page further
    // down than anything here scrolls to. Every tab stops at the last full page, so the positions
    // it can be in are `total - viewport + 1`, and with that the thumb reaches the last row when
    // the content does.
    let mut state = ScrollbarState::new(scrolled.total - viewport + 1)
        .position(scrolled.position)
        .viewport_content_length(viewport);

    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            // the ends of the bar are the corners of the window, which are already drawn
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(border)
            .thumb_symbol("█")
            .thumb_style(border),
        bar,
        &mut state,
    );
}

/// What the open tab has to say about itself, along the bottom.
fn footer(app: &App) -> String {
    match app.tab {
        Tab::Chat => match app.busy {
            true => " esc stops it ".to_owned(),
            false => " alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions ".to_owned(),
        },
        Tab::Context => {
            // counted by whether the model is being shown what the item says, which is the
            // question this line is answering. An elided item is in the request and is not being
            // shown, and calling it "going" would be the more misleading of the two
            let items = app.kernel.items();
            let out = items
                .iter()
                .filter(|item| !item.state.sends_content())
                .count();
            let elided = items.iter().filter(|item| item.state.is_elided()).count();
            match (out, elided) {
                (0, _) => format!(" {} items, all of them going ", items.len()),
                (n, 0) => format!(" {} items, {n} not going ", items.len()),
                (n, e) => format!(" {} items, {n} not going, {e} elided ", items.len()),
            }
        }
        // the pane keeps the last few hundred; the log keeps everything, and `/save` writes it
        Tab::Trace => format!(" {} events · /save keeps them all ", app.trace.len()),
        // note: the caveat comes first because it is the one thing on this tab that is not
        // negotiable. A registered shell that is not refused can read, write and reach the
        // network whatever the other rows answer, so a tab that listed five verdicts and said
        // nothing about that would be reporting four restrictions that are not there
        // note: the count of what is *not* listed. The tab is the decisions; this is the honest
        // footnote that they are not the whole policy
        Tab::Permissions => {
            let mut parts = Vec::new();
            if let Some(line) = app.confinement() {
                parts.push(line);
            }
            match app.undecided() {
                0 => {}
                n => parts.push(format!("{n} more it will ask about")),
            }
            // note: the same keys whatever else is on the line. Which of them were named used to
            // depend on whether there was a sandbox line to fit in beside them, so `r` was
            // advertised only on a tab with no shell on it - and with nothing decided yet the
            // footer offered three keys that do nothing whatever, because there is no row for
            // them to act on
            if !app.permissions().is_empty() {
                parts.push("space cycles · a allow · n never · r ask again".to_owned());
            }

            format!(" {} ", parts.join(" · "))
        }
    }
}

// ------------------------------------------------------------------------------ the conversation

fn draw_chat(frame: &mut Frame, app: &mut App, inner: Rect) -> Scrolled {
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        // the model writes markdown, and a terminal that printed the asterisks would be showing
        // the punctuation instead of the emphasis. Nothing else here is markdown: a tool's output
        // is whatever the tool said, and running it through a renderer would be inventing
        // structure the tool did not put there
        if entry.speaker == Speaker::Model {
            let split = chunks(&entry.text);
            let last = split.len().saturating_sub(1);
            for (nth, chunk) in split.into_iter().enumerate() {
                // a fenced block gets a rule down its left rather than a slab of background,
                // which is the one thing a terminal cannot do without knowing the theme - and its
                // tokens in colours chosen the same way
                let prose = match chunk {
                    Chunk::Code { language, body } => {
                        // the markdown renderer put a blank line either side of a block, and it
                        // is not rendering these any more
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
                        true => lines.extend(gutter(&line, width)),
                        false => match rule(&line) {
                            // a horizontal rule, drawn rather than spelled `---`
                            true => lines.push(Line::styled("─".repeat(width), faint())),
                            false => lines.extend(refit(&line, width)),
                        },
                    }
                }
            }
            lines.push(Line::default());
            continue;
        }

        let (prefix, style) = match entry.speaker {
            Speaker::User => ("> ", Style::default().fg(Color::White).bold()),
            Speaker::Reasoning => ("", quiet().italic()),
            Speaker::Call => ("⟩ ", Style::default().fg(Color::Cyan)),
            Speaker::Result => ("│ ", quiet()),
            Speaker::Note => ("· ", quiet()),
            Speaker::Error => ("! ", Style::default().fg(Color::Red)),
            Speaker::Model => unreachable!("rendered above"),
        };

        for text in wrapped(&entry.text, width, prefix) {
            lines.push(Line::styled(text, style));
        }
        lines.push(Line::default());
    }

    // what the last frame measured is what the scrolling keys are working against
    app.rendered = lines.len();
    app.viewport = inner.height as usize;
    let bottom = lines.len().saturating_sub(inner.height as usize);
    if app.follow {
        app.scroll = bottom;
    }
    let at = app.scroll.min(bottom);
    let total = lines.len();

    frame.render_widget(Paragraph::new(lines).scroll((at as u16, 0)), inner);

    Scrolled {
        position: at,
        total,
        area: inner,
    }
}

// ---------------------------------------------------------------------------------- the context

/// The context as a table: every item, what kind of thing it is, what it costs, and either the
/// first line of what it says or - if it is not going into the next request - why not.
///
/// note: With the whole window to work in there is room for the last column, and it is the one
/// that matters: a list of labels and numbers tells you an item exists, and this tells you what
/// the model is actually being told.
fn draw_context(frame: &mut Frame, app: &mut App, area: Rect) -> Scrolled {
    let items = app.kernel.items();
    if items.is_empty() {
        frame.render_widget(Paragraph::new("nothing here yet").style(quiet()), area);
        return Scrolled::default();
    }

    let [head, inner] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = inner.width as usize;

    // the columns give way from the right as the window narrows, so this works in eighty
    let label = 26.min(width / 3);
    let kind = if width >= 84 { 18 } else { 0 };
    let counted = 4 + 2 + label + 1 + kind + 8 + 2;
    let says = width.saturating_sub(counted);

    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "  {:<4}{:<label$} {:<kind$}{:>8}  {}",
                "id",
                "label",
                match kind {
                    0 => "",
                    _ => "kind",
                },
                "tokens",
                match says >= 8 {
                    true => "what it says, or why it is not being sent",
                    false => "",
                }
            ),
            quiet(),
        )),
        head,
    );

    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let (mark, style) = match item.state {
                ContextState::Active => ("·", Style::default()),
                ContextState::Pinned => ("▪", Style::default().fg(Color::Yellow)),
                ContextState::Excluded => ("-", quiet()),
                // in the request, but only as a marker: a mark of its own, because "going" and
                // "not going" is the wrong question about it and either answer would mislead
                ContextState::Elided => ("…", quiet()),
                ContextState::Archived => ("▫", quiet()),
                ContextState::Superseded => ("~", quiet()),
                _ => ("?", quiet()),
            };

            // an item that is not going says why, in the projector's own words; one that is
            // shows the first thing the model will read of it. An elided one is on the first
            // side of that: what the model reads is the note, so the note is what to show
            let (tail, tail_style) = match item.state.sends_content() {
                false => (
                    match &item.note {
                        Some(note) => format!("{}: {note}", item.state),
                        None => item.state.to_string(),
                    },
                    quiet().italic(),
                ),
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

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{:>3} {mark} {:<label$} ",
                        item.id.0,
                        clip(&item.label, label)
                    ),
                    style,
                ),
                Span::styled(
                    match kind {
                        0 => String::new(),
                        _ => format!("{:<kind$}", clip(item.kind.name(), kind)),
                    },
                    quiet(),
                ),
                Span::styled(format!("{:>8}  ", thousands(item.tokens)), style),
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

/// What the policy will answer about each capability, and which tools that covers.
///
/// note: The permission prompt is the policy's only other appearance, and it shows up one call at
/// a time, at the worst possible moment to think about it. This is the same decisions, all of
/// them, in advance, and changeable - which is also the plainest thing to point at when somebody
/// asks what a replaceable `PermissionPolicy` buys you: a policy is an object with state, not a
/// callback you can only learn about by triggering it.
fn draw_permissions(frame: &mut Frame, app: &mut App, area: Rect) -> Scrolled {
    let rows = app.permissions();
    if rows.is_empty() {
        // note: what is *not* here is a row per thing nobody has answered about yet. The policy
        // asks about everything by default, so listing the defaults is listing the absence of
        // decisions - and it buried the one or two lines that say what this agent can do without
        // stopping. What arrives here is what somebody answered `a` or `n` to
        frame.render_widget(
            Paragraph::new(
                "nothing has been decided yet.\n\nThe policy asks about everything it has not \
                 been told about; answer a question with `a` or `n` and it will be here, where it \
                 can be changed.",
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
                    true => "the tools it covers",
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
            let (answer, style) = match row.verdict {
                Verdict::Allow => ("allow", Style::default().fg(Color::Green)),
                Verdict::Ask => ("ask", Style::default().fg(Color::Yellow)),
                Verdict::Deny => ("deny", Style::default().fg(Color::Red)),
            };
            // a capability nothing declares is still worth a row, and it should say so rather
            // than look like an oversight - but `network` is not one of them, however it looks: no
            // tool declares it and the shell is judged against it anyway, on what the command says
            let tools = match (row.tools.is_empty(), row.sometimes.is_empty()) {
                (true, true) => "nothing registered needs it".to_owned(),
                (true, false) => format!(
                    "{}, when the command reaches for it",
                    row.sometimes.join(", ")
                ),
                (false, true) => row.tools.join(", "),
                (false, false) => format!(
                    "{}; {}, when the command reaches for it",
                    row.tools.join(", "),
                    row.sometimes.join(", ")
                ),
            };

            ListItem::new(Line::from(vec![
                Span::raw(format!(
                    "  {:<capability$} ",
                    clip(&row.subject.to_string(), capability)
                )),
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
        // note: underlined rather than a dark slab behind it. `Rgb(40, 40, 40)` is a shade of the
        // background this program does not know it has - it reads as barely-there on a dark theme
        // and as a black bar on a light one, which is the same mistake the code blocks avoid
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

/// Every event, newest at the bottom, in two aligned columns.
///
/// note: The names are a column of their own rather than run together with what they say, which
/// is what a whole window buys: `permission.requested` is the longest of them, so twenty-two
/// columns line every event up under the last. Anything that still does not fit wraps under the
/// column rather than being cut off - a log whose lines end in an ellipsis in the middle of the
/// interesting part is not a log.
fn draw_trace(frame: &mut Frame, app: &mut App, inner: Rect) -> Scrolled {
    const NAMES: usize = 22;

    let (width, height) = (inner.width as usize, inner.height as usize);
    let column = match width >= NAMES + 20 {
        true => NAMES,
        false => 0,
    };

    let mut lines: Vec<Line> = Vec::new();
    for event in &app.trace {
        let colour = match () {
            _ if event.name.ends_with(".failed") => Color::Red,
            _ if event.name.starts_with("permission") => Color::Yellow,
            _ if event.name.is_empty() => Color::Gray,
            _ => Color::White,
        };

        let named = Style::default().fg(colour);
        let said = quiet();
        let indent = " ".repeat(column.max(2));
        let mut detail = wrapped(&event.detail, width, &indent).into_iter();

        match (event.name.is_empty(), event.detail.is_empty()) {
            // a continuation: something the event before it had more to say about
            (true, _) => lines.extend(detail.map(|line| Line::styled(line, said))),
            (false, true) => lines.push(Line::styled(event.name.clone(), named)),
            (false, false) => {
                // the first line of the detail sits beside the name, the rest under it
                let first = detail.next().unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<column$}", event.name), named),
                    Span::styled(first.trim_start().to_owned(), said),
                ]));
                lines.extend(detail.map(|line| Line::styled(line, said)));
            }
        }
    }

    // it is a log, so it is read from the bottom; `trace_scroll` counts upwards from there
    let bottom = lines.len().saturating_sub(height);
    app.trace_scroll = app.trace_scroll.min(bottom);
    let at = bottom - app.trace_scroll;
    let total = lines.len();

    frame.render_widget(Paragraph::new(lines).scroll((at as u16, 0)), inner);

    Scrolled {
        position: at,
        total,
        area: inner,
    }
}

// ------------------------------------------------------------------------------------ the prompt

fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Input;
    // the same box does two jobs, so it has to say which one it is doing: typing into it
    // ordinarily sends a message, and typing into it while an item is being edited rewrites what
    // the model will read
    let (title, colour) = match app.editing {
        Some(id) => (
            format!(" editing [{id}] · enter commits · esc cancels "),
            Color::Yellow,
        ),
        None => (
            match focused {
                true => " you ".to_owned(),
                false => " you · tab ".to_owned(),
            },
            match focused {
                true => Color::White,
                false => Color::Gray,
            },
        ),
    };
    app.input.set_block(
        Block::bordered()
            .title(title)
            .border_style(Style::default().fg(colour)),
    );
    // the cursor belongs wherever the keys are going
    app.input.set_cursor_style(match focused {
        true => Style::default().add_modifier(Modifier::REVERSED),
        false => Style::default(),
    });

    frame.render_widget(&app.input, area);
}

// ------------------------------------------------------------------------------- the status line

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let dim = quiet();
    let mut spans = vec![match app.busy {
        true => Span::styled(
            format!(" {} ", state_name(&app.kernel)),
            Style::default().fg(Color::Yellow),
        ),
        false => Span::styled(format!(" {} ", state_name(&app.kernel)), dim),
    }];

    let mut add = |text: String, style: Style| {
        spans.push(Span::styled("· ", dim));
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    };

    // the address as well as the name, because the same name at a different address is a different
    // model - `/model` and `/seams` have said so all along, but only when asked, so a session
    // pointed at a local ollama looked exactly like one talking to OpenRouter. The host alone:
    // the rest of the URL is `/provider`'s to show, and there is no room for it here
    if let Some(info) = app.kernel.model_info() {
        add(format!("{} @ {}", info.model, app.provider.host()), dim);
    }

    let budget = app.kernel.budget();
    let used = thousands(budget.used());
    add(
        match (budget.fraction_used(), budget.limit) {
            // a decimal place, because rounding a large context down to "0%" reads like a
            // measurement that is not being taken; and the limit itself, because a percentage
            // of an unstated total is not a fact anybody can act on
            (Some(fraction), Some(limit)) => {
                format!(
                    "~{used} tokens, {:.1}% ({})",
                    fraction * 100.0,
                    compact(limit)
                )
            }
            _ => format!("~{used} tokens, of an unknown limit"),
        },
        match budget.fraction_used() {
            Some(fraction) if fraction >= 0.9 => Style::default().fg(Color::Red),
            Some(fraction) if fraction >= 0.7 => Style::default().fg(Color::Yellow),
            Some(_) => Style::default().fg(Color::Green),
            None => dim,
        },
    );

    // the `~` above is not decoration: that figure is an estimate from a counter that does not
    // have the model's tokenizer. This one is what the provider charged for, and the two being
    // side by side is the only reason either can be trusted. `/budget` has the whole story
    if let Some(reported) = budget.reported.and_then(|usage| usage.input_tokens) {
        add(format!("{} really", thousands(reported as usize)), dim);
    }

    let withheld = app.kernel.with_context(|context| context.tokens_withheld());
    if withheld != 0 {
        add(format!("{} held back", thousands(withheld)), dim);
    }
    add(
        match app.busy {
            true => "esc stops it".to_owned(),
            false => "F1 for the keys".to_owned(),
        },
        dim,
    );

    // the line is drawn without wrapping, so anything past the right edge is simply gone - and
    // what sits at that end is the provider's own figure and the key that opens the help, which
    // are worth more than the address. The address is what gives way: `openrouter.ai` costs 16
    // columns and `generativelanguage.googleapis.com` costs 36, which is the difference between
    // a line that fits at 100 columns and one that loses its last two facts
    let line = Line::from(spans);
    let over = line.width().saturating_sub(area.width as usize);
    let line = match over {
        0 => line,
        _ => Line::from(shrink_host(line.spans, over)),
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// The least of an address worth showing. Below this it goes entirely: `gen…` names nothing, and
/// the columns it was costing are better spent on the figures to its right.
const HOST_FLOOR: usize = 8;

/// The same spans with the ` @ host` shortened to claw back `over` columns, or taken off when
/// there is no shortening of it left worth reading.
fn shrink_host(spans: Vec<Span<'static>>, over: usize) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let style = span.style;
            let Some((model, host)) = span.content.split_once(" @ ") else {
                return span;
            };

            // one column of the saving goes on the ellipsis that says it was shortened
            let keep = Span::raw(host).width().saturating_sub(over + 1);
            let shortened = match keep >= HOST_FLOOR {
                // from the right: the leftmost label is the one that distinguishes an endpoint -
                // `generativelanguage` in Google's, the resource name in an Azure deployment -
                // and the rest of it is a domain shared with everything else the vendor runs
                true => format!("{model} @ {}…", &host[..prefix_within(host, keep)]),
                false => model.to_owned(),
            };

            Span::styled(shortened, style)
        })
        .collect()
}

/// What the runtime is doing, in one word.
fn state_name(kernel: &Kernel) -> &'static str {
    match kernel.state() {
        State::Idle => "idle",
        State::Requesting => "asking",
        State::Ready { .. } => "ready",
        State::Executing { .. } => "running",
        State::Deciding { .. } => "waiting on you",
        State::Finished { .. } => "done",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------------- the overlays

fn draw_overlay(frame: &mut Frame, overlay: &Overlay, app: &App) {
    match overlay {
        Overlay::Text {
            title,
            body,
            scroll,
        } => panel(frame, &format!(" {title} "), body, *scroll, 100, 90),
        Overlay::Permission => draw_permission(frame, app),
    }
}

/// A tool is waiting to be told whether it may run.
fn draw_permission(frame: &mut Frame, app: &App) {
    let Some(request) = app.kernel.pending_permissions().into_iter().next() else {
        return;
    };
    let waiting = app.kernel.pending_permissions().len();

    // what the policy will actually consult, not what the tool declared: `shell` reaching for the
    // network and `read` handed a path there is a rule about are both judged against something the
    // spec does not mention, and a question that named only the spec would be answering for less
    // than it decides
    let judged: Vec<String> = app
        .policy
        .judges(&request)
        .iter()
        .map(|subject| subject.to_string())
        .collect();
    // two lines of options rather than one that wraps wherever it happens to run out: the answers
    // on the first, and the two that are about looking closer or giving up on the lot on the
    // second
    let body = format!(
        "{} wants: {}\n\n{}\n\
         [y] once   [a] always, for {}   [n] no\n\
         [i] the exact JSON   [d] {}{}",
        request.tool,
        judged.join(", "),
        readable(&request.args),
        judged.join(" and "),
        match waiting > 1 {
            true => "drop them all",
            false => "drop it",
        },
        match waiting > 1 {
            true => format!("\n\n{} more after this one", waiting - 1),
            false => String::new(),
        }
    );

    // as tall as the question is, rather than a fixed box with a hole in it: the arguments are
    // the part somebody has to actually read, and they are as long as they are
    let area = centred(frame.area(), 72, wrapped(&body, 68, "").len() as u16 + 2);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(wrapped(&body, area.width.saturating_sub(4) as usize, "").join("\n")).block(
            Block::bordered()
                .title(" a tool wants to run ")
                .border_style(Style::default().fg(Color::Yellow))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

/// Renders a tool's arguments so that a person can read them before saying yes to them.
///
/// note: `to_string_pretty` turns the `new` argument of an edit into one enormous line with `\n`
/// written out in the middle of it, which is exactly the argument somebody needs to read most
/// carefully. Multi-line strings are put back into lines here, and `[i]` still shows the JSON
/// verbatim - readable by default, exact on request, and neither one hiding the other.
fn readable(args: &serde_json::Value) -> String {
    let Some(fields) = args.as_object() else {
        return serde_json::to_string_pretty(args).unwrap_or_default();
    };
    if fields.is_empty() {
        return "(no arguments)\n\n".to_owned();
    }

    let mut out = String::new();
    for (name, value) in fields {
        match value {
            serde_json::Value::String(text) if text.contains('\n') => {
                out.push_str(&format!("{name}:\n"));
                for line in text.lines() {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            serde_json::Value::String(text) => out.push_str(&format!("{name}: {text}\n")),
            other => out.push_str(&format!("{name}: {other}\n")),
        }
        out.push('\n');
    }

    out
}

/// A bordered box over the middle of the screen.
fn panel(frame: &mut Frame, title: &str, body: &str, scroll: usize, columns: u16, percent: u16) {
    // no taller than it has anything to say: `/budget` is six lines, and a box that took nine
    // tenths of the screen to show them would be hiding the conversation for no reason
    let wanted = wrapped(body, columns.saturating_sub(4) as usize, "").len() as u16 + 2;
    let area = centred(
        frame.area(),
        columns,
        wanted.min(frame.area().height * percent / 100),
    );
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(ratatui::widgets::Padding::horizontal(1));
    let inner = block.inner(area);

    let lines = wrapped(body, inner.width as usize, "");
    let at = scroll.min(lines.len().saturating_sub(inner.height as usize));
    let footer = format!(
        " {}–{} of {} · any key closes ",
        at + 1,
        (at + inner.height as usize).min(lines.len()),
        lines.len()
    );

    let total = lines.len();
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .scroll((at as u16, 0))
            .block(block.title_bottom(footer)),
        area,
    );

    scrollbar(
        frame,
        area,
        Style::default().fg(Color::Cyan),
        Scrolled {
            position: at,
            total,
            area: inner,
        },
    );
}

/// Puts a blank line in, unless there is one there already or there is nothing to separate from.
fn separate(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_some_and(|line| !line.spans.is_empty()) {
        lines.push(Line::default());
    }
}

/// What a fence's info string means to the highlighter, which thinks in file extensions.
///
/// note: models write `rust` and `python` far more often than `rs` and `py`, and a block that
/// silently came out uncoloured because of that would look like the highlighter had failed. The
/// list is short on purpose: anything not here is passed through as-is, which already covers `rs`,
/// `go`, `sql`, `toml` and the rest.
fn extension(language: &str) -> &str {
    match language.to_ascii_lowercase().as_str() {
        "rust" => "rs",
        "python" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "console" | "zsh" | "fish" => "sh",
        "yaml" => "yml",
        "c++" => "cpp",
        "c#" | "csharp" => "cs",
        "golang" => "go",
        "rb" | "ruby" => "rb",
        "makefile" | "make" => "sh",
        _ => language,
    }
}

/// The colour a token of that name is drawn in.
///
/// note: by name rather than by theme, which is the whole reason the highlighting is done here
/// instead of by the markdown renderer. A theme is a set of 24-bit colours chosen against a known
/// background, and this program does not know the background - the same argument that put a rule
/// down the left of a code block instead of a slab behind it. Named colours are the terminal's
/// own, so they are legible in whatever the user has set up.
fn token(name: &str) -> Style {
    let colour = match name {
        "comment" => Color::Gray,
        "string" | "character" => Color::Green,
        "keyword" | "kw" => Color::Magenta,
        "digit" | "boolean" => Color::Yellow,
        "function" | "macro" | "tag" => Color::Blue,
        "struct" | "namespace" | "type" | "attribute" | "key" => Color::Cyan,
        "operator" | "reference" => Color::Reset,
        // markdown, diffs and the rest of what synoptic knows about; whatever is left is code
        "heading" | "header" | "bold" => return Style::default().bold(),
        "italic" | "quote" => return Style::default().italic(),
        "insertion" => Color::Green,
        "deletion" => Color::Red,
        "link" | "list" => Color::Cyan,
        _ => Color::Reset,
    };

    Style::default().fg(colour)
}

/// Draws a fenced block: a rule down the left, and its tokens in this program's own colours.
///
/// note: the block is handed to the highlighter whole, so that a string or a comment running
/// across lines is one token rather than several guesses. A language nothing here recognises still
/// gets the rule and the wrapping - it is a code block whether or not anybody can colour it.
fn highlighted(language: &str, body: &str, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let room = width.saturating_sub(2).max(8);
    let source: Vec<String> = body.lines().map(str::to_owned).collect();

    let mut highlighter = synoptic::from_extension(extension(language), 4);
    if let Some(highlighter) = &mut highlighter {
        highlighter.run(&source);
    }

    let mut drawn = Vec::new();
    for (y, line) in source.iter().enumerate() {
        let spans: Vec<(String, Style)> = match &highlighter {
            Some(highlighter) => highlighter
                .line(y, line)
                .into_iter()
                .map(|piece| match piece {
                    synoptic::TokOpt::Some(text, name) => (text, token(&name)),
                    synoptic::TokOpt::None(text) => (text, Style::default()),
                })
                .collect(),
            None => vec![(line.clone(), Style::default().fg(Color::Cyan))],
        };

        for row in fit(spans, room) {
            let mut cells = vec![bar.clone()];
            cells.extend(row);
            drawn.push(Line::from(cells));
        }
    }

    drawn
}

/// Breaks a run of styled pieces into rows no wider than `room`, keeping every style.
///
/// note: code is not prose and is not wrapped like it. A line that is too long is cut where the
/// room runs out rather than at a word boundary, because the alternative - reflowing - would be
/// showing something the model did not write.
fn fit(spans: Vec<(String, Style)>, room: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = vec![Vec::new()];
    let mut used = 0;

    for (text, style) in spans {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let left = room - used;
            let taken: String = rest.chars().take(left).collect();
            let bytes = taken.len();
            if !taken.is_empty() {
                used += taken.chars().count();
                rows.last_mut()
                    .expect("there is always a row")
                    .push(Span::styled(taken, style));
            }
            rest = &rest[bytes..];
            if used >= room && !rest.is_empty() {
                rows.push(Vec::new());
                used = 0;
            }
        }
    }

    // a block ending in a newline gives a last row with nothing in it, which is a blank line the
    // model did not write
    if rows.last().is_some_and(Vec::is_empty) && rows.len() > 1 {
        rows.pop();
    }

    rows
}

/// One stretch of a model's answer: either prose, or a fenced block with the language it claimed.
#[derive(Debug, PartialEq)]
enum Chunk<'a> {
    /// Everything that is not a fenced block, markdown and all.
    Prose(&'a str),
    /// A fenced block, without its fences.
    Code {
        /// The info string the fence carried, e.g. `rust`; empty if it carried none.
        language: &'a str,
        /// What is between the fences.
        body: &'a str,
    },
}

/// Splits an answer at its fences.
///
/// note: done here rather than left to the markdown renderer, which is otherwise perfectly able
/// to handle a code block, because colouring one needs three things the renderer does not hand
/// back: the language the fence claimed, the whole block at once - a string or a comment can run
/// across lines, and a highlighter shown one line at a time gets those wrong - and the fact that
/// it *is* a block, which is what earns it the rule down its left.
///
/// note: an unterminated fence runs to the end of the answer rather than being read as prose,
/// because every code block is unterminated for as long as it is still arriving.
fn chunks(text: &str) -> Vec<Chunk<'_>> {
    /// The fence a line opens or closes with: its character and how many of them.
    fn fence(line: &str) -> Option<(char, usize)> {
        // up to three spaces of indent, per CommonMark; four would make it an indented block
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            return None;
        }
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let run = trimmed.chars().take_while(|c| *c == marker).count();

        (run >= 3).then_some((marker, run))
    }

    let mut chunks = Vec::new();
    let mut prose = 0;
    let mut open: Option<(char, usize, usize)> = None;
    let mut at = 0;

    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let bare = line.trim_end_matches(['\n', '\r']);

        match open {
            None => {
                let Some((marker, run)) = fence(bare) else {
                    continue;
                };
                if start > prose {
                    chunks.push(Chunk::Prose(&text[prose..start]));
                }
                open = Some((marker, run, at));
            }
            // a closing fence is the same character, at least as long, and says nothing else
            Some((marker, run, body)) => {
                let closes = fence(bare).is_some_and(|(c, n)| c == marker && n >= run)
                    && bare.trim().chars().all(|c| c == marker);
                if !closes {
                    continue;
                }
                let language = text[start_of_line(text, body)..body].trim();
                let language = language.trim_start_matches([marker]).trim();
                chunks.push(Chunk::Code {
                    language,
                    body: &text[body..start],
                });
                open = None;
                prose = at;
            }
        }
    }

    match open {
        // still arriving: what there is of it is a block, not prose
        Some((marker, _, body)) => {
            let language = text[start_of_line(text, body)..body].trim();
            chunks.push(Chunk::Code {
                language: language.trim_start_matches([marker]).trim(),
                body: &text[body..],
            });
        }
        None if text.len() > prose => chunks.push(Chunk::Prose(&text[prose..])),
        None => {}
    }

    chunks
}

/// Where the line ending at `end` began.
fn start_of_line(text: &str, end: usize) -> usize {
    text[..end]
        .trim_end_matches('\n')
        .rfind('\n')
        .map(|at| at + 1)
        .unwrap_or(0)
}

/// A rectangle in the middle, at most `columns` by `rows`, and never bigger than what it is in.
///
/// note: the smallest a box is allowed to be comes *before* the size of the thing it is in, and
/// the last word is the terminal's. A box has to be about twenty columns and four rows to be worth
/// drawing at all, but a terminal narrower or shorter than that is not a reason to draw outside the
/// buffer, which is a panic: `F1` in a one-row window took the whole program down and the session
/// with it, and a window is one row for as long as somebody is dragging its edge.
fn centred(area: Rect, columns: u16, rows: u16) -> Rect {
    let width = columns
        .min(area.width.saturating_sub(4))
        .max(20)
        .min(area.width);
    let height = rows
        .min(area.height.saturating_sub(2))
        .max(4)
        .min(area.height);

    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// How markdown is dressed for a terminal that does not know what colour the background is.
///
/// note: The defaults put a cyan slab behind an H1 and a black one behind every code block, which
/// looks like a redaction on a dark theme and a bruise on a light one. Weight and colour say the
/// same thing and survive both. The `#` and the ``` are dropped: they are the punctuation of the
/// format, and what is wanted is what they mean.
#[derive(Clone)]
struct Markdown;

impl tui_markdown::StyleSheet for Markdown {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default().fg(Color::Yellow).bold().underlined(),
            2 => Style::default().fg(Color::Yellow).bold(),
            _ => Style::default().bold(),
        }
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> Style {
        Style::default().fg(Color::Cyan)
    }

    fn code_block_fence(&self) -> &str {
        ""
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Gray).italic()
    }

    fn link(&self) -> Style {
        Style::default().fg(Color::Blue).underlined()
    }
}

/// The options every model answer is rendered with.
fn markdown() -> tui_markdown::Options<Markdown> {
    tui_markdown::Options::new(Markdown)
}

/// Whether a line is a horizontal rule, which markdown spells in punctuation.
///
/// note: Only outside a fenced block, where `---` is three characters a tool printed rather than
/// a divider a model asked for; the caller has already sent those the other way.
fn rule(line: &Line<'_>) -> bool {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let text = text.trim();

    text.len() >= 3
        && (text.chars().all(|c| c == '-')
            || text.chars().all(|c| c == '*')
            || text.chars().all(|c| c == '_'))
}

/// A line of a fenced code block: a rule down the left, and no reflowing of what is inside it.
fn gutter(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let code = Style::default().fg(Color::Cyan);
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    split_to_fit(&text, width.saturating_sub(2).max(8))
        .into_iter()
        .map(|piece| Line::from(vec![bar.clone(), Span::styled(piece, code)]))
        .collect()
}

/// Breaks a line of already-styled fragments into lines that fit, keeping every fragment's style
/// and hanging the continuations under whatever the line was indented by.
///
/// note: `Paragraph` can wrap, but it wraps at render time, and this program counts lines to know
/// where it is scrolled to - a widget that quietly turned forty lines into sixty would put the
/// scrolling out by twenty. So the wrapping happens here, where the count is taken.
fn refit(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let plain: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if plain.chars().count() <= width {
        return vec![
            Line::from(
                line.spans
                    .iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style))
                    .collect::<Vec<_>>(),
            )
            .style(line.style),
        ];
    }

    // an indented line's continuations belong under it rather than back at the margin, and so do
    // a list item's: `- ` and `1. ` are indentation as far as reading it goes
    let spaces = plain.chars().take_while(|c| *c == ' ').count();
    let hang = " ".repeat(spaces + bullet(&plain[spaces..]));
    let hang = match hang.chars().count() + 8 < width {
        true => hang,
        false => String::new(),
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut column = 0;
    for span in &line.spans {
        // `split_inclusive` keeps the spaces, so what is placed is a word and the gap after it
        for word in span.content.split_inclusive(' ') {
            for piece in split_to_fit(word, width - hang.chars().count()) {
                let length = piece.trim_end().chars().count();
                if column != 0 && column + length > width {
                    out.push(Line::from(std::mem::take(&mut current)).style(line.style));
                    column = hang.chars().count();
                    if !hang.is_empty() {
                        current.push(Span::raw(hang.clone()));
                    }
                }
                current.push(Span::styled(piece.clone(), span.style));
                column += piece.chars().count();
            }
        }
    }
    out.push(Line::from(current).style(line.style));

    out
}

/// The width of a list marker at the start of a line, or nought if there is not one.
fn bullet(text: &str) -> usize {
    if let Some(rest) = text.strip_prefix(['-', '*', '+'])
        && rest.starts_with(' ')
    {
        return 2;
    }

    // `12. ` and `12) `, however many digits it runs to
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    match digits != 0 && matches!(text.get(digits..digits + 2), Some(". ") | Some(") ")) {
        true => digits + 2,
        false => 0,
    }
}

/// Splits a word that is wider than the line into pieces that are not.
fn split_to_fit(word: &str, width: usize) -> Vec<String> {
    if word.chars().count() <= width {
        return vec![word.to_owned()];
    }
    let characters: Vec<char> = word.chars().collect();

    characters
        .chunks(width.max(1))
        .map(|piece| piece.iter().collect())
        .collect()
}

// ------------------------------------------------------------------------------------- the words

/// Breaks text into lines that fit, keeping the newlines it already had, hanging continuations
/// under the prefix, and leaving a line that already fits exactly as it was.
///
/// note: The last of those is what makes code and command output readable in here - a wrapper
/// that reflowed every line would turn an indented block into a paragraph. A line that has to be
/// broken keeps its own indentation on the pieces, so a list stays a list.
fn wrapped(text: &str, width: usize, prefix: &str) -> Vec<String> {
    let head = prefix.chars().count();
    let width = width.max(head + 12);
    let hanging = " ".repeat(head);

    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let lead: String = paragraph.chars().take_while(|c| *c == ' ').collect();
        let room = (width - head).saturating_sub(lead.chars().count()).max(12);

        for line in fold(paragraph.trim_start(), room) {
            let start = match out.is_empty() {
                true => prefix,
                false => &hanging,
            };
            out.push(format!("{start}{lead}{line}"));
        }
    }

    out
}

/// Breaks one paragraph into pieces no wider than `room`.
///
/// note: `split(' ')` rather than `split_whitespace`, because the latter collapses a run of
/// spaces into one and this is what draws `/payload` - a panel whose whole claim is that it shows
/// the bytes that would go out. A file's indentation inside a JSON string is spaces in a row, and
/// re-flowing them away would be quietly answering a different question. It keeps the help's
/// columns lined up in a narrow pane, too.
fn fold(body: &str, room: usize) -> Vec<String> {
    if body.chars().count() <= room {
        return vec![body.to_owned()];
    }

    let mut out = Vec::new();
    let mut line = String::new();
    // a word can now be the empty string - that is what a run of spaces is made of - so "have I
    // put anything on this line yet" is its own question rather than `line.is_empty()`
    let mut fresh = true;

    for word in body.split(' ') {
        // a single word longer than the pane is broken rather than allowed to overflow; the last
        // piece stays open, so that whatever follows can share the line with it
        if word.chars().count() > room {
            if !fresh {
                out.push(std::mem::take(&mut line));
            }
            let characters: Vec<char> = word.chars().collect();
            for piece in characters.chunks(room) {
                out.push(piece.iter().collect());
            }
            line = out.pop().unwrap_or_default();
            fresh = false;
            continue;
        }

        if !fresh && line.chars().count() + 1 + word.chars().count() > room {
            out.push(std::mem::take(&mut line));
            fresh = true;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(word);
        fresh = false;
    }
    out.push(line);

    out
}

/// Shortens text to a width, with an ellipsis if it had to.
fn clip(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        true if width > 1 => format!("{}…", text.chars().take(width - 1).collect::<String>()),
        true => text.chars().take(width).collect(),
        false => text.to_owned(),
    }
}

/// A round number in as few characters as it can be said in: `1M`, `131k`, `4.1k`.
///
/// note: For the limit rather than the count. Nobody reads `1,048,576` as anything but "a lot",
/// and it is the shape of the number that matters when it is sitting next to a percentage.
fn compact(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1_000.0),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        1_000_000..10_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
        _ => format!("{}M", n / 1_000_000),
    }
}

/// Formats a number with `,` as the thousands separator.
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();

    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }

    out
}
