//! Drawing. Nothing here decides anything - it reads [`App`] and the kernel and puts what it
//! finds on the screen.
//!
//! note: the frame and its chrome - the tab bar, the footer, the prompt, the status line - and
//! `draw`, the one entry point. `tabs` draws the four bodies, `overlay` the panel that floats over
//! one of them, `markdown` and `table` turn what a model wrote into styled lines, and `text`
//! measures and fits all of it. Everything but [`GREETING`], [`SECTIONS`], [`Section`],
//! [`everything`], [`wrapped_rows`] and `draw` itself is private to this module, because a screen
//! is not an API.
//!
//! note: this module is behind the `tui` feature. `/help` is answered by a build with no screen, so
//! the text a command owns cannot sit behind the feature that draws; the key reference lives in
//! [`crate::help`], which works in every configuration. What is here is a re-export, because `ui`
//! is the path somebody may already be reaching it through.

use std::time::Duration;

use nachalnik::{Budget, Kernel, State};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::app::{
    App, Focus, Going, Overlay, Tab,
    text::{plural, thousands},
};

mod markdown;
mod overlay;
mod table;
mod tabs;
mod text;

pub use crate::help::{SECTIONS, Section, everything};
use crate::ui::{
    overlay::{BESIDES, MIN_CHAT, MIN_QUESTION, draw_overlay, draw_question, question_rows},
    tabs::{draw_chat, draw_context, draw_permissions, draw_trace},
    text::{clip, columns, compact, prefix_within, rows_for, suffix_within},
};

/// The first line of a session that is not being resumed.
///
/// note: `pub` for the same reason [`SECTIONS`] is: a test can check that the first screen shows
/// what the keys do. `tab` does not move to the context; on the chat tab it moves the keys onto a
/// waiting question. Plain `alt+tab` does.
///
/// note: the tab keys are here rather than along the bottom of the chat tab, where they took the
/// footer's place on every screen for something read once.
pub const GREETING: &str = "ctrl+t or alt+tab opens the next tab · alt+1 / 2 / 3 / 4 opens chat / \
                            context / trace / permissions · ctrl+p shows the next request · \
                            F1 lists the keys";

/// How many rows a set of lines occupies once soft-wrapped to `width`.
///
/// note: `TextArea` wraps when it draws and does not report how many rows that came to, so this
/// counts them the same way `WrapMode::WordOrGlyph` breaks them: fill up to the width, go back to
/// the last space if there is one, and split a word that would not fit on a line of its own. It has
/// to agree with the widget about the *number* of rows rather than about where the breaks fall, and
/// the screen test holds the two together: if they ever disagree, the box is the wrong size and
/// text goes missing.
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
/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // editing an item needs more room than composing a message. The tab underneath can afford to
    // give it up because the item being edited is on it
    let most = match app.editing.is_some() {
        true => 16,
        false => 8,
    };
    // the wrapped height, not the number of lines typed. With soft wrapping, one long line is
    // several rows; a box sized to the line count would show the last row and hide the rest
    let inner = frame.area().width.saturating_sub(2) as usize;
    // the prompt belongs to the conversation and to an edit wherever it started. The other three
    // tabs are read and operated rather than typed into; a box there was a mode, where every letter
    // was a key or a character depending on the focus
    let prompted = app.prompted();
    // the search box goes in the prompt's place, which is empty on panes that have a search. One
    // row: what is being typed is a phrase, not a message, and a box that grew would take rows from
    // the thing it is filtering
    let searching = app.search.is_some();
    let input_height = match (prompted, searching) {
        (_, true) => 1,
        (true, false) => (wrapped_rows(app.input.lines(), inner) as u16).clamp(1, most) + 2,
        (false, false) => 0,
    };
    // in the prompt's place rather than laid over the middle of the screen, so that everything the
    // question is about stays reachable while it waits: a question covering the context tab is a
    // question about item 22 asked with the list of items underneath it
    let height = frame.area().height;
    let wanted = match app.tab == Tab::Chat {
        true => question_rows(app, inner),
        false => 0,
    };
    // the question may have everything except the status line and enough of the conversation to see
    // what the question is about. If that is not enough to answer with, the conversation gives way
    // too. `App::prompted` is false while a question waits, so `input_height` is already nought
    // here. The box holding the keys is never the one that goes
    let mut question = 0;
    if wanted != 0 {
        question = wanted.min(height.saturating_sub(1 + MIN_CHAT));
        if question < MIN_QUESTION.min(wanted) {
            question = wanted.min(height.saturating_sub(BESIDES));
        }
    }
    let [body, asked, input, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(question),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // one projection a frame, shared by the three places that report on it. Each asking for its own
    // would be three walks of the context per redraw, and three answers that could disagree about
    // the same request
    let going = app.going();
    // One budget a frame, for the same reason: the corner and the context tab's own line are two
    // readings of the request that is about to go, and two walks could disagree about it
    let budget = app.kernel.budget();
    let scrolled = draw_body(frame, app, &going, &budget, body);
    if question != 0 {
        // what the frame could really scroll to is what the keys work against from here on, the
        // same write-back the overlay does
        app.question_scroll = draw_question(frame, app, asked);
    }
    match (searching, prompted) {
        // the rows the context tab just drew are the ones the search found. Filtering the whole
        // context again to count them doubled the cost of every search keystroke
        (true, _) => draw_search(
            frame,
            app,
            (app.tab == Tab::Context).then_some(scrolled.total),
            input,
        ),
        (false, true) => draw_input(frame, app, input),
        (false, false) => {}
    }
    draw_status(frame, app, &going, &budget, status);

    if app.overlay.is_some() {
        // what the frame could actually scroll to is what the keys work against from here on.
        // Without the write-back, `scroll` counts presses rather than rows: ten pages down past
        // the end of a short body is ten pages back up before anything moves
        let at = draw_overlay(frame, app);
        if let Some(Overlay::Text { scroll, .. }) = &mut app.overlay {
            *scroll = at;
        }
    }
}

// ----------------------------------------------------------------------------------- the frame

/// Text that is secondary but still meant to be read.
///
/// note: `Gray`, not `DarkGray`. `DarkGray` is the terminal's bright black and sits a shade off the
/// background on many themes. Everything that is not the answer - why an item is not being sent,
/// what an event says, the whole trace - is meant to be read, and so is drawn in this.
pub(super) fn quiet() -> Style {
    Style::default().fg(Color::Gray)
}

/// Chrome that is supposed to stay out of the way: borders, rules, the rule down a code block.
///
/// note: this one is `DarkGray`, and it is the only thing that should be. It draws lines, not
/// words.
pub(super) fn faint() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The window: a strip of tabs, and whichever one is open filling everything under it.
///
/// note: the border is the frame of the open window and says so, in the same colour the open tab's
/// name is written in on the strip above it. It does not go faint when the keys are off the tab's
/// body, because on the chat tab they never are on it: `Focus::Body` there means the pinned
/// question, which has a box of its own. The tab a session is mostly spent on would be the one
/// window that could never look open, and "unfocused window" reads as "this is not where you are".
///
/// note: what has the keys *within* the window is said by the box that has them - the prompt and
/// the question take the accent colour, and grey or red when the keys are elsewhere - and, on the
/// two list tabs, by the selected row, which is reversed under the keys and underlined without
/// them. Both sit next to the thing they describe, which a border a whole window away does not.
fn draw_body(
    frame: &mut Frame,
    app: &mut App,
    going: &Going,
    budget: &Budget,
    area: Rect,
) -> Scrolled {
    // the chat tab has a second thing the keys can be on, and only while a question is pinned
    // there; on the other three, `Focus::Body` is the only place they ever are
    let asked = app.asked().is_some() || app.reached().is_some();

    let mut strip = Vec::new();
    for tab in Tab::ALL {
        if !strip.is_empty() {
            strip.push(Span::styled("│", faint()));
        }
        // note: `chat` goes red while a tool is waiting to be told whether it may run, or a running
        // command whether it may reach the network. The question is pinned there rather than laid
        // over the screen, so it is possible to walk away from it and look at what it is about, and
        // something has to say, from the other three tabs, that the session is waiting for somebody
        // to walk back
        strip.push(Span::styled(
            format!(" {} ", tab.name()),
            match (tab == app.tab, tab == Tab::Chat && asked) {
                (_, true) => Style::default().fg(Color::Red).bold(),
                (true, _) => Style::default().fg(app.accent).bold(),
                _ => quiet(),
            },
        ));
    }

    let edge = Style::default().fg(app.accent);
    let block = Block::bordered()
        .title(Line::from(strip))
        .title_bottom(
            Line::styled(
                fitted(
                    &footer(app, going, budget),
                    area.width.saturating_sub(2).into(),
                ),
                quiet(),
            )
            .right_aligned(),
        )
        .border_style(edge);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let scrolled = match app.tab {
        Tab::Chat => draw_chat(frame, app, going, inner),
        Tab::Context => draw_context(frame, app, going, inner),
        Tab::Trace => draw_trace(frame, app, inner),
        Tab::Permissions => draw_permissions(frame, app, inner),
    };
    scrollbar(frame, area, edge, scrolled);

    scrolled
}

/// How far through its content a tab is, and the rows it drew that content in.
#[derive(Clone, Copy, Default)]
pub(super) struct Scrolled {
    /// The first row on screen, counted from the top of the content.
    position: usize,
    /// How many rows of content there are in all.
    total: usize,
    /// Where the content was drawn; the bar lines up with these rows and no others.
    area: Rect,
}

/// The rows of a scrolled pane that fit in it, from `at` down.
///
/// note: the rows themselves rather than all of them and an offset, because the offset
/// `Paragraph::scroll` takes is a `u16`. Past 65,535 rows it wrapped, and a chat following its
/// bottom showed rows from near its beginning.
pub(super) fn in_view<T>(rows: Vec<T>, at: usize, area: Rect) -> Vec<T> {
    rows.into_iter().skip(at).take(area.height.into()).collect()
}

/// Draws a scrollbar down the window's right-hand border, when there is anything to scroll.
///
/// note: on the border rather than in a column of its own. Giving up a column makes a tab one
/// character narrower for the whole session to show something true only sometimes; the table tabs
/// use that column for what the model reads of an item. The track is the border character, so an
/// unscrollable window looks unchanged.
///
/// note: it lines up with the content rather than the window. The context and permissions tabs use
/// their first row for a header, so a bar starting above it would be off by one for the whole list.
pub(super) fn scrollbar(frame: &mut Frame, window: Rect, border: Style, scrolled: Scrolled) {
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
    // note: `content_length` is the number of scroll positions, not rows. Ratatui places the thumb
    // over `0..content_length` and adds the viewport at the far end, so passing the row count
    // places the last position a page below the last scrollable position and the thumb stops short
    // of the bottom. Every tab stops at the last full page, so the positions are
    // `total - viewport + 1`, and the thumb reaches the last row when the content does.
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

/// What the open tab has to say about itself, along the bottom, the most important first.
pub(super) fn footer(app: &App, going: &Going, budget: &Budget) -> Vec<String> {
    match app.tab {
        // note: a conversation scrolled back stays where it was left, so this line says there is
        // more underneath and how to reach it. Without it, "sticky" reads as "stuck": the window
        // stops moving and nothing explains why. Following, it says nothing: the tab keys are in
        // the greeting and in F1
        //
        // note: with a `~` while some of the lines below are estimated, which they are for the
        // first frames of a long session or after a resize; the chat draws only what is on screen
        Tab::Chat if app.follow => Vec::new(),
        Tab::Chat => vec![
            match app.rendered.saturating_sub(app.viewport + app.scroll) {
                0 => "ctrl+e follows the newest".to_owned(),
                n => format!(
                    "{}{} line(s) below · ctrl+e follows them",
                    if app.estimated { "~" } else { "" },
                    thousands(n)
                ),
            },
        ],
        Tab::Context => {
            // counted by whether the model is shown what the item says. An elided item is in the
            // request but not shown; calling it "going" would be more misleading
            let items = app.kernel.items();
            let out = items
                .iter()
                .filter(|item| !going.sends_content(item))
                .count();
            let elided = items.iter().filter(|item| item.state.is_elided()).count();
            let held = plural(items.len(), "item");
            let counted = match (out, elided) {
                (0, _) => format!("{held}, all going"),
                (n, 0) => format!("{held}, {n} not going"),
                (n, e) => format!("{held}, {n} not going, {e} elided"),
            };

            // note: the figure to act on, on the tab where acting happens. The corner says the
            // request is over the limit and turns red, but has no room to say by how much; the
            // difference between "over" and "over by two thousand" is reading forty rows versus
            // removing one of them. Not shown under the limit, where it is not needed
            match budget
                .limit
                .map(|limit| budget.used().saturating_sub(limit))
                .filter(|over| *over != 0)
            {
                Some(over) => vec![counted, format!("~{} over the limit", thousands(over))],
                None => vec![counted],
            }
        }
        // the pane keeps the last few hundred; the log keeps everything, and `/save` writes it
        Tab::Trace => vec![
            format!("{} events", app.trace.len()),
            "/save keeps them all".to_owned(),
        ],
        // note: the caveat comes first because it overrides the rows. A registered shell that is
        // not refused can read, write and reach the network whatever the other rows answer; a tab
        // listing five verdicts and saying nothing about it would report four restrictions that are
        // not there.
        //
        // note: the count is of what is not listed. The tab shows decisions; they are not the whole
        // policy
        Tab::Permissions => {
            let mut parts = Vec::new();
            if let Some(line) = app.confinement() {
                parts.push(line);
            }
            match app.undecided() {
                0 => {}
                n => parts.push(format!("{n} more it will ask about")),
            }
            // note: the same keys whatever else is on the line, with or without a sandbox line
            // beside them. Not named when nothing is decided yet and there is no row for them to
            // act on
            if !app.permissions().is_empty() {
                parts.push("space cycles · a allow · n never · r ask again".to_owned());
            }

            parts
        }
    }
}

/// The footer's parts that fit in a width, padded by a space either side.
///
/// note: whole parts are dropped from the end because the line is right-aligned. A title too wide
/// for its border is cut from the left, where the part that matters most is. A first part that does
/// not fit by itself is shortened from its end.
fn fitted(parts: &[String], width: usize) -> String {
    let mut line = String::new();
    for part in parts {
        let longer = match line.is_empty() {
            true => part.clone(),
            false => format!("{line} · {part}"),
        };
        if columns(&longer) + 2 > width {
            break;
        }
        line = longer;
    }
    match (line.is_empty(), parts.first()) {
        (true, Some(first)) => format!(" {} ", clip(first, width.saturating_sub(2))),
        (true, None) => String::new(),
        (false, _) => format!(" {line} "),
    }
}

// ------------------------------------------------------------------------------------ the prompt

/// The prompt, which is the one thing on the chat tab the keys can be on.
///
/// note: the accent when the keys are on it is the same colour the pinned question wears, because
/// both are boxes that can hold keys and the colour shows which one holds them. Not white: against
/// grey it is a difference in brightness rather than hue, the weaker of the two signals and the
/// first to go on a pale theme.
///
/// note: an edit is the accent only while the keys are on it, like a message. The same colour doing
/// a second job would leave two such boxes on screen at once, with the item being edited on the tab
/// underneath. The title says this box is not composing a message; the colour says the one thing it
/// says everywhere else.
fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Input;
    // the same box does two jobs, so it has to say which one it is doing: typing into it
    // ordinarily sends a message, and typing into it while an item is being edited rewrites what
    // the model will read. Either way the title says how to reach it when the keys are elsewhere,
    // the way the question's does
    let title = match (app.editing, focused) {
        (Some(id), true) => format!(" editing [{id}] · enter commits · esc cancels "),
        (Some(id), false) => format!(" editing [{id}] · tab "),
        (None, true) => " you ".to_owned(),
        (None, false) => " you · tab ".to_owned(),
    };
    let colour = match focused {
        true => app.accent,
        false => Color::Gray,
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

/// The `/` box: what is being searched for, and how much of the pane is left.
///
/// note: the count is the point of the line as much as the query is. A filter that found nothing
/// and a filter that found everything look identical from a pane you have scrolled halfway down,
/// and the number is what tells them apart without scrolling back.
fn draw_search(frame: &mut Frame, app: &mut App, found: Option<usize>, area: Rect) {
    let Some(search) = &app.search else {
        return;
    };

    let found = match (found, app.tab) {
        (Some(found), _) => found,
        (None, Tab::Trace) => app.traced().len(),
        (None, _) => app.listed().len(),
    };
    let of = match app.tab {
        Tab::Trace => app.trace.len(),
        _ => app.kernel.items().len(),
    };

    // where the next character goes, not the end of the query any more. `left` and `right` move it,
    // so the cursor is drawn between the two halves rather than after both
    let (before, after) = search.parts();
    // note: a block over the character under the cursor, not a bar between two characters. A bar
    // glyph is its own cell with only its left edge painted, so it reads as a cursor with a space
    // after it. Past the end there is no character; the block is the cell the next one will take
    let mut rest = after.chars();
    let under = rest.next().map_or_else(|| " ".to_owned(), String::from);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::raw(before.to_owned()),
            // the box has the keys, and nothing else on the screen should look like it does
            Span::styled(
                under,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::REVERSED),
            ),
            Span::raw(rest.as_str().to_owned()),
            Span::styled(format!("  {found} of {of} · esc clears"), quiet()),
        ])),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, going: &Going, budget: &Budget, area: Rect) {
    let dim = quiet();
    let mut spans = match app.busy {
        true => {
            let mut working = vec![Span::styled(
                format!(" {} ", state_name(&app.kernel)),
                Style::default().fg(Color::Yellow),
            )];
            working.extend(waiting(app.since.elapsed()));
            working.push(Span::raw(" "));

            working
        }
        false => vec![Span::styled(format!(" {} ", state_name(&app.kernel)), dim)],
    };

    let mut add = |text: String, style: Style| {
        spans.push(Span::styled("· ", dim));
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    };

    // the address as well as the name, because the same name at a different address is a different
    // model: without it a session pointed at a local ollama looks exactly like one talking to
    // OpenRouter. The host alone: the rest of the URL is `/endpoint`'s to show, and there is no
    // room for it here
    //
    // note: a session with no model gets a placeholder in the model's place rather than a corner
    // with one fewer thing in it, because the gap is the one thing nobody can act on: a corner
    // that leaves the model out reads as a corner that has not caught up yet. The address is
    // still shown: it is settled, and it is what `/models` is about to list
    match app.kernel.model_info() {
        Some(info) => add(format!("{} @ {}", info.model, app.provider.host()), dim),
        None => add(
            format!("no model @ {}", app.provider.host()),
            Style::default().fg(Color::Yellow),
        ),
    }

    // The next request's cost with the message being typed, taken from the last request's real cost
    // where there is one. The `~` stays either way: both are predictions. An anchored figure is out
    // by a few percent of what changed since the last request, not of the whole context, and a
    // draft moving the figure is only worth showing at that accuracy. See `App::anchored` and
    // `App::expected` for why every other view shows the same figure
    let draft = app.drafted();
    let next = app.expected(going, budget) + draft;
    let used = thousands(next);
    let fraction = budget
        .limit
        .filter(|limit| *limit != 0)
        .map(|limit| next as f64 / limit as f64);
    add(
        match (fraction, budget.limit) {
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
        match fraction {
            Some(fraction) if fraction >= 0.9 => Style::default().fg(Color::Red),
            Some(fraction) if fraction >= 0.7 => Style::default().fg(Color::Yellow),
            Some(_) => Style::default().fg(Color::Green),
            None => dim,
        },
    );
    // note: past the limit the figure above stops describing the next request, and without this it
    // reads as a catastrophe that resolves itself on the next keystroke. A tool loop leaves the
    // context fat - compaction runs when a request is *built*, not when a turn ends - so the corner
    // can be red and far over the limit while the request that follows is well under it. This adds
    // the mechanism between the two, not a number.
    //
    // note: "runs", not "will fix it". Whether the pass finds anything it may take is its own
    // business - everything it wants may be pinned - so this says what is certain and leaves
    // `/budget` to say the rest.
    if fraction.is_some_and(|fraction| fraction >= 1.0) && app.kernel.compactor().is_some() {
        add("the compactor runs first".to_owned(), dim);
    }

    // and what of that figure is the thing not yet sent, because a number that moves as
    // somebody types is worth reading only if it says which part is theirs
    if draft != 0 {
        add(format!("{} of it typed", thousands(draft)), dim);
    }

    // the `~` above is not decoration: that figure is an estimate from a counter without the
    // model's tokenizer. This one is what the provider charged for; the two side by side is the
    // only reason either can be trusted. `/budget` has the whole story
    if let Some(reported) = budget.reported.and_then(|usage| usage.input_tokens) {
        add(format!("{} really", thousands(reported as usize)), dim);
    }

    // note: counted over the request, not the states, so this, `/budget`, and the `held` column are
    // one answer. `tokens_withheld` misses an item the projector repaired away, which holds as much
    // as any excluded one
    let (withheld, _) = app.withheld(going);
    if withheld != 0 {
        add(format!("{} held back", thousands(withheld)), dim);
    }
    // note: `esc stops it` and nothing in its place when idle. `F1` appears in `GREETING` on the
    // first screen and in every `there is no /x`; this line's right end is the first thing a narrow
    // terminal loses. This is the only place a running turn shows how to stop it, and it is on
    // every tab, so the chat's footer does not repeat it
    if app.busy {
        add("esc stops it".to_owned(), dim);
    }

    // the line is drawn without wrapping, so anything past the right edge is simply gone - and
    // what sits at that end is the figures, which are worth more than the address. The address is
    // what gives way, and `generativelanguage.googleapis.com` is a third of a 100-column line
    let line = Line::from(spans);
    let width: usize = line.spans.iter().map(|span| columns(&span.content)).sum();
    let over = width.saturating_sub(area.width as usize);
    let line = match over {
        0 => line,
        _ => Line::from(shrink_address(line.spans, over)),
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// The least of an address worth showing. Below this it goes entirely: `gen…` names nothing, and
/// the columns it was costing are better spent on the figures to its right.
const HOST_FLOOR: usize = 8;

/// The least of a model's name worth showing, more than a host's: the name is what the line is
/// about, and `…h` tells nothing about what a session is talking to.
const MODEL_FLOOR: usize = 12;

/// The same spans with the `model @ host` shortened to claw back `over` columns.
///
/// note: a ladder, because more than one thing here can give way and they are not worth the same.
/// The host goes first, then the model's vendor prefix, then the model itself from the left, and
/// the figures at the right end, which all of this protects, never do. The host alone is not
/// enough: a name like `dots-studio/dots-3-note-preview:free` is ordinary on OpenRouter, this
/// program's default endpoint, and a line carrying one can lose its last figure off the right edge
/// with the address already gone.
fn shrink_address(spans: Vec<Span<'static>>, over: usize) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let style = span.style;
            let Some((model, host)) = span.content.split_once(" @ ") else {
                return span;
            };

            // one column of the saving goes on the ellipsis that says it was shortened
            let keep = columns(host).saturating_sub(over + 1);
            if keep >= HOST_FLOOR {
                // from the right: the leftmost label distinguishes an endpoint -
                // `generativelanguage` in Google's, the resource name in an Azure deployment - and
                // the rest is a domain shared with everything else the vendor runs
                return Span::styled(
                    format!("{model} @ {}…", &host[..prefix_within(host, keep)]),
                    style,
                );
            }

            // the address is gone; whatever is still over has to come out of the name
            let short = over.saturating_sub(columns(&format!(" @ {host}")));

            Span::styled(shrink_model(model, short), style)
        })
        .collect()
}

/// A model's name with `over` columns taken out of it, or as near as is still worth reading.
///
/// note: the opposite end from a host, because the distinguishing part is at the opposite end.
/// `dots-studio/` is shared with everything that vendor publishes and `dots-3-note-preview:free` is
/// the model, so the prefix goes first and the ellipsis takes from the left.
fn shrink_model(model: &str, over: usize) -> String {
    if over == 0 {
        return model.to_owned();
    }

    let named = model.split_once('/').map_or(model, |(_, rest)| rest);
    let saved = columns(model) - columns(named);
    let Some(short) = over.checked_sub(saved).filter(|short| *short != 0) else {
        return named.to_owned();
    };

    // one column of the saving goes on the ellipsis, as with a host
    let keep = columns(named).saturating_sub(short + 1);
    match keep >= MODEL_FLOOR {
        true => format!("…{}", &named[named.len() - suffix_within(named, keep)..]),
        // below the floor there is nothing to gain by saying half of it; the line is drawn as it
        // is and the terminal clips what does not fit
        false => named.to_owned(),
    }
}

/// How long each of the three dots stays lit.
const BLINK: Duration = Duration::from_millis(280);

/// How long a wait has to last before the marker also says how long it has been.
const AT_LENGTH: Duration = Duration::from_secs(5);

/// Three dots, one lit and moving; after a while, the elapsed seconds.
///
/// note: the lit dot comes from the clock, not a frame counter, so it moves at the same speed
/// whatever the screen is doing and stops moving if the screen stops being drawn. `asking` is the
/// same whether a request is in flight or the program is wedged; the moving dot tells them apart.
///
/// note: the seconds only after five of them. A two-second turn should not leave a number
/// flickering on the line, and a ninety-second turn should not make someone guess how long they
/// have waited.
pub(super) fn waiting(elapsed: Duration) -> Vec<Span<'static>> {
    let lit = (elapsed.as_millis() / BLINK.as_millis()) as usize % 3;
    let mut spans: Vec<Span<'static>> = (0..3)
        .map(|n| {
            Span::styled(
                "•",
                match n == lit {
                    true => Style::default().fg(Color::Yellow).bold(),
                    false => faint(),
                },
            )
        })
        .collect();

    if elapsed >= AT_LENGTH {
        spans.push(Span::styled(format!(" {}s", elapsed.as_secs()), quiet()));
    }

    spans
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
