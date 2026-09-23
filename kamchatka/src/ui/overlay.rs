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
    app::{App, Focus, Overlay, Page, Tab, text::thousands},
    ui::text::{refit, wrapped},
};

use super::{Scrolled, faint, markdown::command, quiet, scrollbar};

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
/// bound on it. Sized as one block, a `revise` carrying a long replacement text would push the
/// answers off the bottom and leave the panel with no way to read the rest and no way to see that
/// `y` was still a key. The answers are pinned to the bottom, and the arguments scroll between
/// them and the header.
///
/// note: the header is styled lines and the other two are strings, because the header is the one
/// region with a colour in it - the advisor's rating - and it is also the one that is pinned above
/// the scroll, which is where a rating has to be: a warning that can be scrolled out of view is a
/// warning nobody is obliged to have seen.
fn question_parts(
    app: &App,
    columns: usize,
    cut: bool,
) -> Option<(Vec<Line<'static>>, Vec<Line<'static>>, Vec<String>)> {
    // a tool's question comes first where both are somehow open, because it is the one holding a
    // turn still. A compaction holds nothing: it is somebody's own command, waiting on them
    let Some(request) = app.asked() else {
        return compaction_parts(app, columns, cut);
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

    let mut head: Vec<Line<'static>> = wrapped(
        &format!("{} wants: {}", request.tool, judged.join(", ")),
        columns,
        "",
    )
    .into_iter()
    .map(Line::raw)
    .collect();
    head.extend(rating(app, &request, columns));
    // and why the list above is every operation the tool has, where it is because the call did
    // not say which one it wanted. It goes in the pinned region with the rating, and for the same
    // reason: it is the part that explains the question, and a line explaining the question is no
    // use below the fold of the arguments it is about
    if let Some(widened) = app.widened(&request) {
        head.extend(
            wrapped(&widened, columns, "")
                .into_iter()
                .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Yellow)))),
        );
    }
    // the one blank line under the header, whether or not there is a rating in it
    head.push(Line::default());
    // the arguments, and then what the ones naming context items actually are. The context tab is
    // a keystroke away and stays that way, and this is still worth having, because the answer is
    // usually right here
    // note: through the wrapper, because these tools take their arguments inside a `call` object
    // and the outside of one is a single field holding the whole call as a blob. What that costs
    // is exactly what this panel is for: `old` in red and `new` in green, one argument to a line
    let mut shown = readable(
        &request.tool,
        &crate::tools::ops::inner(&request.args)
            .unwrap_or(std::borrow::Cow::Borrowed(&request.args)),
        columns,
        worst(app, &request),
    );
    let about = app.about(&request);
    if !about.is_empty() {
        shown.extend(
            about
                .iter()
                .flat_map(|line| refit(&Line::raw(line.clone()), columns)),
        );
    }

    Some((
        head,
        // `readable` puts a blank line after every field, so the last of them is a row of nothing
        // at the bottom of the panel - and a row of nothing that does not fit is a panel saying
        // there is more to read and then paging down to a blank
        trimmed(shown),
        wrapped(&answers, columns, ""),
    ))
}

/// What the advisor made of the command, as a line of the header, or nothing at all.
///
/// note: the colour is what this adds. Somebody answering a question about a command has to read
/// the command either way - that is what the panel under this is for - and what a band of green,
/// yellow or red buys is the half-second before that: whether this is the ordinary `cargo test` the
/// turn has been full of, or the one call that is about to send something somewhere. It is above
/// the arguments because that is the pinned region; see `question_parts`.
///
/// note: the band and the words both come from [`Rated::shown`], and the colour is the only thing
/// decided here. `ui` does not read the score, does not know where the thresholds are, and cannot
/// disagree with the sentence beside it - the same division `Exit` is drawn under, one file along.
///
/// note: the percentage is on the line because the band it produced is not a fact about the
/// command. `Rated::shown` will not draw an unsure rating green, so an uncertain reading arrives
/// yellow - and a person who cannot tell that from a confident yellow has been told the advisor
/// was sure when it was not.
#[cfg(feature = "shell-advisor")]
fn rating(app: &App, request: &nachalnik::PermissionRequest, columns: usize) -> Vec<Line<'static>> {
    use crate::tools::Rating;

    let Some(rated) = app.rating(request) else {
        return Vec::new();
    };
    let shown = rated.shown();
    let colour = match shown {
        Rating::Reads => Color::Green,
        Rating::Changes => Color::Yellow,
        Rating::Grave => Color::Red,
    };

    refit(
        &Line::from(vec![
            Span::raw("the advisor reads this as: "),
            Span::styled(shown.said().to_owned(), Style::default().fg(colour).bold()),
            Span::styled(format!(" · {:.0}% sure", rated.confidence * 100.0), quiet()),
        ]),
        columns,
    )
}

/// The same where the advisor is not in the build: no line at all.
#[cfg(not(feature = "shell-advisor"))]
fn rating(_: &App, _: &nachalnik::PermissionRequest, _: usize) -> Vec<Line<'static>> {
    Vec::new()
}

/// Which stage of the command earned the band, where the advisor took one apart.
///
/// note: the pair with [`rating`] above, and it answers the question that one cannot. A band is
/// how bad the command is and a long `&&` chain earns its colour from one link - so the line says
/// *how bad* and this says *which part*, and the second is worth having exactly when the command
/// is too long to find it in by eye. It costs no row: what it produces is an underline under a run
/// of a command that is already on the screen.
///
/// note: `None` is every way of not having one and does not distinguish between them - no
/// advisor, a command with no joints, one the advisor declined to take apart - the same way
/// [`App::rating`] answers for the band. What it means on the screen is the same in each case:
/// nothing underlined, and a command to read as a whole.
#[cfg(feature = "shell-advisor")]
fn worst(app: &App, request: &nachalnik::PermissionRequest) -> Option<(usize, usize)> {
    app.rating(request)?.worst
}

/// The same where the advisor is not in the build.
#[cfg(not(feature = "shell-advisor"))]
fn worst(_: &App, _: &nachalnik::PermissionRequest) -> Option<(usize, usize)> {
    None
}

/// The other question: a compaction pass, listed, waiting on a `y`.
///
/// note: the list goes where a tool's arguments go, so it scrolls the same way and the panel does
/// not have to know it is reading something else. What a person needs to decide is the same shape
/// in both cases - what is about to happen, to what - and the one thing this adds is that the
/// items are named by identifier, because the answer to "not that one" is `p` on the context tab
/// and that is the number it is found by.
fn compaction_parts(
    app: &App,
    columns: usize,
    cut: bool,
) -> Option<(Vec<Line<'static>>, Vec<Line<'static>>, Vec<String>)> {
    let proposed = app.proposed.as_ref()?;

    let answers = format!(
        "{}[y] take it   [n] leave it{}",
        match app.focus == Focus::Body && app.tab == Tab::Chat {
            true => "",
            false => "[tab] puts the keys here, and then:\n",
        },
        match cut {
            true => "   pgup / pgdn for the rest",
            false => "",
        },
    );

    let head: Vec<Line<'static>> = wrapped(
        &format!(
            "compacting would take {} item(s), holding {} tokens - less what the markers cost. \
             Nothing has happened yet: `p` on the context tab keeps one out of it\n",
            proposed.count,
            thousands(proposed.holding),
        ),
        columns,
        "",
    )
    .into_iter()
    .map(Line::raw)
    .collect();
    let shown = proposed
        .rows
        .iter()
        .flat_map(|row| refit(&Line::raw(row.clone()), columns))
        .collect();

    Some((head, shown, wrapped(&answers, columns, "")))
}

/// The same lines, without the empty ones at the end.
fn trimmed(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
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

    // the accent when the keys are on it, so that "this is answerable right now" and "this is
    // waiting for you to come back" are not the same picture. The title says how to reach it, the
    // way the prompt's does when the keys are somewhere else
    //
    // note: red stays red whatever the frame is. It is the one colour here that is not saying
    // where the keys are - it is saying a tool is waiting on somebody - and a person who set the
    // frame red would otherwise have configured that distinction away
    let asking = app.focus == Focus::Body && app.tab == Tab::Chat;
    let style = match asking {
        true => Style::default().fg(app.accent),
        false => Style::default().fg(Color::Red),
    };
    let block = Block::bordered()
        .title(
            match (asking, app.proposed.is_some() && app.asked().is_none()) {
                (true, false) => " a tool wants to run ".to_owned(),
                (false, false) => " a tool wants to run · tab ".to_owned(),
                (true, true) => " what a compaction would take ".to_owned(),
                (false, true) => " what a compaction would take · tab ".to_owned(),
            },
        )
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
    // things rather than as a question and the ways of answering it. The header is separated
    // from the arguments the same way
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
    let total = args.len();
    let at = app
        .question_scroll
        .min(total.saturating_sub(middle.height as usize));

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(head), above);
    frame.render_widget(Paragraph::new(args).scroll((at as u16, 0)), middle);
    frame.render_widget(Paragraph::new(foot.join("\n")), below);

    scrollbar(
        frame,
        area,
        style,
        Scrolled {
            position: at,
            total,
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
///
/// note: `edit`'s two arguments are drawn in a diff's colours, because that is the call where two
/// blocks of near-identical text sit one above the other and the whole question is which of them
/// is on its way out. The value is coloured and the field name is not, so what is green is exactly
/// the text that will be in the file. The colours are the ones a fenced diff already gets in
/// `markdown`, and they are the terminal's own for the reason given there - this program does not
/// know what is behind them. The red here is a pair with the green and reads as one; the border's
/// red is about whether anybody is at the keys, and nothing else in the panel is either colour.
///
/// note: a shell command is drawn as code, through the same [`highlighted`] a fenced ```sh block in
/// the chat goes through, and broken at its joints by [`joints`](crate::tools::joints) first.
/// Wrapped as prose it would be folded at whatever space ran out, with the continuation back at the
/// margin - so the second half of a pipeline would sit under `cmd:` looking exactly like the next
/// argument, on the one screen whose whole job is saying what is about to run. The rule down the
/// left settles that by itself; the highlighting is what makes a quoted string legible as one thing
/// rather than as a run of flags.
///
/// note: by the tool's name as well as the field's, the way [`App::about`] picks its two out. A
/// `cmd` is a shell command *here* because `shell` is the tool that takes one, and somebody else's
/// tool with a field of that name has not said it is drawing a command line.
fn readable(
    tool: &str,
    args: &serde_json::Value,
    columns: usize,
    worst: Option<(usize, usize)>,
) -> Vec<Line<'static>> {
    let Some(fields) = args.as_object() else {
        return serde_json::to_string_pretty(args)
            .unwrap_or_default()
            .lines()
            .flat_map(|line| refit(&Line::raw(line.to_owned()), columns))
            .collect();
    };
    if fields.is_empty() {
        return vec![Line::raw("(no arguments)")];
    }

    let mut out = Vec::new();
    for (name, value) in fields {
        let written = match name.as_str() {
            "old" => Style::default().fg(Color::Red),
            "new" => Style::default().fg(Color::Green),
            _ => Style::default(),
        };
        match value {
            // before the multi-line arm, so that a command which brought its own newlines - a
            // heredoc, most often - is drawn as code too. `joints` declines to touch that one, so
            // what it gets is the rule and the colours and none of the breaking
            serde_json::Value::String(text) if tool == "shell" && name == "cmd" => {
                out.extend(refit(&Line::raw(format!("{name}:")), columns));
                out.extend(command(text, columns, worst));
            }
            serde_json::Value::String(text) if text.contains('\n') => {
                out.extend(refit(&Line::raw(format!("{name}:")), columns));
                for line in text.lines() {
                    out.extend(refit(
                        &Line::from(Span::styled(format!("  {line}"), written)),
                        columns,
                    ));
                }
            }
            serde_json::Value::String(text) => out.extend(refit(
                &Line::from(vec![
                    Span::raw(format!("{name}: ")),
                    Span::styled(text.clone(), written),
                ]),
                columns,
            )),
            other => out.extend(refit(&Line::raw(format!("{name}: {other}")), columns)),
        }
        out.push(Line::default());
    }

    out
}

/// A bordered box over the middle of the screen; returns the offset it drew at.
///
/// note: something with more than one face gets a strip of them along the top, drawn the way the
/// window's own tabs are, because it is the same gesture: `←` and `→` move between them and the
/// open one is the one in yellow. A single-page overlay - which is nearly all of them - has no
/// strip.
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

    // no taller than it has anything to say: `/budget` is a handful of lines, and a box that took
    // nine tenths of the screen to show them would be hiding the conversation for no reason
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
/// buffer, which is a panic: `F1` in a one-row window would take the whole program down and the
/// session with it, and a window is one row for as long as somebody is dragging its edge.
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
