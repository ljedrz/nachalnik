//! A System One engine running on this machine, spoken to over a pipe.
//!
//! note: this exists because the open engines are *libraries*. `laya`, the one this was written
//! against, is `pip install laya` and a `Router` with a `predict` method: no HTTP interface, no
//! CLI, nothing to point a base URL at. So a local advisor is a process somebody runs, and the
//! seam it fits is [`SystemOne`](nachalnik_providers::system1::SystemOne) rather than a second
//! address.
//!
//! note: **nothing leaves the machine**, and that is what this changes. Everything `tools::advice`
//! says about disclosure is about a third party reading a tool's arguments, which for a write is
//! the text being written and for a shell call is the command line. Pointed at a local engine
//! there is no third party: the arguments go to a process the person running this started, under
//! their own user, and come back as numbers. `--advise` still says what it says, because what a
//! flag turns on should not depend on an environment variable - but the thing it is careful about
//! is not happening.
//!
//! note: **the child's output is this program's to hold, not the terminal's.** Both streams are
//! piped: stdout because it is the answers, and stderr because a session with a screen is a
//! session whose terminal is being drawn on. An engine that inherits it writes over the frame -
//! a checkpoint downloading says so at length, and `laya` pulls one on first use - and what
//! lands is a session nobody can read.
//!
//! note: piped is not enough on its own. A pipe nobody reads fills and the child blocks writing
//! to it, so the advisor stops answering and nothing says why. So stderr is *drained* - a task
//! reads it for the life of the child - and the last few lines are kept. They are attached to
//! whatever failure they explain rather than printed, because the moment a traceback is worth
//! reading is the moment a question comes back with nothing in it.
//!
//! note: one long-lived process rather than one per question. `laya` loads a 421M-parameter
//! checkpoint, and `Router(preload=True)` exists because that is a cost to pay once. Paying it
//! per permission question would put seconds in front of somebody waiting to press `y`, on every
//! command, and not doing that is what the model was chosen for.
//!
//! note: the protocol is the body [`Jev`](nachalnik_providers::system1::Jev) already sends, one
//! JSON object per line, answered by one JSON object per line. Not a new format: each question
//! goes out through the `Question::to_wire` that `Jev::render` uses, and
//! [`Answers`](nachalnik_providers::system1::Answers) reads the reply the way the HTTP path does -
//! so a shim is a loop around `predict`, and the two engines cannot drift into two request
//! shapes. What a local engine does with `model` is its own business; `laya`'s router picks a
//! checkpoint.

use std::{collections::VecDeque, process::Stdio, sync::Arc, time::Duration};

use nachalnik::BoxError;
use nachalnik_providers::system1::{Answers, Question, SystemOne};
use parking_lot::Mutex as Sync;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
};

/// How long one question may take before the advisor is given up on.
///
/// note: the same thirty seconds the HTTP client waits, and for the same reason: this is a
/// permission gate with somebody sitting in front of it, and a local engine that has not
/// answered in half a minute has hung rather than thought. What a timeout costs here is the
/// second opinion on one call - see the failure note on [`Local::ask`].
const PATIENCE: Duration = Duration::from_secs(30);

/// How many lines of whatever the engine says about itself are kept.
///
/// note: a ring rather than everything, because the first thing a local engine does is download
/// a checkpoint and say so, at length. What these are for is explaining one failure, and the
/// lines that explain one are the last ones.
const REMEMBERED: usize = 20;

/// And how much of one line, in bytes.
///
/// note: a progress bar redraws itself with a carriage return and no newline, so a downloader's
/// whole output can arrive as one line megabytes long. Reading it by lines is right and keeping
/// all of one is not.
const LINE: usize = 200;

/// What the child is told this is, since a local engine has no model to name.
///
/// note: sent all the same, because the body is the one `Jev` sends and a shim written against
/// that body should not have to special-case a missing field. A shim is free to ignore it, and
/// `laya`'s router does - it picks a checkpoint by reading the state's script.
const MODEL: &str = "local";

/// A System One engine this program started, and the pipe to it.
pub struct Local {
    /// What was run, for the notice and for what a failure names.
    command: String,
    /// The child, its stdin and its stdout, held together because they are used together.
    ///
    /// note: one lock over all three rather than a lock each, and an async one. A question is a
    /// write *and* the read that answers it, and two of those interleaved on one pipe would pair
    /// each answer with the wrong question - which is worse than a wrong answer, because both
    /// would parse. `--parallel` is what makes that reachable: the kernel asks the policy about
    /// several calls at once, and this is the thing they all queue on.
    ///
    /// note: `tokio`'s rather than `parking_lot`'s, because it is held across an `.await`.
    pipe: Arc<Mutex<Option<Pipe>>>,
    /// What is worth saying and has not been said yet, oldest first.
    ///
    /// note: a queue rather than the single slot the HTTP client keeps, because the lines worth
    /// reporting here arrive on their own schedule rather than one per request. A local engine
    /// loads a checkpoint before it can answer anything, and the only account of how that is
    /// going is what the engine writes about itself - so those lines are reported as they come
    /// instead of being kept for a failure that may never happen.
    ///
    /// note: bounded, for the reason the ring below is. Nobody is obliged to read these and a
    /// session that never does should not grow a queue.
    notice: Arc<Sync<VecDeque<String>>>,
    /// The last few lines the engine wrote to its own stderr, drained as they arrive.
    ///
    /// note: kept as well as reported, because the two are read at different moments. A line
    /// reported at the tick it arrived has scrolled past by the time a question goes unanswered;
    /// this is what gets hung on that failure.
    said: Arc<Sync<VecDeque<String>>>,
}

/// Asks the engine one trivial question, so that being ready is something it demonstrated.
///
/// note: two lines is what somebody waiting wants - it is not ready, and now it is - and the
/// second one has to mean something. Watching the engine's output for a word like `ready` would
/// be reading a magic string out of a program this does not own; answering a question is the
/// thing itself, and any engine that can be pointed at this can do it.
///
/// note: not awaited, which is why it is a task. Loading a checkpoint takes seconds
/// and a session must not wait on an advisor it may never consult - so the warm-up and the first
/// real question race for the same lock, and whichever arrives second waits for the first. That
/// is the wait the session was always going to have, spent once.
///
/// note: it goes through `exchange`, so a warm-up that fails closes the pipe exactly as a real
/// question would - and says so, with whatever the engine wrote on its stderr attached. It is the
/// local engine's counterpart to `Jev::probe` at startup: a shim that cannot answer is found
/// before a permission question depends on it rather than at the first `y`.
fn warm(
    pipe: Arc<Mutex<Option<Pipe>>>,
    notice: Arc<Sync<VecDeque<String>>>,
    said: Arc<Sync<VecDeque<String>>>,
) {
    tokio::spawn(async move {
        let body = json!({
            "model": MODEL,
            "state": { "cmd": "true" },
            "questions": { "ready": Question::noul("Is this a command?").to_wire() },
        });

        // note: nothing is done with the answer. What is being checked is that one came back at
        // all, which is what readiness means here; a `ready` that also had to be
        // *correct* would be this program grading an engine on a question it made up
        if exchange(&pipe, &notice, &said, &body).await.is_ok() {
            remark(&notice, "the advisor is ready".to_owned());
        }
    });
}

/// Reads the child's stderr for as long as it has one, keeping the last [`REMEMBERED`] lines.
///
/// note: a task rather than a read at the point of failure, because the reason to read it at all
/// is that an unread pipe fills and blocks the writer. What would block is the engine, on its own
/// diagnostics, and the symptom would be an advisor that stopped answering for a reason nothing
/// could report.
///
/// note: kept and not reported. Reporting each line as it arrives is unreadable: a downloader
/// draws a progress bar by rewriting one line with carriage returns, so what arrives is enormous
/// lines of `0%|    |` and the session fills with them. What somebody
/// waiting on a checkpoint wants is that it is loading and that they will be told when it is
/// done, which is two lines - see [`Local::new`]. These are for the failure they explain.
///
/// note: it ends when the child closes the stream, which is when the child ends, so there is
/// nothing to cancel. [`Local`] kills the child on drop and this sees the close.
fn drain(errors: tokio::process::ChildStderr, said: Arc<Sync<VecDeque<String>>>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(errors).lines();
        while let Ok(Some(mut line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > LINE {
                // on a character boundary, since this is somebody else's output
                let mut room = LINE;
                while room > 0 && !line.is_char_boundary(room) {
                    room -= 1;
                }
                line.truncate(room);
                line.push('…');
            }

            let mut said = said.lock();
            if said.len() == REMEMBERED {
                said.pop_front();
            }
            said.push_back(line);
        }
    });
}

/// The last few lines an engine wrote about itself, as one, or nothing where it wrote none.
///
/// note: for hanging on the end of a failure rather than for printing. A local engine that
/// answered nothing has usually said why on its stderr, and that sentence is the difference
/// between "the advisor stopped answering" and a traceback naming the line.
fn complaint(said: &Sync<VecDeque<String>>) -> String {
    let said = said.lock();
    match said.is_empty() {
        true => String::new(),
        false => format!(
            "; it last said: {}",
            said.iter().cloned().collect::<Vec<_>>().join(" / ")
        ),
    }
}

/// Adds something worth saying, keeping the queue bounded.
///
/// note: the *oldest* is dropped when it is full, which is the opposite of what a queue nobody
/// reads usually wants. What fills this is an engine talking about itself while it starts, and
/// the line that matters is the last one - `ready`, or whatever it failed with.
fn remark(notice: &Sync<VecDeque<String>>, line: String) {
    let mut notice = notice.lock();
    if notice.len() == REMEMBERED {
        notice.pop_front();
    }
    notice.push_back(line);
}

/// The half of a running child this talks through.
struct Pipe {
    /// Kept so that dropping [`Local`] kills the engine rather than orphaning it.
    ///
    /// note: `kill_on_drop`, set where it is spawned. A model this size is hundreds of megabytes
    /// of resident memory, and a session that left one running every time it ended would be a
    /// program that leaks a process per run.
    _child: Child,
    writes: ChildStdin,
    reads: BufReader<ChildStdout>,
}

impl Local {
    /// Starts the engine named by a command line, and holds the pipe to it.
    ///
    /// note: the words split on whitespace and the first one run directly, which is what `--mcp`
    /// does and for its reason: no shell in the middle, so a path with a space in it is a
    /// problem somebody can see rather than a quoting rule they have to know. A local engine is
    /// `<interpreter> <script>` at minimum, because `laya` ships no executable at all.
    ///
    /// note: it does not wait for the child to be *ready*. Loading a checkpoint takes seconds,
    /// and blocking startup on it would make every session pay for an advisor a session might
    /// never consult - the first question waits instead, which is the one place the cost belongs.
    pub fn new(command: &str) -> Result<Self, BoxError> {
        let mut words = command.split_whitespace();
        let program = words
            .next()
            .ok_or("a local advisor needs a command to run")?;

        let mut child = tokio::process::Command::new(program)
            .args(words)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // note: piped and drained below, never inherited. A session with a screen is one
            // whose terminal is being drawn on, and a child writing to the same terminal writes
            // over the frame - which a checkpoint downloading does at length
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("could not start the advisor `{program}`: {e}"))?;

        let writes = child.stdin.take().ok_or("the advisor has no stdin")?;
        let reads = BufReader::new(child.stdout.take().ok_or("the advisor has no stdout")?);

        let said = Arc::new(Sync::new(VecDeque::new()));
        let notice = Arc::new(Sync::new(VecDeque::new()));
        if let Some(errors) = child.stderr.take() {
            drain(errors, said.clone());
        }

        let pipe = Arc::new(Mutex::new(Some(Pipe {
            _child: child,
            writes,
            reads,
        })));
        remark(
            &notice,
            format!("the advisor `{program}` is not ready yet; you will be told when it is"),
        );
        warm(pipe.clone(), notice.clone(), said.clone());

        Ok(Self {
            command: command.to_owned(),
            pipe,
            notice,
            said,
        })
    }

    /// What was run, which is what the setup tab and the status line name.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// One question and its answer, over the pipe.
    async fn asked(&self, body: &Value) -> Result<Value, BoxError> {
        exchange(&self.pipe, &self.notice, &self.said, body).await
    }
}

/// One question and its answer, over a pipe somebody else is holding.
///
/// note: a function over the three handles rather than a method, because [`Local::new`] fires a
/// warm-up before there is a `Local` to call one on. What the two callers must share is this
/// exactly: the same lock, so a warm-up still in flight is a real question's queue rather than a
/// second writer, and the same closing rule.
///
/// note: every failure here closes the pipe rather than leaving it half-used. A child that died,
/// a line that did not parse and a question that timed out all leave a stream whose next read is
/// the answer to the question *before* it - and an advisor answering the previous call's question
/// is the one failure mode worse than no advisor, because it is a confident answer about the
/// wrong command. So the pipe is taken, and every later question says the advisor is gone.
async fn exchange(
    pipe: &Mutex<Option<Pipe>>,
    notice: &Sync<VecDeque<String>>,
    said: &Sync<VecDeque<String>>,
    body: &Value,
) -> Result<Value, BoxError> {
    let mut held = pipe.lock().await;
    let open = held.as_mut().ok_or("the advisor is no longer running")?;

    let asking = async {
        let line = serde_json::to_string(body)?;
        open.writes.write_all(line.as_bytes()).await?;
        open.writes.write_all(b"\n").await?;
        open.writes.flush().await?;

        let mut answer = String::new();
        match open.reads.read_line(&mut answer).await? {
            0 => Err::<Value, BoxError>("the advisor stopped answering".into()),
            _ => Ok(serde_json::from_str(&answer)?),
        }
    };

    let closed = |why: String| -> BoxError {
        let why = format!("{why}{}", complaint(said));
        remark(notice, why.clone());
        why.into()
    };

    match tokio::time::timeout(PATIENCE, asking).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(e)) => {
            *held = None;
            Err(closed(format!("the advisor failed and was closed: {e}")))
        }
        Err(_) => {
            *held = None;
            Err(closed(format!(
                "the advisor did not answer in {}s and was closed",
                PATIENCE.as_secs()
            )))
        }
    }
}

#[nachalnik::async_trait]
impl SystemOne for Local {
    async fn ask(
        &self,
        state: Value,
        questions: Vec<(String, Question)>,
    ) -> Result<Answers, BoxError> {
        if questions.is_empty() {
            return Err("no questions to ask".into());
        }

        // note: built here rather than by asking `Jev` to render one, because a `Jev` is an HTTP
        // client with a key in it and there may not be one. What keeps the two bodies the same
        // is that this is the documented request shape, with each question put through the
        // `to_wire` that `Jev::render` uses
        let body = json!({
            "model": MODEL,
            "state": state,
            "questions": questions
                .iter()
                .map(|(name, question)| (name.clone(), question.to_wire()))
                .collect::<serde_json::Map<_, _>>(),
        });

        Ok(Answers::read(self.asked(&body).await?))
    }

    fn notice(&self) -> Option<String> {
        self.notice.lock().pop_front()
    }

    fn named(&self) -> String {
        self.command.clone()
    }
}

/// A local advisor where one is asked for, and nothing where none is.
///
/// note: read here rather than in `endpoint`, and checked *before* the key, which is what "takes
/// precedence" means: a machine with both set has an engine of its own running and a key it
/// would otherwise spend, and the local one is both cheaper and quieter. Somebody who wants the
/// remote one on a machine that has a local engine unsets this.
pub fn configured() -> Option<Result<Arc<dyn SystemOne>, BoxError>> {
    let command = std::env::var("SYSTEM1_ADVISOR_COMMAND").ok()?;
    if command.trim().is_empty() {
        return None;
    }

    Some(Local::new(&command).map(|local| Arc::new(local) as Arc<dyn SystemOne>))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the engine said about itself, as the failure path would attach it.
    fn complaint_of(local: &Local) -> String {
        complaint(&local.said)
    }

    /// The complaint once `lines` of the engine's own output have arrived.
    ///
    /// note: the child writes them before it answers, and the answer can still get here first:
    /// the ring is filled by its own task, and on windows the pipe is read on a blocking thread
    /// that returns as soon as it has anything, so two lines written a moment apart reach the
    /// ring a wake-up apart. Waiting for the first and asserting on the second is a race, and
    /// windows loses it.
    async fn complaint_after(local: &Local, lines: usize) -> String {
        for _ in 0..40 {
            if local.said.lock().len() >= lines {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        complaint_of(local)
    }

    /// Everything the advisor has to say, drained, so a test can look for one line among them.
    ///
    /// note: `notice` is a queue rather than a slot, and the first thing in it is always the
    /// engine starting, which is in the way of every assertion below.
    fn everything(local: &Local) -> Vec<String> {
        std::iter::from_fn(|| local.notice()).collect()
    }

    /// The shipped shim's own translation, checked by `cargo test` rather than by hand.
    ///
    /// note: `contrib/laya_advisor.py` is the other half of this feature and is the half that
    /// can be wrong on a machine with no checkpoint on it. Its `--selftest` runs the translation
    /// over a recorded laya-shaped answer and needs no `laya` installed, so there is no reason
    /// for it not to be run here. The failure it guards against is laya's `confidence` passed
    /// through as the caller's, which it is not, and which turns every command yellow.
    ///
    /// note: skipped where there is no `python3`, like every other test in this file that needs
    /// one. What the suite loses on such a machine is a check on a file it also cannot run.
    #[test]
    fn the_shipped_shim_translates_what_laya_answers() {
        let shim = concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/laya_advisor.py");
        let Ok(out) = std::process::Command::new("python3")
            .args([shim, "--selftest"])
            .output()
        else {
            return;
        };

        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The shim's probe asks the questions the program asks, word for word, of the state the
    /// program sends.
    ///
    /// note: a probe that does not ask what the program asks measures something nobody runs, and
    /// reads as evidence while doing it. Copying the rubric is not enough: a bare command is a
    /// different and easier question than the call the program sends, and the gap between the two
    /// answers is large enough to be mistaken for a fact about the rubric. So both the texts and
    /// the *state* are held to the program's.
    ///
    /// note: the texts live in two files and two languages because one of them is a Python script
    /// somebody runs by hand. This is what stops that being a drift: the copy has to contain
    /// everything the program sends, checked against the constants rather than against a second
    /// copy of them.
    ///
    /// note: the state is checked by its keys rather than by its rendering, because what the probe
    /// has to get right is which parts of a call the advisor is shown. A key added to
    /// [`state`](crate::tools::advice::state) and not to the probe is the drift this fails on.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn the_probe_asks_the_question_the_program_asks() {
        use nachalnik::{Capability, PermissionId, PermissionRequest, ToolCallId};

        use crate::tools::advice;

        let shim = include_str!("../contrib/laya_advisor.py");

        let sent = advice::LEVELS.iter().copied().chain([
            advice::DECIDE,
            advice::DESTROYS,
            advice::PLACE,
            advice::RUIN,
            advice::RUINED,
            advice::INTACT,
        ]);
        for text in sent {
            assert!(
                shim.contains(text),
                "`contrib/laya_advisor.py` does not send this, so its probe is about a different \
                 question than the program: {text:?}"
            );
        }

        let state = advice::state(&PermissionRequest {
            id: PermissionId(1),
            call: ToolCallId::from("call-1"),
            tool: "shell".to_owned(),
            capabilities: vec![Capability::exec("run")],
            args: Arc::new(json!({ "cmd": "ls" })),
        });
        for key in state.as_object().expect("the state is an object").keys() {
            assert!(
                shim.contains(&format!("\"{key}\"")),
                "the probe's state has no `{key}`, so it shows the advisor less than the program \
                 does"
            );
        }
    }

    /// A command that is not there is a refusal naming it, rather than a panic or a hang.
    #[tokio::test]
    async fn an_advisor_that_cannot_be_started_says_which_one() {
        let Err(refused) = Local::new("kamchatka-no-such-advisor-anywhere") else {
            panic!("nothing is going to start that");
        };
        let said = refused.to_string();
        assert!(
            said.contains("kamchatka-no-such-advisor-anywhere"),
            "{said}"
        );
    }

    /// An empty command is a caller's mistake rather than a spawn of nothing.
    #[test]
    fn a_command_with_no_program_in_it_is_refused() {
        assert!(Local::new("   ").is_err());
    }

    /// The whole round trip against a real child, which is the only thing that checks the
    /// protocol is a protocol.
    ///
    /// note: the shim here is four lines of Python rather than the shipped one, because the
    /// shipped one needs `laya` and a checkpoint. What it stands in for is the *contract* -
    /// a body on stdin with `state` and `questions` in it, an `answers` object on stdout - and
    /// that contract is the thing two processes can disagree about. Whether `laya` places a
    /// command well is a question about `laya`.
    ///
    /// note: it reads the questions back out of the request and answers each by name, so a shim
    /// that was handed the wrong shape produces the wrong names and this fails on the lookup
    /// rather than on the parse.
    #[tokio::test]
    async fn a_question_and_its_answer_make_it_through_a_real_child() {
        let shim = "\
import json, sys
for line in sys.stdin:
    asked = json.loads(line)
    out = {}
    for name, q in asked['questions'].items():
        if q['type'] == 'noul':
            out[name] = {'type': 'noul', 'noul': 0.75}
        else:
            out[name] = {'type': 'score', 'score': 2.0, 'confidence': 0.9,
                         'legend': {}, 'probabilities': {}}
    json.dump({'model': 'stub', 'answers': out}, sys.stdout)
    print()
    sys.stdout.flush()
";
        let at = std::env::temp_dir().join(format!("kamchatka-shim-{}.py", std::process::id()));
        if std::fs::write(&at, shim).is_err() {
            return;
        }

        let Ok(local) = Local::new(&format!("python3 {}", at.display())) else {
            // no python3 here; the contract is checked wherever there is one
            let _ = std::fs::remove_file(&at);
            return;
        };

        let answers = local
            .ask(
                json!({ "tool": "shell", "arguments": { "cmd": "rm -rf ~" } }),
                vec![
                    ("verdict".to_owned(), Question::noul("is it bad?")),
                    (
                        "rating".to_owned(),
                        Question::score("how bad?", ["none", "some", "total"]),
                    ),
                ],
            )
            .await
            .expect("the child answered");

        // read back through the same `Answers` the HTTP path uses, by the names asked
        assert_eq!(answers.noul("verdict"), Some(0.75));
        assert_eq!(answers.score("rating"), Some(2.0));
        assert_eq!(answers.confidence("rating"), Some(0.9));
        assert_eq!(answers.model, "stub");
        // and the accessors still refuse a type nobody sent, which is what says the reader is
        // the real one rather than something written for this test
        assert_eq!(answers.score("verdict"), None);

        // the pipe survives an exchange, so the next question is not a fresh process
        let again = local
            .ask(
                json!({ "tool": "shell" }),
                vec![("verdict".to_owned(), Question::noul("again?"))],
            )
            .await
            .expect("the same child answered twice");
        assert_eq!(again.noul("verdict"), Some(0.75));
        assert!(
            !everything(&local)
                .iter()
                .any(|line| line.contains("closed")),
            "nothing went wrong"
        );

        let _ = std::fs::remove_file(&at);
    }

    /// Whatever the engine says about itself is captured, not printed.
    ///
    /// note: the bug this is here for, which a screen makes obvious and a test does not: stderr
    /// inherited puts the child's output on the terminal `ratatui` is drawing, so a checkpoint
    /// downloading writes over the session. What the test can check is the other side of the
    /// same fact - the lines went somewhere this program can produce them from - and a line
    /// that reached the ring is a line that did not reach the frame.
    ///
    /// note: it also pins that stderr being read at all does not depend on a failure. The pipe
    /// is drained for the life of the child, because an unread one fills and blocks the engine
    /// writing to it - which would be an advisor that stopped answering with nothing saying why.
    #[tokio::test]
    async fn what_the_engine_says_about_itself_is_captured_rather_than_printed() {
        let shim = "\
import sys
print('laya: fetching a checkpoint', file=sys.stderr, flush=True)
print('x' * 4000, file=sys.stderr, flush=True)
for line in sys.stdin:
    print('{\"model\": \"stub\", \"answers\": {}}', flush=True)
";
        let at = std::env::temp_dir().join(format!("kamchatka-noisy-{}.py", std::process::id()));
        if std::fs::write(&at, shim).is_err() {
            return;
        }
        let Ok(local) = Local::new(&format!("python3 {}", at.display())) else {
            let _ = std::fs::remove_file(&at);
            return;
        };

        local
            .ask(
                json!({ "cmd": "ls" }),
                vec![("q".to_owned(), Question::noul("is it?"))],
            )
            .await
            .expect("it answered");

        let complaint = complaint_after(&local, 2).await;

        assert!(
            complaint.contains("fetching a checkpoint"),
            "the engine's own output should be held here: {complaint}"
        );
        // and a line long enough to be a progress bar redrawing itself is cut rather than kept
        assert!(
            !complaint.contains(&"x".repeat(LINE + 1)),
            "a very long line is kept at {LINE} bytes"
        );
        assert!(
            complaint.contains('…'),
            "and the cut is marked: {complaint}"
        );

        let _ = std::fs::remove_file(&at);
    }

    /// Two lines and no more: it is not ready, and then it is.
    ///
    /// note: this is all a session is told about a local engine starting. Reporting the engine's
    /// own output instead is unreadable - a downloader draws a progress bar by rewriting one
    /// line, so what arrives is enormous `0%|    |` lines in the middle of the session.
    ///
    /// note: and "ready" is something the engine *demonstrated*, which is why the shim here
    /// answers rather than printing a word. A readiness read out of the child's output would be
    /// this program matching a magic string in a program it does not own.
    #[tokio::test]
    async fn a_local_engine_says_it_is_not_ready_and_then_that_it_is() {
        let shim = "\
import json, sys
print('Fetching 38 files: 0%|          | 0/38', file=sys.stderr, flush=True)
for line in sys.stdin:
    asked = json.loads(line)
    out = {n: {'type': 'noul', 'noul': 0.5} for n in asked['questions']}
    json.dump({'model': 'stub', 'answers': out}, sys.stdout)
    print()
    sys.stdout.flush()
";
        let at = std::env::temp_dir().join(format!("kamchatka-warm-{}.py", std::process::id()));
        if std::fs::write(&at, shim).is_err() {
            return;
        }
        let Ok(local) = Local::new(&format!("python3 {}", at.display())) else {
            let _ = std::fs::remove_file(&at);
            return;
        };

        let mut said = Vec::new();
        for _ in 0..80 {
            said.extend(everything(&local));
            if said.iter().any(|line| line.contains("is ready")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        assert_eq!(
            said.len(),
            2,
            "two lines and no more is the whole report: {said:?}"
        );
        assert!(said[0].contains("not ready yet"), "{said:?}");
        assert!(said[1].contains("is ready"), "{said:?}");

        // the progress bar the engine drew is kept for a failure and was never reported
        let complaint = complaint_after(&local, 1).await;
        assert!(
            complaint.contains("Fetching 38 files"),
            "it is kept: {complaint}"
        );
        assert!(
            !said.iter().any(|line| line.contains("Fetching")),
            "and not reported: {said:?}"
        );

        let _ = std::fs::remove_file(&at);
    }

    /// A child that says nothing and exits leaves an advisor that is gone, and says so.
    ///
    /// note: the property the pipe is taken for. The failure is not that one question went
    /// unanswered - it is that the *next* question must not be paired with a stale line, so
    /// once the stream is doubtful it is closed rather than reused.
    #[tokio::test]
    async fn an_advisor_that_stops_answering_is_closed_rather_than_reused() {
        let Ok(local) = Local::new("true").map_err(|_| ()) else {
            // no `true` on this platform; the two tests above cover the rest
            return;
        };

        let asking = || {
            local.ask(
                json!({ "cmd": "ls" }),
                vec![("q".to_owned(), Question::noul("is it?"))],
            )
        };

        assert!(
            asking().await.is_err(),
            "a child that exited answers nothing"
        );
        let said = everything(&local);
        assert!(
            said.iter().any(|line| line.contains("closed")),
            "it wrote down what happened: {said:?}"
        );
        // and the first thing it said was that it was not ready, which is what a person waiting
        // on a checkpoint is owed - and there is no "ready" after it, because there never was
        assert!(said[0].contains("not ready yet"), "{said:?}");
        assert!(
            !said.iter().any(|line| line.contains("is ready")),
            "a child that never answered is never ready: {said:?}"
        );

        // and the second question is refused by the pipe rather than by the child
        let again = asking().await.expect_err("the advisor is gone");
        assert!(again.to_string().contains("no longer running"), "{again}");
    }
}
