//! Driving a session with nothing drawing it.
//!
//! The loop next door in `main.rs` draws a frame and waits for a key; this one waits for a line.
//! Everything between the two is the same [`App`]: `submit` takes the line a person would have
//! typed, a command or a message, `on_event` takes what the kernel says back, and the session,
//! the tools, the policy and the trace are where they always were.
//!
//! note: what it writes is split by who reads it. The **records** are the session log, one JSON
//! object per line, taken out of [`Kernel::history_since`](nachalnik::Kernel::history_since)
//! rather than off the broadcast - so they are the same bytes `/save` writes, in the same order,
//! and they include the `session.started` that is emitted while the kernel is still being
//! constructed and that no subscriber can ever catch. The **prose** is a person's half: what the
//! model said, and what the program had to say about the run. One goes to a pipe and the other to
//! a terminal, which is why they are two writers rather than a verbosity flag.
//!
//! note: what this does *not* do is answer a question by asking somebody. There is nobody to ask,
//! so the answer handed to [`Headless::new`] is the one every question gets - decided in advance
//! on the command line, and recorded as the decision it is. `Grant::Deny` is the default in the
//! program above, because a run nobody is watching should not be able to do a thing nobody has
//! allowed.

use std::{io::Write, time::Duration};

use nachalnik::{Delta, Event, Grant};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt},
    sync::{broadcast, mpsc},
    time::Instant,
};

use crate::app::{App, Outcome, Overlay, Speaker};

/// A session driven by lines rather than by keys.
pub struct Headless<'a> {
    /// What a question nobody is there to answer is answered with.
    on_ask: Grant,
    /// The session log, one JSON record per line: the same bytes `/save` writes.
    records: &'a mut dyn Write,
    /// The model's own words, and what the program has to say about the run.
    prose: Printable<&'a mut dyn Write>,
    /// How long the whole run may take, if anything says.
    deadline: Option<Duration>,
    /// Whether a `ctrl+c` stops the run rather than killing the process.
    ctrl_c: bool,
    /// Whether `SIGTERM` and `SIGHUP` end the session rather than the process.
    terminated: bool,
    /// Whether the prose is part-way through a line somebody else would finish.
    ///
    /// note: the model's answer arrives in fragments and is printed as it does, so the last thing
    /// written is nearly always half a sentence with no newline after it. Everything else that
    /// writes here is a whole line, and without this the two run together: an answer of `4` would
    /// have the closing line stuck to the end of it.
    mid_line: bool,
}

impl<'a> Headless<'a> {
    /// One, writing its log to `records` and everything a person reads to `prose`.
    pub fn new(on_ask: Grant, records: &'a mut dyn Write, prose: &'a mut dyn Write) -> Self {
        Self {
            on_ask,
            records,
            prose: Printable(prose),
            deadline: None,
            ctrl_c: false,
            terminated: false,
            mid_line: false,
        }
    }

    /// Stops the run after this long, however far it has got.
    ///
    /// note: inside the loop rather than a `timeout` around it. A dropped future never reaches the
    /// end of `run`, so the session would never be finished and the last records - the turn it was
    /// interrupted in among them - would be written nowhere. A deadline reached here interrupts
    /// the turn, lets what arrived be recorded, and leaves by the ordinary door.
    ///
    /// note: what it does not interrupt is a *command* of the operator's own that is waiting on an
    /// endpoint - `/models` fetches a list and `/model` and `/provider` finish a switch before the
    /// next line is read. Those are awaited inside the branch that read the line, so this branch
    /// and `ctrl+c` cannot be reached until they answer. The hole is narrow: the model's own turns
    /// are interruptible, which is where a run spends its time. Closing it means running a command
    /// as a task the loop can outlive, and a half-applied `/provider` is a worse thing to leave
    /// behind than a late deadline - see POSTPONED.md.
    pub fn deadline(mut self, after: Duration) -> Self {
        self.deadline = Some(after);
        self
    }

    /// Takes `ctrl+c` as "stop" rather than letting it kill the process.
    ///
    /// note: off by default. This is a library loop, and taking a process-wide signal is the
    /// caller's decision to make - a host with its own shutdown has one already and would find this
    /// competing with it. `kamchatka --headless` turns it on, because there it *is* the program.
    ///
    /// note: one stop rather than a kill, for the same reason `esc` is: a run interrupted has a
    /// half-answer, a tool result and a record worth keeping, and stopping cooperatively is what
    /// lets them survive. A second `ctrl+c` leaves at once.
    pub fn stops_on_ctrl_c(mut self) -> Self {
        self.ctrl_c = true;
        self
    }

    /// Takes `SIGTERM` and `SIGHUP` as `/quit` rather than letting them kill the process; see
    /// [`crate::stopping::Terminated`].
    ///
    /// note: off by default for the reason `stops_on_ctrl_c` is.
    pub fn leaves_when_terminated(mut self) -> Self {
        self.terminated = true;
        self
    }

    /// Ends whatever half-written line the model left, so a whole one can follow it.
    fn fresh_line(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.mid_line) {
            writeln!(self.prose).map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Reads lines, drives the kernel, and returns when there is nothing left of either.
    ///
    /// note: it ends when the input is closed *and* nothing is in flight, rather than at the end
    /// of the first turn. A single question piped in is then the same code path as a session held
    /// open by a script that sends another line when it has read the answer to the last, and the
    /// difference between them is where the pipe came from.
    pub async fn run(
        &mut self,
        app: &mut App,
        events: &mut broadcast::Receiver<Event>,
        finished: &mut mpsc::UnboundedReceiver<Outcome>,
        input: impl AsyncBufRead + Unpin,
    ) -> Result<(), String> {
        // note: this loop is the definition of a caller with no keys, so it says so rather than
        // being told by whoever built it - which means an embedder that drives `Headless` gets the
        // same `/help` the program does. See `App::keys`
        app.keys = false;

        let mut lines = input.lines();
        let mut reading = true;
        // note: an instant rather than a duration, so that it means the same thing however many
        // times round the loop it is waited on; and taken once the run starts rather than when
        // the driver was built, since a caller may have held it for a while
        let mut ends = self.deadline.map(|after| Instant::now() + after);
        let mut stopping = false;
        // subscribed once, because a second press arriving while the first is being handled is the
        // one that means leave; see `crate::stopping`
        //
        // note: and only where it was asked for. Subscribing is what installs the process-wide
        // handler, and it installs it for the life of the process - so a subscription taken beside
        // a branch that is switched off would take SIGINT away from a caller who never asked for
        // any of it: the handler is in, the branch never polls it, and the signal goes nowhere at
        // all. It also needs the runtime's signal driver, which comes with the io driver, so
        // subscribing unasked panics a host that built its runtime with `enable_time` alone
        let mut presses = self
            .ctrl_c
            .then(crate::stopping::Stopping::new)
            .transpose()
            .map_err(|e| format!("could not listen for ctrl+c: {e}"))?;
        let mut terminations = self
            .terminated
            .then(crate::stopping::Terminated::new)
            .transpose()
            .map_err(|e| format!("could not listen for a request to end: {e}"))?;
        // a second `ctrl+c`, which is the one way out that does not wait for a running turn
        let mut at_once = false;
        // note: the *last* one rather than any, because a turn that failed and was then carried on
        // from is a session that recovered, and a run that reported it as a failure would have
        // every script treating one provider hiccup as a dead session
        let mut failed = None;
        // where the log had got to the last time it was written out, so that nothing is written
        // twice and nothing is missed
        let mut written = 0;
        // and the same for the lines the program itself has said
        let mut said = 0;
        // and the generation that mark belongs to, since `/cleanup` starts the sequence again
        let mut cleared = app.cleared();
        // what wakes the loop for a running command's question, which is no event of the kernel's
        let mut reaching = app.policy.reaching().subscribe();

        // note: an async block rather than the loop alone, so that every way out of it - a
        // `break`, and an error writing the prose or reading the input as much as `/quit` - ends
        // up below, where a turn still running is stopped and waited for before `session.finished`
        let driven: Result<(), String> = async { loop {
            // before anything else, and wherever the question came from: a turn that stopped to
            // ask, or a `/step` that reached one. Answered in the branch that handles the outcome,
            // a question from `/step` would go unanswered, and a session whose input had closed
            // would sit in `select!` with nothing left that could ever wake it
            //
            // note: `!busy` is not optional. The question is broadcast as `permission.requested`
            // while the turn that raised it is still in flight, so a loop that answered on sight
            // would answer one the kernel had not finished asking - the decision recorded, the
            // outcome then arriving with nothing left waiting, and the turn never carried on with.
            // Answering only while the kernel rests is the same rule the keys follow, for the same
            // reason
            if !app.busy && app.asked().is_some() {
                self.answer(app)?;
            }
            // note: and a running command's question on sight, busy or not, because it is not the
            // kernel's. It arrives while the call runs and holds the call until it is answered, so
            // waiting for the kernel to rest would be waiting for the command that is waiting on
            // this
            if app.reached().is_some() {
                self.answer_reaching(app)?;
            }
            self.flush(app, &mut written)?;
            self.echo(app, &mut said, &mut cleared)?;
            // note: a session that has spent what it was given would refuse every further line
            // anyway - `App::start_turn` is where that is decided, so a caller cannot get round it
            // by not asking. What this adds is that the refusals are not printed one a line for
            // the rest of a script somebody piped in: there is nothing left for the input to do
            if app.overspent() {
                reading = false;
            }
            // a session that is not going to be given anything else to do, and is not doing
            // anything, is over. `quit` is `/quit`, which means the same here as at a prompt
            if app.leaving() || (!reading && !app.busy) {
                break;
            }

            tokio::select! {
                // note: `!busy` is what makes a pipe behave like somebody who waits for the
                // answer before typing the next thing. Without it a script's lines are all read
                // the moment they are written, and two things go wrong that a person at a prompt
                // never sees: a command runs in the middle of the turn before it, so `/budget`
                // lands above the answer it was asked after; and a *message* sent into a running
                // turn is held in `App::typed_ahead`, which holds one, so the third line of a
                // three-line script would quietly replace the second.
                // Nothing here can be typed during a turn, so nothing is lost by reading it after
                line = lines.next_line(), if reading && !app.busy => match line {
                    // note: what the prompt does with enter on nothing, and with spaces round a
                    // line. Otherwise a blank line down a pipe is sent as an empty message and
                    // answered - a request for nothing - and `  /help` is a message here and a
                    // command there
                    Ok(Some(line)) if line.trim().is_empty() => {}
                    Ok(Some(line)) => {
                        // the lines it said are printed by `echo` below, which is watching
                        // `App::loose` for the ones that arrive with no line to answer either;
                        // the page is this call's alone and has no other way out
                        let opened = app.submit(line.trim()).await.page;
                        if let Some(Overlay::Text { title, pages, .. }) = opened {
                            self.fresh_line()?;
                            writeln!(self.prose, "--- {title} ---").map_err(|e| e.to_string())?;
                            // note: every page rather than the one it was opened at. A screen
                            // turns them with `←` and `→` and there is no key to press down a
                            // pipe, so a caller handed one page of several would be reading a
                            // reference whose others it has no way to ask for.
                            //
                            // note: named only where there is more than one. A page opened by
                            // `App::preview` is deliberately nameless - there is one of it - and a
                            // rule saying nothing over `/budget` would be chrome for its own sake
                            for page in &pages {
                                if pages.len() > 1 {
                                    writeln!(self.prose, "-- {} --", page.name)
                                        .map_err(|e| e.to_string())?;
                                }
                                writeln!(self.prose, "{}", page.body)
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                        // `/copy` down a pipe is still worth answering: stdout is the session log
                        // and stderr may well be somebody's terminal, which is where the sequence
                        // goes either way. Where it is not, the line says so rather than the
                        // command reporting that it did something
                        if let Some(text) = app.clipboard.take()
                            && let Err(why) = crate::clipboard::hand_over(&text)
                        {
                            app.say(crate::app::Speaker::Note, why);
                        }
                        // `/compact` asks, and there are no keys here to answer with. Taken
                        // rather than left, which is the opposite of what `--on-ask` does with a
                        // tool's question - and the two are different questions. A tool's is the
                        // *model* asking to do something nobody vouched for, so the default is
                        // no; this one is the operator's own line, and a script that says
                        // `/compact` and is answered "left alone" has been refused the thing it
                        // asked for. The list is on stderr above it either way
                        if let Some(proposed) = app.proposed.clone() {
                            // the list itself, which on a screen is in the panel and down a pipe
                            // has nowhere else to go. Without it this mode takes items on the
                            // strength of a line saying how many, which is the opposite of what
                            // the command is for
                            self.fresh_line()?;
                            for row in &proposed.rows {
                                writeln!(self.prose, "· {row}").map_err(|e| e.to_string())?;
                            }
                            app.take_proposal(true).await;
                            // and what it did, in the same breath as what it proposed. The pass
                            // reports itself through an event like any other, and the loop would
                            // otherwise read that one on some later turn round - after the next
                            // line of the script, if there is one
                            while let Ok(event) = events.try_recv() {
                                self.say(&event)?;
                                app.on_event(event);
                            }
                        }
                    }
                    // stdin has closed. Whatever is running still finishes, and the loop leaves
                    // when it has: a script that pipes one question in and goes away is asking
                    // for the answer, not for the turn to be abandoned
                    Ok(None) => reading = false,
                    Err(e) => return Err(format!("could not read the input: {e}")),
                },
                event = events.recv() => match event {
                    Ok(event) => {
                        self.say(&event)?;
                        app.on_event(event);
                    }
                    // note: the records are read out of the log rather than from here, so a
                    // subscriber that fell behind has missed nothing that is written out. What it
                    // has missed is the *prose*, which is the half nothing else keeps
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        self.fresh_line()?;
                        writeln!(
                            self.prose,
                            "[{missed} fragment(s) went by too fast to print; the records have them]"
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                // note: a deadline that is not set waits on a future that never completes, which
                // is what `select!` does with a branch that must never win. The alternative is a
                // precondition, and a disabled branch is a subtler thing to reason about than a
                // future that is honestly never ready
                () = async {
                    match ends {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    // cleared, or the instant is in the past from here on and this branch wins
                    // every time round the loop for ever
                    ends = None;
                    reading = false;
                    app.interrupt();
                    self.fresh_line()?;
                    writeln!(self.prose, "· out of time; stopping")
                        .map_err(|e| e.to_string())?;
                }
                // note: a run that was not told to take `ctrl+c` waits here on a future that is
                // never ready, which is what the deadline above does with a deadline nobody set and
                // for the same reason. A disabled branch would not do: `select!` evaluates the
                // expression whether or not the branch is enabled, and the expression is where the
                // subscription would be
                () = async {
                    match presses.as_mut() {
                        Some(presses) => presses.pressed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match stopping {
                        // the second one: whatever is still running is somebody else's problem now
                        true => {
                            at_once = true;
                            break;
                        }
                        false => {
                            stopping = true;
                            reading = false;
                            app.interrupt();
                            self.fresh_line()?;
                            writeln!(
                                self.prose,
                                "· stopping; what has arrived is kept, and again leaves at once"
                            )
                            .map_err(|e| e.to_string())?;
                        }
                    }
                }
                Some(outcome) = finished.recv() => {
                    // the turn's last events are still queued behind this one, and `select!` picks
                    // whichever branch is ready rather than whichever happened first
                    while let Ok(event) = events.try_recv() {
                        self.say(&event)?;
                        app.on_event(event);
                    }
                    failed = match &outcome {
                        Outcome::Failed(e) => Some(e.clone()),
                        _ => None,
                    };
                    app.on_outcome(outcome);
                }
                // taken as `/quit`, which the check at the top of the loop then acts on
                () = async {
                    match terminations.as_mut() {
                        Some(terminations) => terminations.arrived().await,
                        None => std::future::pending().await,
                    }
                } => app.quit = true,
                // answered at the top of the loop, like the kernel's questions
                Ok(()) = reaching.changed() => {}
            }
        } Ok(()) }.await;

        // what the prose fails to say here is not a reason to leave a turn running: the records
        // are the part that is kept, and they are read out of the log below
        if !at_once {
            let waited = app
                .wait_for_turn(events, finished, |event| {
                    let _ = self.say(event);
                })
                .await;
            failed = waited.or(failed);
        }

        // note: the session is ended here rather than by the caller, and it is the one piece of
        // lifecycle this loop owns. `session.finished` is a record like any other, and a caller
        // that ended the session after this returned would have written every record but the last
        // one down the stream.
        app.kernel.finish();
        let flushed = self.flush(app, &mut written);
        driven?;
        flushed?;
        self.echo(app, &mut said, &mut cleared)?;
        // and the last answer's own line, which nothing else is going to end: a model that stops
        // mid-sentence, or on a closing fence, leaves the caller's parting line stuck to the end
        // of it
        self.fresh_line()?;

        // note: the reason is not repeated here. It has been on the prose since the moment it
        // happened, and what the caller wants from this is the exit code. A piped run whose model
        // could never be reached must not end in a `0`, or a script reports a session that never
        // happened as a success
        match failed {
            Some(_) => Err("the last turn failed".to_owned()),
            None => Ok(()),
        }
    }

    /// Says what the program has said since the last look, and prints whatever a command opened.
    ///
    /// note: this is what stops `/budget` from being silent. A command answers through
    /// [`App::say`] for a line and an overlay for a page of text, because the screen is what reads
    /// both - so a caller with no screen has to read them too, or every verb typed at it does
    /// nothing visible.
    ///
    /// note: only [`Speaker::Note`] and [`Speaker::Error`] - what the *program* said. The model's
    /// own words are printed from the fragments as they arrive, and they land in the same list, so
    /// echoing those as well would print every answer twice.
    ///
    /// note: pages are not read from here. A command's page comes back from `submit` itself, and
    /// the overlay it also sets is left where it is: nothing in a headless run draws one, and
    /// taking it would be this loop keeping a screen's state tidy on a screen's behalf.
    ///
    /// note: which lines those are, and why `said` counts the filtered sequence rather than the
    /// list, are both [`App::notes`], because the other loop with no screen - the one in
    /// [`crate::remote`] - needs the same answer, and a watermark rule stated twice will
    /// eventually be two.
    fn echo(&mut self, app: &App, said: &mut usize, cleared: &mut u64) -> Result<(), String> {
        // `/cleanup` empties the sequence this is a watermark into rather than shortening it, so the
        // mark goes back to nothing with it. Nothing is printed to say so: what a pipe has already
        // been handed cannot be taken back, and the lines that would have said it are the ones
        // that just went. See `App::cleared`
        if *cleared != app.cleared() {
            *cleared = app.cleared();
            *said = 0;
        }

        let fresh = app
            .notes(*said)
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>();
        *said += fresh.len();
        for text in fresh {
            self.fresh_line()?;
            writeln!(self.prose, "· {text}").map_err(|e| e.to_string())?;
        }

        self.prose.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Answers every question waiting on somebody who is not there, and carries on.
    ///
    /// note: it answers all of them rather than one, because a model that asks for three things at
    /// once produces three questions and the flag is the same answer to each.
    ///
    /// note: through [`App::decide`] rather than through the kernel, because answering is more
    /// than the kernel's decision. The sandbox has to be told about a granted command that reaches
    /// the network, or `--on-ask allow` allows a `curl` and then runs it with the network cut and
    /// no account of why. And the turn has to be driven on afterwards - a decision leaves the
    /// kernel resting with nothing to drive it, unless somebody asked to drive it a transition at
    /// a time with `/step`, in which case answering must not quietly run the rest of the turn.
    /// `decide` does all of it once, where every caller gets it.
    fn answer(&mut self, app: &mut App) -> Result<(), String> {
        for pending in app.kernel.pending_permissions() {
            let tool = pending.tool.clone();
            // read before the answer, because answering takes the question away
            let widened = app.widened(&pending);
            app.decide(pending.id, self.on_ask, false)
                .map_err(|e| format!("could not answer for `{tool}`: {e}"))?;
            self.fresh_line()?;
            writeln!(
                self.prose,
                "· {tool}: {}, because nobody is here to be asked",
                self.on_ask
            )
            .map_err(|e| e.to_string())?;
            // and why it was asked at all, where the answer is that the call named no operation.
            // This is the run where somebody wrote `--allow fs:read` and is watching every call be
            // refused, with nothing between the two saying why they never meet
            if let Some(widened) = widened {
                writeln!(self.prose, "  {widened}").map_err(|e| e.to_string())?;
            }
        }

        Ok(())
    }

    /// Answers every running command waiting to hear whether it may reach the network.
    ///
    /// note: through [`App::decide_reach`], for the reason [`Headless::answer`] goes through
    /// `App::decide`: the answer is written down there, and a loop that answered the command alone
    /// would leave the record with a command that reached out and nothing saying who let it.
    fn answer_reaching(&mut self, app: &mut App) -> Result<(), String> {
        for waiting in app.policy.reaching().waiting() {
            // the command may have ended in the moment since the look, which is nothing to report
            if app.decide_reach(waiting.id, self.on_ask, false).is_err() {
                continue;
            }
            self.fresh_line()?;
            writeln!(
                self.prose,
                "· `{}` reached for the network: {}, because nobody is here to be asked",
                crate::app::text::one_line(&waiting.cmd),
                self.on_ask
            )
            .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Writes out every record the session has grown since the last time.
    fn flush(&mut self, app: &App, written: &mut u64) -> Result<(), String> {
        for record in app.kernel.history_since(*written) {
            let line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
            writeln!(self.records, "{line}").map_err(|e| e.to_string())?;
            *written = record.seq;
        }
        self.records.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// The part of an event a person watching wants to see go by.
    ///
    /// note: three, rather than `text::trace_line` over everything. The whole trace is in the
    /// records, and a session that printed all of it would bury the answer somebody is waiting
    /// for under the forty lines it took to get there.
    ///
    /// note: and no more than three. `App::on_event` already says something about a stop, a
    /// compaction and a failure, and those go out through `echo` - so an arm here for any of them
    /// would print the same news twice in two wordings. What is left is what nothing else says: the
    /// model's words, which `App` files under a speaker `echo` skips precisely so that this one can
    /// stream them, and the two tool lines, which the terminal draws from the context and a
    /// headless run has no other sight of. A request prints nothing and ends the line the last
    /// answer left open, so that one answer does not run into the next.
    fn say(&mut self, event: &Event) -> Result<(), String> {
        // no newline after a fragment: this arrives in pieces and is a sentence being written.
        // Everything below it is a whole line, so each of them ends that one first
        if let Event::ModelDelta {
            delta: Delta::Text(text),
        } = event
        {
            self.mid_line = !text.ends_with('\n');

            return write!(self.prose, "{text}").map_err(|e| e.to_string());
        }
        // a new request is a new answer, which starts on a line of its own: the last one very
        // likely ended mid-line, and the first fragment of this one was written straight after it
        if matches!(event, Event::ModelRequested { .. }) {
            return self.fresh_line();
        }
        if !matches!(
            event,
            Event::ToolRequested { .. } | Event::ToolFinished { .. }
        ) {
            return Ok(());
        }
        self.fresh_line()?;

        match event {
            Event::ToolRequested { tool, args, .. } => {
                writeln!(self.prose, "⟩ {tool}({args})")
            }
            Event::ToolFinished {
                tool,
                is_error,
                tokens,
                ..
            } => writeln!(
                self.prose,
                "· {tool}: {tokens} tokens{}",
                match is_error {
                    true => ", an error",
                    false => "",
                }
            ),
            _ => Ok(()),
        }
        .map_err(|e| e.to_string())
    }
}

/// A person's half of the output with the control characters taken out, bar the newline and the
/// tab.
///
/// note: the prose is somebody's terminal, and most of what goes into it is not this program's to
/// vouch for - the model's words, a provider's error, an item's content read back. An escape
/// sequence among them is the terminal's to act on: clear the screen, retitle the window, set the
/// clipboard, draw a line that looks like one of this program's. The screen never had the problem,
/// because the drawing library drops them, and this drops the same ones, the same way.
///
/// note: over the bytes as they come, which is safe because a C0 byte never occurs inside a
/// multi-byte character. The C1 set is two bytes in UTF-8 and is taken out where the bytes are
/// text, which is every write here: each is a formatted string, whole.
pub(crate) struct Printable<W>(pub(crate) W);

impl<W: Write> Write for Printable<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let kept: Vec<u8> = match std::str::from_utf8(bytes) {
            Ok(text) => text
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                .collect::<String>()
                .into_bytes(),
            Err(_) => bytes
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_control() || matches!(b, b'\n' | b'\t'))
                .collect(),
        };
        self.0.write_all(&kept)?;

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// What a session that is about to be driven by lines should say for itself.
///
/// note: on the prose rather than in the context. It is the counterpart of the greeting a terminal
/// session opens with, and like that one it is addressed to whoever is reading rather than to the
/// model - so it is said through [`App::say`], which puts it where a `/save` will not confuse it
/// for something the model was told.
pub fn opening(app: &mut App, on_ask: Grant) {
    app.say(
        Speaker::Note,
        format!(
            "headless: a line is a message, a line starting with `/` is a command, and a question \
             nobody can be asked is answered `{on_ask}`"
        ),
    );
}
