//! What drawing the chat tab costs as a session grows.
//!
//! note: the screen is redrawn on every tick and every key press, so this is the cost a long
//! session pays most often. Three cases, each at a few session lengths: a redraw with nothing
//! changed, a redraw at another width, which is what a resize does and what nothing laid out for
//! the old width survives, and the first frame of a fresh app, which is what opening a resumed
//! session costs.
//!
//! note: a session here is turns of the shape a coding session has: a short question, an answer
//! in markdown with a list, a code block and now and then a table, a call, and a long result. The
//! model is never asked anything; the items go straight into the kernel, and the app is told about
//! them the way the running program is, through the kernel's events.
//!
//! `cargo bench -p kamchatka --bench redraw`

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use divan::Bencher;
use kamchatka::{
    app::{App, Tab},
    tools::{Careful, Limits},
    ui,
};
use nachalnik::{Config, Content, ContextItem, Kernel, test::call};
use nachalnik_providers::OpenAiCompatible;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

fn main() {
    divan::main();
}

/// How many turns the session has.
const TURNS: [usize; 3] = [10, 100, 1000];

/// The size of the terminal, about what a laptop's is.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

/// A redraw with nothing changed since the last one.
#[divan::bench(args = TURNS)]
fn steady(bencher: Bencher, turns: usize) {
    let mut app = session(turns, 0);
    let mut terminal = terminal(WIDTH);
    draw(&mut terminal, &mut app);

    bencher.bench_local(|| draw(&mut terminal, &mut app));
}

/// A redraw at a width other than the last one, as each step of a resize is.
#[divan::bench(args = TURNS)]
fn resized(bencher: Bencher, turns: usize) {
    let mut app = session(turns, 0);
    let mut terminals = [terminal(WIDTH), terminal(WIDTH - 1)];
    let mut which = 0;

    bencher.bench_local(|| {
        which = 1 - which;
        draw(&mut terminals[which], &mut app);
    });
}

/// The first frame of an app that has just been handed the session.
///
/// note: every sample's session has words of its own. What a frame drew is kept by the text, on
/// the thread rather than in the app (see `ui::tabs::DRAWN`), so a second app holding the same
/// words would draw its first frame from what the first app drew. A resumed session in a fresh
/// process has nothing kept.
#[divan::bench(args = TURNS, sample_count = 20)]
fn first(bencher: Bencher, turns: usize) {
    static SAMPLE: AtomicUsize = AtomicUsize::new(1);
    bencher
        .with_inputs(|| {
            let sample = SAMPLE.fetch_add(1, Ordering::Relaxed);
            (session(turns, sample), terminal(WIDTH))
        })
        .bench_local_values(|(mut app, mut terminal)| draw(&mut terminal, &mut app));
}

fn terminal(width: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(width, HEIGHT)).expect("a terminal")
}

fn draw(terminal: &mut Terminal<TestBackend>, app: &mut App) {
    terminal.draw(|frame| ui::draw(frame, app)).expect("drawn");
}

/// An app on the chat tab, holding `turns` turns, whose words are told apart from another
/// session's by `sample`.
fn session(turns: usize, sample: usize) -> App {
    let kernel = Kernel::new(Config::default());
    let mut events = kernel.subscribe();
    let policy = Arc::new(Careful::new());
    kernel.set_policy(policy.clone());
    // nothing is sent anywhere; the app wants a provider to name on the status line
    let provider = Arc::new(OpenAiCompatible::new("bench", "http://127.0.0.1:1", ""));
    let (outcomes, _finished) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(kernel, policy, provider, Limits::default(), outcomes);

    for n in 0..turns {
        app.kernel.push(ContextItem::user(format!(
            "turn {n}: why does the parser drop the last line of the file?"
        )));
        let id = format!("call_{n}");
        app.kernel.push(ContextItem::assistant(
            answer(n, sample),
            vec![call(&id, "shell", json!({ "cmd": "cargo test -p parser" }))],
        ));
        app.kernel.push(ContextItem::tool_result(
            id.into(),
            "shell",
            Content::text(output(n)),
            false,
        ));
        // drained as it goes, so the broadcast channel never lags past what the app has seen
        while let Ok(event) = events.try_recv() {
            app.on_event(event);
        }
    }
    app.show(Tab::Chat);

    app
}

/// An answer the way a model writes one: prose, a list, a code block, and every fifth turn a table.
fn answer(n: usize, sample: usize) -> String {
    let mut answer = format!(
        "The loop in `read_lines` stops when `next` returns `None`, which happens before the \
         last line is pushed if the file does not end in a newline. Turn {n} of session \
         {sample}.\n\n\
         - the buffer is only flushed on `\\n`\n\
         - the final partial line stays in `pending`\n\
         - nothing reads `pending` after the loop\n\n\
         ```rust\n"
    );
    for line in 0..20 {
        answer.push_str(&format!(
            "    let line_{line} = pending.drain(..).collect::<String>(); // {n}\n"
        ));
    }
    answer.push_str("```\n");
    if n.is_multiple_of(5) {
        answer.push_str(
            "\n| case | before | after |\n|---|---|---|\n\
             | trailing newline | 3 lines | 3 lines |\n\
             | no trailing newline | 2 lines | 3 lines |\n\
             | empty file | 0 lines | 0 lines |\n",
        );
    }

    answer
}

/// A test run's output, long enough that the chat shows only the start of it.
fn output(n: usize) -> String {
    let mut output = String::new();
    for line in 0..200 {
        output.push_str(&format!("test parser::tests::case_{n}_{line} ... ok\n"));
    }
    output.push_str("\ntest result: ok. 200 passed; 0 failed\n");

    output
}
