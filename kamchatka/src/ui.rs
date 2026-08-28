//! Drawing. Nothing here decides anything - it reads [`App`] and the kernel and puts what it
//! finds on the screen.
//!
//! note: The pane on the right is the point of this program. Every other terminal agent in the
//! world can show you a conversation; what this one shows beside it is the *context* - every
//! item, what it costs, and whether it is going into the next request - because in this runtime
//! that is a list of ordinary values rather than something the harness keeps to itself.

use nachalnik::{ContextState, Kernel, State};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, Paragraph},
};

use crate::app::{App, Focus, Overlay, Speaker};

/// The width the context pane wants.
const SIDE: u16 = 38;

/// What the keys do, shown by F1.
const HELP: &str = "\
  GETTING AROUND
    tab                 move between the prompt and the context
    pgup / pgdn         scroll the conversation
    ctrl+t              show or hide the trace
    ctrl+p              the exact request that would be sent next
    f1                  this
    esc                 close this, or stop what is running
    ctrl+c              stop what is running; again to leave

  THE PROMPT
    enter               send
    alt+enter           a new line

  THE CONTEXT, when it has the focus
    up / down, j / k    pick an item
    space               take it out of the next request, or put it back
    p                   pin it, so that compaction cannot touch it
    enter               read what it actually says
    u / U               undo / redo the last change to the context

  COMMANDS
    /request            the request that would go next
    /payload            the provider's own rendering of it, byte for byte
    /raw                the provider's own last answer
    /prune SELECTOR     take items out; e.g. all:tool_results, 12
    /keep SELECTOR      pin them
    /restore SELECTOR   put them back
    /budget             the estimate, what the last request really cost, and the
                        correction the counter has worked out from the difference
    /tools              what the model is offered
    /policy             what runs without being asked about
    /model [ID]         show or switch the model
    /params [KEY JSON]  show or set a model parameter
    /continue           carry on after the request budget ran out
    /save [PATH]        the session log, and a snapshot to resume from
    /quit";

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let input_height = (app.input.lines().len() as u16).clamp(1, 8) + 2;
    let [body, input, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let side = SIDE.min(frame.area().width / 2);
    let [conversation, aside] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(side)]).areas(body);

    draw_conversation(frame, app, conversation);
    match app.show_trace {
        true => {
            let [context, trace] =
                Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(aside);
            draw_context(frame, app, context);
            draw_trace(frame, app, trace);
        }
        false => draw_context(frame, app, aside),
    }

    draw_input(frame, app, input);
    draw_status(frame, app, status);

    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, overlay, app);
    }
}

// ------------------------------------------------------------------------------ the conversation

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered()
        .title(" conversation ")
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        let (prefix, style) = match entry.speaker {
            Speaker::User => ("> ", Style::default().fg(Color::White).bold()),
            Speaker::Model => ("", Style::default()),
            Speaker::Reasoning => ("", Style::default().fg(Color::DarkGray).italic()),
            Speaker::Call => ("⟩ ", Style::default().fg(Color::Cyan)),
            Speaker::Result => ("│ ", Style::default().fg(Color::DarkGray)),
            Speaker::Note => ("· ", Style::default().fg(Color::DarkGray)),
            Speaker::Error => ("! ", Style::default().fg(Color::Red)),
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

    frame.render_widget(Paragraph::new(lines).scroll((at as u16, 0)), inner);
}

// ---------------------------------------------------------------------------------- the context

fn draw_context(frame: &mut Frame, app: &mut App, area: Rect) {
    let budget = app.kernel.budget();
    let title = match budget.limit {
        Some(limit) => format!(
            " context · {} / {} ",
            thousands(budget.used()),
            thousands(limit)
        ),
        None => format!(" context · {} tokens ", thousands(budget.used())),
    };

    let focused = app.focus == Focus::Context;
    let block = Block::bordered().title(title).border_style(match focused {
        true => Style::default().fg(Color::Yellow),
        false => Style::default().fg(Color::DarkGray),
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items = app.kernel.items();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("nothing yet").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    // the width the label gets is whatever the identifier, the mark and the token count leave
    let label_width = (inner.width as usize).saturating_sub(3 + 2 + 8);
    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let (mark, style) = match item.state {
                ContextState::Active => ("·", Style::default()),
                ContextState::Pinned => ("▪", Style::default().fg(Color::Yellow)),
                ContextState::Excluded => ("-", Style::default().fg(Color::DarkGray)),
                ContextState::Archived => ("▫", Style::default().fg(Color::DarkGray)),
                ContextState::Superseded => ("~", Style::default().fg(Color::DarkGray)),
                _ => ("?", Style::default().fg(Color::DarkGray)),
            };
            let label = clip(&item.label, label_width);

            ListItem::new(Line::styled(
                format!(
                    "{:>3} {mark} {label:<label_width$}{:>7}",
                    item.id.0,
                    thousands(item.tokens)
                ),
                style,
            ))
        })
        .collect();

    app.selected = app.selected.min(items.len() - 1);
    app.list.select(Some(app.selected));
    let highlight = match focused {
        true => Style::default().add_modifier(Modifier::REVERSED),
        false => Style::default().bg(Color::Rgb(40, 40, 40)),
    };

    frame.render_stateful_widget(
        List::new(rows).highlight_style(highlight),
        inner,
        &mut app.list,
    );
}

// ------------------------------------------------------------------------------------ the trace

fn draw_trace(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(" trace ")
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let lines: Vec<Line> = app
        .trace
        .iter()
        .rev()
        .take(height)
        .rev()
        .map(|line| {
            Line::styled(
                clip(line, inner.width as usize),
                Style::default().fg(Color::DarkGray),
            )
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------------------------ the prompt

fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Input;
    app.input.set_block(
        Block::bordered()
            .title(match focused {
                true => " you ",
                false => " you · tab ",
            })
            .border_style(match focused {
                true => Style::default().fg(Color::White),
                false => Style::default().fg(Color::DarkGray),
            }),
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
    let dim = Style::default().fg(Color::DarkGray);
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

    if let Some(info) = app.kernel.model_info() {
        add(info.model, dim);
    }

    let budget = app.kernel.budget();
    let used = thousands(budget.used());
    add(
        match budget.fraction_used() {
            // a decimal place, because rounding a large context down to "0%" reads like a
            // measurement that is not being taken
            Some(fraction) => format!("~{used} tokens, {:.1}% of the limit", fraction * 100.0),
            None => format!("~{used} tokens, of an unknown limit"),
        },
        match budget.fraction_used() {
            Some(fraction) if fraction >= 0.9 => Style::default().fg(Color::Red),
            Some(fraction) if fraction >= 0.7 => Style::default().fg(Color::Yellow),
            _ => dim,
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

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
        Overlay::Help => panel(frame, " the keys ", HELP, 0, 78, 80),
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

    let capabilities: Vec<String> = request
        .capabilities
        .iter()
        .map(|capability| capability.to_string())
        .collect();
    let body = format!(
        "{} wants: {}\n\n{}\n\
         [y] once   [a] always, for {}   [n] no   [i] the exact JSON{}",
        request.tool,
        capabilities.join(", "),
        readable(&request.args),
        capabilities.join(" and "),
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

    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .scroll((at as u16, 0))
            .block(block.title_bottom(footer)),
        area,
    );
}

/// A rectangle in the middle, at most `columns` by `rows`, and never bigger than what it is in.
fn centred(area: Rect, columns: u16, rows: u16) -> Rect {
    let width = columns.min(area.width.saturating_sub(4)).max(20);
    let height = rows.min(area.height.saturating_sub(2)).max(4);

    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
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
fn fold(body: &str, room: usize) -> Vec<String> {
    if body.chars().count() <= room {
        return vec![body.to_owned()];
    }

    let mut out = Vec::new();
    let mut line = String::new();
    for word in body.split_whitespace() {
        // a single word longer than the pane is broken rather than allowed to overflow; the last
        // piece stays open, so that whatever follows can share the line with it
        if word.chars().count() > room {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let characters: Vec<char> = word.chars().collect();
            for piece in characters.chunks(room) {
                out.push(piece.iter().collect());
            }
            line = out.pop().unwrap_or_default();
            continue;
        }

        let column = line.chars().count();
        if column != 0 && column + 1 + word.chars().count() > room {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
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
