//! The panel that floats over a tab, and the question that takes the prompt's place.
//!
//! note: an overlay is drawn over whichever tab is open rather than instead of it, because what
//! it holds is nearly always *about* something on the tab underneath - the whole of an item, the
//! request that is about to go out, the answer a tool gave. A permission question is the one
//! thing here that is not a panel: it stands where the prompt does, since answering it is the
//! only thing to be typed at that moment.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::{
    app::{App, Focus, Overlay, Page, Tab},
    ui::text::wrapped,
};

use super::{Scrolled, faint, quiet, scrollbar};

/// Whatever is on top of everything else.
///
/// Returns how far down it really is, which is not always how far down it was asked to be.
pub(super) fn draw_overlay(frame: &mut Frame, app: &App) -> usize {
    match &app.overlay {
        Some(Overlay::Text {
            title,
            pages,
            page,
            scroll,
        }) => panel(frame, &format!(" {title} "), pages, *page, *scroll, 100, 90),
        None => 0,
    }
}

/// What a waiting question is made of, at the width it will be drawn at.
///
/// note: three regions rather than one paragraph, because the arguments are the only part with no
/// bound on it. Sized as one block, an `amend` carrying eighty lines of replacement text pushed
/// the answers off the bottom and left the panel with no way to read the rest and no way to see
/// that `y` was still a key - the question was unanswerable by anything except a guess. The
/// answers are pinned to the bottom, and the arguments scroll between them and the header.
fn question_parts(
    app: &App,
    columns: usize,
    cut: bool,
) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let request = app.asked()?;
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
    // second. The `pgup / pgdn` joins them when there are arguments below the fold, rather than
    // going on a line of its own: a line of its own costs the arguments two rows on the screen
    // that made it necessary in the first place
    //
    // note: and a line above them, until the keys are here, because until then none of them does
    // anything. Listing `[y] once` beside a `y` that is being swallowed is the panel promising a
    // key it has not got - and the swallowing is deliberate, so the honest thing is to name the
    // one key that works. It goes when `tab` is pressed, which is also the moment the options
    // start being true
    let answers = format!(
        "{}[y] once   [a] always, for {}   [n] no\n[i] the exact JSON   [d] {}{}{}",
        // the same reading `draw_question` colours the border by, so the panel cannot be counted
        // one way and drawn the other
        match app.focus == Focus::Body && app.tab == Tab::Chat {
            true => "",
            false => "[tab] puts the keys here, and then:\n",
        },
        judged.join(" and "),
        match waiting > 1 {
            true => "drop them all",
            false => "drop it",
        },
        match cut {
            true => "   pgup / pgdn for the rest",
            false => "",
        },
        match waiting > 1 {
            true => format!("\n\n{} more after this one", waiting - 1),
            false => String::new(),
        }
    );

    let head = wrapped(
        &format!("{} wants: {}\n", request.tool, judged.join(", ")),
        columns,
        "",
    );
    // the arguments, and then what the ones naming context items actually are. This used to be the
    // only way to know: the question was a box over the middle of the screen, so a question about
    // eliding item 22 was unanswerable while the thing asking it covered the list saying what 22
    // is. It is a convenience now - the context tab is a keystroke away and stays that way - and
    // still worth having, because the answer is usually right here
    let mut shown = readable(&request.args);
    let about = app.about(&request);
    if !about.is_empty() {
        shown.push_str(&format!("{}\n", about.join("\n")));
    }

    Some((
        head,
        // `readable` puts a blank line after every field, so the last of them is a row of nothing
        // at the bottom of the panel - and a row of nothing that does not fit is a panel saying
        // there is more to read and then paging down to a blank
        trimmed(wrapped(&shown, columns, "")),
        wrapped(&answers, columns, ""),
    ))
}

/// The same lines, without the empty ones at the end.
fn trimmed(mut lines: Vec<String>) -> Vec<String> {
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines
}

/// What a question costs on top of the lines it holds: a border top and bottom, and the blank row
/// between the arguments and the answers.
///
/// note: named because two places have to agree about it - the height the panel asks for, and the
/// reading of whether the arguments had to be cut, which is what puts `pgup / pgdn` on the answers.
/// Disagreeing by one row is a panel that says there is more to read when there is not.
pub(super) const BORDERS_AND_GAP: u16 = 3;

/// The fewest rows a question is drawn in, whatever a short screen would rather give it: two of
/// border, the header and the blank under it, a row of arguments and the two the answers are on.
pub(super) const MIN_QUESTION: u16 = 7;

/// What has to be left over when a question is taking the last of the screen: a row of the
/// conversation, and the status line.
pub(super) const BESIDES: u16 = 2;

/// How much of the conversation a question leaves alone while there is any choice about it. The
/// question is about something that was said up there, which is the whole reason it is pinned
/// below it rather than laid over it.
pub(super) const MIN_CHAT: u16 = 5;

/// How many rows the pinned question would like, or `0` when nothing is being asked. What it
/// actually gets is [`draw`]'s to decide, and is less on a screen with no room for it.
pub(super) fn question_rows(app: &App, columns: usize) -> u16 {
    let Some((head, args, foot)) = question_parts(app, columns.saturating_sub(2), false) else {
        return 0;
    };

    (head.len() + args.len() + foot.len()) as u16 + BORDERS_AND_GAP
}

/// A tool is waiting to be told whether it may run, in the prompt's place.
///
/// Returns the offset the arguments were really drawn at.
pub(super) fn draw_question(frame: &mut Frame, app: &App, area: Rect) -> usize {
    let columns = area.width.saturating_sub(4) as usize;
    let Some((head, args, foot)) = question_parts(app, columns, false) else {
        return 0;
    };

    // yellow when the keys are on it, so that "this is answerable right now" and "this is waiting
    // for you to come back" are not the same picture. The title says how to reach it, the way the
    // prompt's does when the keys are somewhere else
    let asking = app.focus == Focus::Body && app.tab == Tab::Chat;
    let style = match asking {
        true => Style::default().fg(Color::Yellow),
        false => Style::default().fg(Color::Red),
    };
    let block = Block::bordered()
        .title(match asking {
            true => " a tool wants to run ".to_owned(),
            false => " a tool wants to run · tab ".to_owned(),
        })
        .border_style(style)
        .padding(ratatui::widgets::Padding::horizontal(1));
    let inner = block.inner(area);

    // the answers get their rows first and the header what is left over, because a panel too small
    // for both is still answerable and is not still readable; the arguments get the remainder,
    // and are the only region that can be asked to show less than it holds
    let cut = (head.len() + args.len() + foot.len()) as u16 + BORDERS_AND_GAP > area.height;
    let foot = match cut {
        true => question_parts(app, columns, true).map_or(foot, |(_, _, it)| it),
        false => foot,
    };
    let bottom = (foot.len() as u16).min(inner.height);
    let top = (head.len() as u16).min(inner.height - bottom);
    // a blank row between what the tool was asked to do and the keys that answer for it. Without
    // it `path: /etc/hosts` and `[y] once` sit on consecutive rows and read as one list of six
    // things rather than as a question and the ways of answering it - and the header is already
    // separated from the arguments this way, so the answers were the odd ones out
    //
    // note: a row of the layout rather than a line of the foot, which is what makes it the first
    // thing to go. In the foot it would be the top line of the one region that gets its rows
    // before anything else, so a panel with a single row to spare would have spent it on a blank
    // and pushed `[y] once` off the bottom. Here it is taken only when the arguments still have a
    // row of their own left afterwards
    let gap = u16::from(inner.height > top + bottom + 1);
    let [above, middle, _, below] = Layout::vertical([
        Constraint::Length(top),
        Constraint::Length(inner.height - top - bottom - gap),
        Constraint::Length(gap),
        Constraint::Length(bottom),
    ])
    .areas(inner);
    let at = app
        .question_scroll
        .min(args.len().saturating_sub(middle.height as usize));

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(head.join("\n")), above);
    frame.render_widget(
        Paragraph::new(args.join("\n")).scroll((at as u16, 0)),
        middle,
    );
    frame.render_widget(Paragraph::new(foot.join("\n")), below);

    scrollbar(
        frame,
        area,
        style,
        Scrolled {
            position: at,
            total: args.len(),
            area: middle,
        },
    );

    at
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

/// A bordered box over the middle of the screen; returns the offset it drew at.
///
/// note: something with more than one face gets a strip of them along the top, drawn the way the
/// window's own tabs are, because it is the same gesture: `←` and `→` move between them and the
/// open one is the one in yellow. A single-page overlay - which is nearly all of them - looks
/// exactly as it did before, strip and all absent.
fn panel(
    frame: &mut Frame,
    title: &str,
    pages: &[Page],
    page: usize,
    scroll: usize,
    columns: u16,
    percent: u16,
) -> usize {
    let Some(showing) = pages.get(page).or_else(|| pages.first()) else {
        return 0;
    };
    // the strip, and the blank line under it that keeps it from reading as the first line of what
    // it is labelling
    let strip = match pages.len() > 1 {
        true => 2,
        false => 0,
    };

    // no taller than it has anything to say: `/budget` is six lines, and a box that took nine
    // tenths of the screen to show them would be hiding the conversation for no reason
    let wanted =
        wrapped(&showing.body, columns.saturating_sub(4) as usize, "").len() as u16 + 2 + strip;
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
    let outer = block.inner(area);
    let [above, inner] = Layout::vertical([
        Constraint::Length(strip.min(outer.height)),
        Constraint::Min(0),
    ])
    .areas(outer);

    let lines = wrapped(&showing.body, inner.width as usize, "");
    let at = scroll.min(lines.len().saturating_sub(inner.height as usize));
    let footer = format!(
        "{} {}–{} of {} · any key closes ",
        match pages.len() > 1 {
            true => " ← → the pages ·",
            false => "",
        },
        at + 1,
        (at + inner.height as usize).min(lines.len()),
        lines.len()
    );

    let total = lines.len();
    frame.render_widget(block.title_bottom(footer), area);
    if strip > 0 {
        frame.render_widget(Paragraph::new(Line::from(tabs(pages, page))), above);
    }
    frame.render_widget(
        Paragraph::new(lines.join("\n")).scroll((at as u16, 0)),
        inner,
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

    at
}

/// The names of an overlay's faces, with the open one marked.
fn tabs(pages: &[Page], page: usize) -> Vec<Span<'static>> {
    let mut strip = Vec::with_capacity(pages.len() * 2);
    for (n, of) in pages.iter().enumerate() {
        if n > 0 {
            strip.push(Span::styled("│", faint()));
        }
        strip.push(Span::styled(
            format!(" {} ", of.name),
            match n == page {
                true => Style::default().fg(Color::Yellow).bold(),
                false => quiet(),
            },
        ));
    }

    strip
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
