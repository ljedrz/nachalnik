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
//! about.

use nachalnik::{ContextKind, ContextState, Kernel, State, Verdict};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, Paragraph},
};

use tui_markdown::StyleSheet as _;

use crate::app::{App, Focus, Overlay, Speaker, Tab};

/// What the keys do, shown by F1.
///
/// note: no `\` continuation after the opening quote: it would eat the newline *and* the two
/// spaces indenting the first heading, leaving `THE TABS` flush against the border while every
/// other heading sat under it.
pub(crate) const HELP: &str = "  THE TABS
    ctrl+t              the next one
    alt+1 / 2 / 3 / 4   chat / context / trace / permissions
    tab                 move between the prompt and the tab, on the last three

  ANYWHERE
    ctrl+p              the exact request that would be sent next
    f1                  this
    esc                 close this, or stop what is running
    ctrl+c              stop what is running; again to leave

  THE PROMPT, which is under every tab
    enter               send
    alt+enter           a new line
    pgup / pgdn         scroll the conversation

  THE CONTEXT TAB, when it has the focus
    up / down, j / k    pick an item
    pgup / pgdn         a screenful at a time
    g / G               the first item / the last
    23G                 the item numbered 23
    space               take it out of the next request, or put it back
    p                   pin it, so that compaction cannot touch it
    e                   change what it says; the old one stays, marked ~
    enter               read the whole of what it says
    u / U               undo / redo the last change to the context

  THE TRACE TAB, when it has the focus
    up / down, j / k    read back through it
    pgup / pgdn         a screenful at a time
    g / G               the oldest it still holds / the newest

  THE PERMISSIONS TAB, when it has the focus
    up / down, j / k    pick a capability
    g / G               the first / the last
    space               cycle it: ask, then allow, then deny
    a / n / r           allow it / never allow it / ask about it again

  A TOOL IS WAITING TO RUN
    y / n               once / no
    a                   always, for what it needs rather than for its name
    i                   the exact JSON, and the tool's own definition
    d                   drop every call it is waiting on, and tell it why

  COMMANDS
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
    /seams              what is plugged into each of the runtime's six parts
    /tools              what the model is offered
    /tools drop ID      stop offering one of them, from now on
    /policy             open the permissions tab; also /permissions
    /model [ID]         show or switch the model
    /params [KEY JSON]  show or set a model parameter
    /save [PATH]        the session log, and a snapshot to resume from
    /quit";

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

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // editing an item wants more room than composing a message does, and the tab underneath is
    // the one thing that can afford to give it up: the item being edited is on it
    let most = match app.editing.is_some() {
        true => 16,
        false => 8,
    };
    let input_height = (app.input.lines().len() as u16).clamp(1, most) + 2;
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

/// The window: a strip of tabs, and whichever one is open filling everything under it.
fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Body;
    let dim = Style::default().fg(Color::DarkGray);

    let mut strip = Vec::new();
    for tab in Tab::ALL {
        if !strip.is_empty() {
            strip.push(Span::styled("│", dim));
        }
        // the open tab looks open whatever the keys are doing; which half of the window they are
        // talking to is the border's job, and having both say it left `chat` looking shut,
        // because the prompt always has the focus there
        strip.push(Span::styled(
            format!(" {} ", tab.name()),
            match tab == app.tab {
                true => Style::default().fg(Color::Yellow).bold(),
                false => dim,
            },
        ));
    }

    let block = Block::bordered()
        .title(Line::from(strip))
        .title_bottom(Line::styled(footer(app), dim).right_aligned())
        .border_style(match focused {
            true => Style::default().fg(Color::Yellow),
            false => dim,
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.tab {
        Tab::Chat => draw_chat(frame, app, inner),
        Tab::Context => draw_context(frame, app, inner),
        Tab::Trace => draw_trace(frame, app, inner),
        Tab::Permissions => draw_permissions(frame, app, inner),
    }
}

/// What the open tab has to say about itself, along the bottom.
fn footer(app: &App) -> String {
    match app.tab {
        Tab::Chat => match app.busy {
            true => " esc stops it ".to_owned(),
            false => " alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions ".to_owned(),
        },
        Tab::Context => {
            let out = app
                .kernel
                .items()
                .iter()
                .filter(|item| !item.is_projected())
                .count();
            match out {
                0 => format!(" {} items, all of them going ", app.kernel.items().len()),
                n => format!(" {} items, {n} not going ", app.kernel.items().len()),
            }
        }
        // the pane keeps the last few hundred; the log keeps everything, and `/save` writes it
        Tab::Trace => format!(" {} events · /save keeps them all ", app.trace.len()),
        Tab::Permissions => " space cycles · a allow · n never · r ask again ".to_owned(),
    }
}

// ------------------------------------------------------------------------------ the conversation

fn draw_chat(frame: &mut Frame, app: &mut App, inner: Rect) {
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        // the model writes markdown, and a terminal that printed the asterisks would be showing
        // the punctuation instead of the emphasis. Nothing else here is markdown: a tool's output
        // is whatever the tool said, and running it through a renderer would be inventing
        // structure the tool did not put there
        if entry.speaker == Speaker::Model {
            let rendered = tui_markdown::from_str_with_options(&entry.text, &markdown());
            for line in rendered.lines {
                // a fenced block gets a rule down its left rather than a slab of background,
                // which is the one thing a terminal cannot do without knowing the theme
                match line.style == Markdown.code() {
                    true => lines.extend(gutter(&line, width)),
                    false => match rule(&line) {
                        // a horizontal rule, drawn rather than spelled `---`
                        true => lines.push(Line::styled(
                            "─".repeat(width),
                            Style::default().fg(Color::DarkGray),
                        )),
                        false => lines.extend(refit(&line, width)),
                    },
                }
            }
            lines.push(Line::default());
            continue;
        }

        let (prefix, style) = match entry.speaker {
            Speaker::User => ("> ", Style::default().fg(Color::White).bold()),
            Speaker::Reasoning => ("", Style::default().fg(Color::DarkGray).italic()),
            Speaker::Call => ("⟩ ", Style::default().fg(Color::Cyan)),
            Speaker::Result => ("│ ", Style::default().fg(Color::DarkGray)),
            Speaker::Note => ("· ", Style::default().fg(Color::DarkGray)),
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

    frame.render_widget(Paragraph::new(lines).scroll((at as u16, 0)), inner);
}

// ---------------------------------------------------------------------------------- the context

/// The context as a table: every item, what kind of thing it is, what it costs, and either the
/// first line of what it says or - if it is not going into the next request - why not.
///
/// note: With the whole window to work in there is room for the last column, and it is the one
/// that matters: a list of labels and numbers tells you an item exists, and this tells you what
/// the model is actually being told.
fn draw_context(frame: &mut Frame, app: &mut App, area: Rect) {
    let items = app.kernel.items();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("nothing here yet").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
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
            Style::default().fg(Color::DarkGray),
        )),
        head,
    );

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

            // an item that is not going says why, in the projector's own words; one that is
            // shows the first thing the model will read of it
            let (tail, tail_style) = match item.is_projected() {
                false => (
                    match &item.note {
                        Some(note) => format!("{}: {note}", item.state),
                        None => item.state.to_string(),
                    },
                    Style::default().fg(Color::DarkGray).italic(),
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
                            ContextKind::AssistantMessage { tool_calls, .. }
                                if !tool_calls.is_empty() =>
                            {
                                let names: Vec<_> =
                                    tool_calls.iter().map(|call| call.tool.as_str()).collect();
                                format!("asked for {}", names.join(", "))
                            }
                            _ => String::new(),
                        },
                    },
                    Style::default().fg(Color::DarkGray),
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
                    Style::default().fg(Color::DarkGray),
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
        false => Style::default().bg(Color::Rgb(40, 40, 40)),
    };

    frame.render_stateful_widget(
        List::new(rows).highlight_style(highlight),
        inner,
        &mut app.list,
    );
}

// ------------------------------------------------------------------------------ the permissions

/// What the policy will answer about each capability, and which tools that covers.
///
/// note: The permission prompt is the policy's only other appearance, and it shows up one call at
/// a time, at the worst possible moment to think about it. This is the same decisions, all of
/// them, in advance, and changeable - which is also the plainest thing to point at when somebody
/// asks what a replaceable `PermissionPolicy` buys you: a policy is an object with state, not a
/// callback you can only learn about by triggering it.
fn draw_permissions(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.permissions();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("no tool has declared anything")
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let [head, inner] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = inner.width as usize;
    let capability = 22.min(width / 3);
    let covers = width.saturating_sub(2 + capability + 1 + 10 + 2);

    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "  {:<capability$} {:<10}  {}",
                "capability",
                "answer",
                match covers >= 8 {
                    true => "the tools it covers",
                    false => "",
                }
            ),
            Style::default().fg(Color::DarkGray),
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
            // a capability nothing declares is still worth a row - `network` is refused here and
            // no built-in tool wants it - but it should say so rather than look like an oversight
            let tools = match row.tools.is_empty() {
                true => "nothing registered needs it".to_owned(),
                false => row.tools.join(", "),
            };

            ListItem::new(Line::from(vec![
                Span::raw(format!(
                    "  {:<capability$} ",
                    clip(&row.capability.to_string(), capability)
                )),
                Span::styled(format!("{answer:<10}  "), style),
                Span::styled(
                    clip(&tools, covers),
                    Style::default().fg(match row.tools.is_empty() {
                        true => Color::DarkGray,
                        false => Color::Reset,
                    }),
                ),
            ]))
        })
        .collect();

    app.chosen = app.chosen.min(rows.len() - 1);
    app.grants.select(Some(app.chosen));
    let highlight = match app.focus == Focus::Body {
        true => Style::default().add_modifier(Modifier::REVERSED),
        false => Style::default().bg(Color::Rgb(40, 40, 40)),
    };

    frame.render_stateful_widget(
        List::new(listed).highlight_style(highlight),
        inner,
        &mut app.grants,
    );
}

// ------------------------------------------------------------------------------------ the trace

/// Every event, newest at the bottom, in two aligned columns.
///
/// note: The names are a column of their own rather than run together with what they say, which
/// is what a whole window buys: `permission.requested` is the longest of them, so twenty-two
/// columns line every event up under the last. Anything that still does not fit wraps under the
/// column rather than being cut off - a log whose lines end in an ellipsis in the middle of the
/// interesting part is not a log.
fn draw_trace(frame: &mut Frame, app: &mut App, inner: Rect) {
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
            _ if event.name.is_empty() => Color::DarkGray,
            _ => Color::Gray,
        };

        let named = Style::default().fg(colour);
        let said = Style::default().fg(Color::DarkGray);
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

    frame.render_widget(Paragraph::new(lines).scroll((at as u16, 0)), inner);
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
                false => Color::DarkGray,
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
    // two lines of options rather than one that wraps wherever it happens to run out: the answers
    // on the first, and the two that are about looking closer or giving up on the lot on the
    // second
    let body = format!(
        "{} wants: {}\n\n{}\n\
         [y] once   [a] always, for {}   [n] no\n\
         [i] the exact JSON   [d] {}{}",
        request.tool,
        capabilities.join(", "),
        readable(&request.args),
        capabilities.join(" and "),
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
        Style::default().fg(Color::DarkGray).italic()
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
    let bar = Span::styled("│ ", Style::default().fg(Color::DarkGray));
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
