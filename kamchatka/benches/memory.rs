//! What a session holds on the heap as it grows, against what its context says.
//!
//! The columns: `heap` is what is live, `payload` the bytes of text the context holds, which any
//! way of keeping the session has to keep, and `ratio` the one over the other; `log` is the
//! kernel's event log, `records` how many it has, and `loose` the chat's lines that are not
//! context items. `draw peak` and `snap peak` are how far above that the heap goes while a frame
//! is drawn at a new width, and while a snapshot is written out.
//!
//! note: a session driven the way the program drives one: keys typed into the app, turns run by
//! the kernel, events fed back to the app. The model streams its answer in small fragments and the
//! shell streams its output line by line, as real ones do, and both make every byte afresh, so
//! nothing is shared with a script prepared in advance.
//!
//! `cargo bench -p kamchatka --bench memory`

use std::{
    alloc::System,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use heapster::Heapster;
use kamchatka::{
    app::{App, Outcome, Tab},
    tools::{Careful, Limits},
    ui,
};
use nachalnik::{
    BoxError, Config, Content, DeltaSink, Kernel, ModelInfo, ModelRequest, ModelResponse,
    OutputSink, Provider, Tool, ToolCall, ToolOutput, ToolSpec, async_trait, test::call,
};
use nachalnik_providers::OpenAiCompatible;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use tokio::sync::{broadcast, mpsc};

#[global_allocator]
static HEAP: Heapster<System> = Heapster::new(System);

/// How many turns the session has had when it is measured.
const AT: [usize; 5] = [0, 100, 250, 500, 1000];

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(session());
}

async fn session() {
    let before = HEAP.use_curr();
    let mut at = Session::new();
    println!(
        "{:>6} {:>12} {:>12} {:>7} {:>12} {:>9} {:>9} {:>12} {:>12}",
        "turns", "heap", "payload", "ratio", "log", "records", "loose", "draw peak", "snap peak"
    );
    let mut turns = 0;
    for milestone in AT {
        while turns < milestone {
            at.turn(turns).await;
            turns += 1;
        }
        // drawn once at the width the session is read at, so what a frame keeps is in the count
        at.draw(120, 40);
        let heap = HEAP.use_curr() - before;
        let payload = at.payload();
        let draw = peak(|| at.draw(119, 40));
        let snapshot = peak(|| serde_json::to_vec(&at.app.kernel.snapshot()).map(|it| it.len()));
        // what a copy of the log allocates, which is about what the log holds
        let (_, log) = HEAP.measure(|| at.app.kernel.history());
        println!(
            "{:>6} {:>12} {:>12} {:>7.2} {:>12} {:>9} {:>9} {:>12} {:>12}",
            turns,
            heap,
            payload,
            heap as f64 / payload.max(1) as f64,
            log.alloc_sum,
            at.app.kernel.last_seq(),
            at.app.loose.len(),
            draw,
            snapshot,
        );
    }
}

/// How far above what was live when it started the heap went while `f` ran.
fn peak<R>(f: impl FnOnce() -> R) -> usize {
    HEAP.reset();
    let start = HEAP.use_curr();
    let kept = f();
    let peak = HEAP.use_max().saturating_sub(start);
    drop(kept);
    peak
}

/// An app with a kernel behind it, and what the loop holds to drive them.
struct Session {
    app: App,
    events: broadcast::Receiver<nachalnik::Event>,
    finished: mpsc::UnboundedReceiver<Outcome>,
}

impl Session {
    fn new() -> Self {
        let kernel = Kernel::new(Config::default());
        let events = kernel.subscribe();
        let policy = Arc::new(Careful::new());
        kernel.set_policy(policy.clone());
        kernel.set_provider(Arc::new(Model::default()));
        kernel.add_tool(Arc::new(Shell));
        let (outcomes, finished) = mpsc::unbounded_channel();
        // nothing is sent anywhere; the app wants a provider to name on the status line
        let provider = Arc::new(OpenAiCompatible::new("bench", "http://127.0.0.1:1", ""));
        let mut app = App::new(kernel, policy, provider, Limits::default(), outcomes);
        app.show(Tab::Chat);

        Self {
            app,
            events,
            finished,
        }
    }

    /// Types a question, waits for the turn it starts to end, and hands the app what happened.
    async fn turn(&mut self, n: usize) {
        for c in format!("question {n}: why does the parser drop the last line?").chars() {
            self.key(KeyCode::Char(c)).await;
        }
        self.key(KeyCode::Enter).await;
        loop {
            tokio::select! {
                event = self.events.recv() => match event {
                    Ok(event) => self.app.on_event(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                outcome = self.finished.recv() => {
                    while let Ok(event) = self.events.try_recv() {
                        self.app.on_event(event);
                    }
                    self.app.on_outcome(outcome.expect("the turn reports how it ended"));
                    self.app.release().await;
                    break;
                }
                () = tokio::time::sleep(Duration::from_secs(10)) => panic!("turn {n} never ended"),
            }
        }
    }

    async fn key(&mut self, code: KeyCode) {
        self.app
            .on_key(KeyEvent::new(code, KeyModifiers::NONE))
            .await;
    }

    fn draw(&mut self, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a terminal");
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .expect("drawn");
    }

    /// The bytes of text the context holds: what any representation of the session has to keep.
    fn payload(&self) -> usize {
        self.app
            .kernel
            .items()
            .iter()
            .map(|item| item.content.to_text().len())
            .sum()
    }
}

/// A model that answers every question by running the tests, then explaining what it found.
#[derive(Default)]
struct Model {
    asked: AtomicUsize,
}

#[async_trait]
impl Provider for Model {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("bench", "bench")
            .with_context_limit(usize::MAX / 2)
            .with_tool_calling(true)
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let n = self.asked.fetch_add(1, Ordering::Relaxed);
        if n.is_multiple_of(2) {
            return Ok(ModelResponse::tool_calls(vec![call(
                &format!("call_{n}"),
                "shell",
                json!({ "cmd": "cargo test -p parser" }),
            )]));
        }

        let mut answer = format!(
            "The loop in `read_lines` stops when `next` returns `None`, which happens before the \
             last line is pushed if the file does not end in a newline. Answer {n}.\n\n\
             - the buffer is only flushed on a newline\n\
             - the final partial line stays in `pending`\n\n```rust\n"
        );
        for line in 0..20 {
            answer.push_str(&format!(
                "    let line_{line} = pending.drain(..).collect::<String>(); // {n}\n"
            ));
        }
        answer.push_str("```\n");
        // in fragments of a few characters, as a model's tokens arrive
        for fragment in answer.as_bytes().chunks(6) {
            deltas.text(String::from_utf8_lossy(fragment).into_owned());
        }

        Ok(ModelResponse::text(answer))
    }
}

/// A shell that runs the tests: two hundred lines, written a line at a time.
struct Shell;

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("shell", "runs a command")
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let mut all = String::from("exit: 0\n");
        for line in 0..200 {
            let line = format!("test parser::tests::{}_{line} ... ok\n", call.id.0);
            output.push(line.clone());
            all.push_str(&line);
        }

        Ok(ToolOutput::new(Content::text(all)))
    }
}
