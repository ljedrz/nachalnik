//! Drawing. Nothing here decides anything - it reads [`App`] and the kernel and puts what it
//! finds on the screen.
//!
//! note: the frame and the chrome on it - the tab bar, the footer, the prompt, the status
//! line - and `draw`, which is the one entry point. What goes inside is next door: `tabs` draws
//! the four bodies, `overlay` the panel that floats over one of them, `markdown` and `table` turn
//! what a model wrote into styled lines, and `text` measures and fits all of it. Everything but
//! [`GREETING`], [`SECTIONS`], [`everything`], [`wrapped_rows`] and `draw` itself is private to
//! this module, because a screen is not an API.
//!
//! note: this whole module is behind the `tui` feature, and the key reference lives in `help`
//! rather than here for that reason: `/help` is answered by a build with no screen, so the text a
//! command owns cannot sit behind the feature that draws. What is here is a re-export, because
//! `ui` is the path somebody may already be reaching it through - [`crate::help`] is the one that
//! works in every configuration.

use std::time::Duration;

use nachalnik::{Kernel, State};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::app::{App, Focus, Going, Overlay, Tab, text::thousands};

mod markdown;
mod overlay;
mod table;
mod tabs;
mod text;

pub use crate::help::{SECTIONS, Section, everything};
use crate::ui::{
    overlay::{BESIDES, MIN_CHAT, MIN_QUESTION, draw_overlay, draw_question, question_rows},
    tabs::{draw_chat, draw_context, draw_permissions, draw_trace},
    text::{compact, prefix_within, rows_for, suffix_within},
};

/// The first line of a session that is not being resumed.
///
/// note: `pub` for the same reason [`SECTIONS`] is: so that a test can check that what somebody is
/// told on their first screen is what the keys actually do. It used to open with `tab moves to
/// the context`, which tab has never done - on the chat tab it moves the keys onto a waiting
/// question, and there is nothing else there to move them to - and it went on to offer `ctrl+t`
/// for the trace, which is two presses away rather than one. The first sentence anybody reads was
/// wrong in both halves.
pub const GREETING: &str = "ctrl+t opens the context, and again the trace · alt+1 comes back · \
                            ctrl+p shows the next request · F1 lists the keys";

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
    // the prompt belongs to the conversation, and to an edit wherever that was started. The three
    // other tabs are read and operated rather than typed into, and a box there was a mode: every
    // letter on them was a key or a character depending on where the focus had got to
    let prompted = app.prompted();
    // the search box goes in the prompt's place, which on the panes that have a search is empty.
    // One row: what is being typed is a phrase, not a message, and a box that grew would be
    // taking rows from the very thing it is filtering
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
    // what it may have is everything except the status line and enough of the conversation to see
    // what the question is about - and if that is not enough to answer with, the conversation
    // gives way too. There is no prompt left to bargain with: `App::prompted` is false while a
    // question waits, so `input_height` is already nought here. It used to be the first thing
    // asked to give way, which meant the box holding the keys was the box that went
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

    // one projection a frame, shared by the three places that report on it. Each asking for its
    // own would be three walks of the context per redraw, and - worse - three answers that could
    // disagree about the same request
    let going = app.going();
    draw_body(frame, app, &going, body);
    if question != 0 {
        // what the frame could really scroll to is what the keys work against from here on, the
        // same write-back the overlay does
        app.question_scroll = draw_question(frame, app, asked);
    }
    match (searching, prompted) {
        (true, _) => draw_search(frame, app, input),
        (false, true) => draw_input(frame, app, input),
        (false, false) => {}
    }
    draw_status(frame, app, &going, status);

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
/// note: `Gray`, not `DarkGray`. Nearly everything on these screens that is not the answer itself
/// used to be `DarkGray` - why an item is not being sent, what an event says, the whole of the
/// trace - and `DarkGray` is the terminal's bright *black*: on a good half of the themes people
/// actually use it sits a shade off the background. That is the wrong thing to do to the column
/// this program exists for. Anything a person is meant to read is this.
pub(super) fn quiet() -> Style {
    Style::default().fg(Color::Gray)
}

/// Chrome that is supposed to stay out of the way: borders, rules, the rule down a code block.
///
/// note: this one is `DarkGray`, and it is the only thing that should be. It is drawing lines,
/// not words.
pub(super) fn faint() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The window: a strip of tabs, and whichever one is open filling everything under it.
///
/// note: the border is the frame of the open window and says so - the same yellow the open tab's
/// name is written in on the strip above it, which is the one thing it is agreeing with. It used to
/// go faint whenever the keys were not on the tab's body, and on the chat tab they never are:
/// `Focus::Body` there means the pinned question, which has a box of its own. So the tab a session
/// is mostly spent on was the one window that could never look open, while the other three lit up,
/// and "unfocused window" reads as "this is not where you are".
///
/// note: what has the keys *within* the window is said by the box that has them - the prompt and
/// the question go yellow, and grey or red when the keys are elsewhere - and, on the two list
/// tabs, by the selected row, which is reversed under the keys and underlined without them. Both
/// of those sit next to the thing they are describing, which a border a whole window away does
/// not.
fn draw_body(frame: &mut Frame, app: &mut App, going: &Going, area: Rect) {
    // the chat tab has a second thing the keys can be on, and only while a question is pinned
    // there; on the other three, `Focus::Body` is the only place they ever are
    let asked = app.asked().is_some();

    let mut strip = Vec::new();
    for tab in Tab::ALL {
        if !strip.is_empty() {
            strip.push(Span::styled("│", faint()));
        }
        // note: `chat` goes red while a tool is waiting to be told whether it may run. The
        // question is pinned there rather than laid over the screen, which is what makes it
        // possible to walk away from it and look at what it is about - so something has to say,
        // from the other three tabs, that walking back is what the session is waiting for
        strip.push(Span::styled(
            format!(" {} ", tab.name()),
            match (tab == app.tab, tab == Tab::Chat && asked) {
                (_, true) => Style::default().fg(Color::Red).bold(),
                (true, _) => Style::default().fg(Color::Yellow).bold(),
                _ => quiet(),
            },
        ));
    }

    let edge = Style::default().fg(Color::Yellow);
    let block = Block::bordered()
        .title(Line::from(strip))
        .title_bottom(Line::styled(footer(app, going), quiet()).right_aligned())
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
pub(super) fn footer(app: &App, going: &Going) -> String {
    match app.tab {
        // note: a conversation somebody has scrolled back through stays where they left it, so
        // this is the line that has to say there is more underneath - and how to get to it. Left
        // out, "sticky" reads as "stuck": the window simply stops moving and nothing accounts
        // for it
        Tab::Chat => {
            let mut parts = Vec::new();
            if !app.follow {
                parts.push(
                    match app.rendered.saturating_sub(app.viewport + app.scroll) {
                        0 => "ctrl+e follows the newest".to_owned(),
                        n => format!("{} line(s) below · ctrl+e follows them", thousands(n)),
                    },
                );
            }
            if app.busy {
                parts.push("esc stops it".to_owned());
            }
            if parts.is_empty() {
                parts.push("alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions".into());
            }

            format!(" {} ", parts.join(" · "))
        }
        Tab::Context => {
            // counted by whether the model is being shown what the item says, which is the
            // question this line is answering. An elided item is in the request and is not being
            // shown, and calling it "going" would be the more misleading of the two
            let items = app.kernel.items();
            let out = items
                .iter()
                .filter(|item| !going.sends_content(item))
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

// ------------------------------------------------------------------------------------ the prompt

/// The prompt, which is the one thing on the chat tab the keys can be on.
///
/// note: yellow when the keys are on it, which is the same yellow the pinned question wears for
/// the same reason - the two are the boxes that can hold them, and one of them holding them is
/// what the colour says. It used to be white, which against grey is a difference in brightness
/// rather than in hue: the weaker of the two signals, the first to go on a pale theme, and the
/// answer to the only question anybody asks of a prompt.
///
/// note: an edit was yellow whether the keys were on it or not, which was the same colour doing a
/// second job - and left two yellow boxes on the screen at once with the item being edited on the
/// tab underneath. What says this box is not composing a message is its title, which spells the
/// whole of it out; the colour is left to say the one thing it says everywhere else.
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
        true => Color::Yellow,
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
fn draw_search(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(search) = &app.search else {
        return;
    };

    let found = match app.tab {
        Tab::Trace => app.traced().len(),
        _ => app.listed().len(),
    };
    let of = match app.tab {
        Tab::Trace => app.trace.len(),
        _ => app.kernel.items().len(),
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::raw(search.query.clone()),
            // the block is a cursor: the box has the keys, and nothing else on the screen should
            // look like it does
            Span::styled("▏", Style::default().fg(Color::Yellow)),
            Span::styled(format!("  {found} of {of} · esc clears"), quiet()),
        ])),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, going: &Going, area: Rect) {
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
    // model - `/model` and `/seams` have said so all along, but only when asked, so a session
    // pointed at a local ollama looked exactly like one talking to OpenRouter. The host alone:
    // the rest of the URL is `/provider`'s to show, and there is no room for it here
    if let Some(info) = app.kernel.model_info() {
        add(format!("{} @ {}", info.model, app.provider.host()), dim);
    }

    let budget = app.kernel.budget();
    // what the next request would cost with the message being typed in it, taken from what the
    // last one really cost wherever there is one to take it from. The `~` stays either way -
    // both are predictions - but an anchored one is out by a few percent of what has changed
    // since the last request rather than of the whole context, and a draft moving the figure is
    // only worth showing at that accuracy. See `App::anchored`
    let draft = app.drafted();
    let next = app
        .anchored(going, &budget)
        .unwrap_or_else(|| budget.used())
        + draft;
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
    // note: past the limit the figure above stops describing the next request, and until this
    // said so it read as a catastrophe that resolved itself on the next keystroke. A tool loop
    // leaves the context fat - compaction runs when a request is *built*, not when a turn ends -
    // so the corner sat at `~16,254 tokens, 270.9% (6.0k)` in red while the request that
    // followed cost 1,100. What was missing is not a number, it is the mechanism between them.
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

    // the `~` above is not decoration: that figure is an estimate from a counter that does not
    // have the model's tokenizer. This one is what the provider charged for, and the two being
    // side by side is the only reason either can be trusted. `/budget` has the whole story
    if let Some(reported) = budget.reported.and_then(|usage| usage.input_tokens) {
        add(format!("{} really", thousands(reported as usize)), dim);
    }

    // note: counted over the request rather than over the states, so this and `/budget` and the
    // `held` column are one answer. `tokens_withheld` misses an item the projector repaired away,
    // which is holding as much as any excluded one
    let (withheld, _) = app.withheld(going);
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
        _ => Line::from(shrink_address(line.spans, over)),
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// The least of an address worth showing. Below this it goes entirely: `gen…` names nothing, and
/// the columns it was costing are better spent on the figures to its right.
const HOST_FLOOR: usize = 8;

/// The least of a model's name worth showing, which is more than a host's: the name is what the
/// line is about, and a session cannot tell what it is talking to from `…h`.
const MODEL_FLOOR: usize = 12;

/// The same spans with the `model @ host` shortened to claw back `over` columns.
///
/// note: a ladder, because there is more than one thing here that can give way and they are not
/// worth the same. The host goes first, then the model's vendor prefix, then the model itself from
/// the left - and the figures and the key at the right end, which is what all of this is protecting,
/// never do. It used to stop after the host: `dots-studio/dots-3-note-preview:free` is 36 columns
/// on OpenRouter, which is this program's default endpoint, and a line carrying one lost `F1 for
/// the keys` off the right edge at 100 columns with the address already gone.
fn shrink_address(spans: Vec<Span<'static>>, over: usize) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let style = span.style;
            let Some((model, host)) = span.content.split_once(" @ ") else {
                return span;
            };

            // one column of the saving goes on the ellipsis that says it was shortened
            let keep = Span::raw(host).width().saturating_sub(over + 1);
            if keep >= HOST_FLOOR {
                // from the right: the leftmost label is the one that distinguishes an endpoint -
                // `generativelanguage` in Google's, the resource name in an Azure deployment -
                // and the rest of it is a domain shared with everything else the vendor runs
                return Span::styled(
                    format!("{model} @ {}…", &host[..prefix_within(host, keep)]),
                    style,
                );
            }

            // the address is gone; whatever is still over has to come out of the name
            let short = over.saturating_sub(Span::raw(format!(" @ {host}")).width());

            Span::styled(shrink_model(model, short), style)
        })
        .collect()
}

/// A model's name with `over` columns taken out of it, or as near as is still worth reading.
///
/// note: the opposite end from a host, because the distinguishing part is at the opposite end.
/// `dots-studio/` is shared with everything that vendor publishes and `dots-3-note-preview:free`
/// is the model, so the prefix goes first and the ellipsis afterwards eats from the left.
fn shrink_model(model: &str, over: usize) -> String {
    if over == 0 {
        return model.to_owned();
    }

    let named = model.split_once('/').map_or(model, |(_, rest)| rest);
    let saved = Span::raw(model).width() - Span::raw(named).width();
    let Some(short) = over.checked_sub(saved).filter(|short| *short != 0) else {
        return named.to_owned();
    };

    // one column of the saving goes on the ellipsis, as with a host
    let keep = Span::raw(named).width().saturating_sub(short + 1);
    match keep >= MODEL_FLOOR {
        true => format!("…{}", &named[named.len() - suffix_within(named, keep)..]),
        // below the floor there is nothing left to say and nothing to be gained by saying half of
        // it; the line is drawn as it is and the terminal clips what does not fit
        false => named.to_owned(),
    }
}

/// How long each of the three dots stays lit.
const BLINK: Duration = Duration::from_millis(280);

/// How long a wait has to last before the marker also says how long it has been.
const AT_LENGTH: Duration = Duration::from_secs(5);

/// Three dots with one of them lit, moving; and after a while, the seconds.
///
/// note: which dot is lit comes from the clock rather than from a frame counter, so it moves at
/// the same speed whatever the screen is doing - and stops where it is if the screen stops being
/// drawn at all. That is the point of it: `asking` on its own is the same word whether a request
/// is in flight or the program is wedged, and the two were indistinguishable.
///
/// note: the seconds only after five of them. A turn that answers in two should not leave a
/// number flickering on the line, and one that has been going for ninety should not make somebody
/// guess at how long they have been waiting - which is the question the marker raises and cannot
/// answer on its own.
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
