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
    prose: &'a mut dyn Write,
    /// How long the whole run may take, if anything says.
    deadline: Option<Duration>,
    /// Whether a `ctrl+c` stops the run rather than killing the process.
    ctrl_c: bool,
    /// Whether the prose is part-way through a line somebody else would finish.
    ///
    /// note: the model's answer arrives in fragments and is printed as it does, so the last thing
    /// written is nearly always half a sentence with no newline after it. Everything else that
    /// writes here is a whole line, and without this the two run together - a live run ended
    /// `42026-09-11T09-46-56Z · 17 events recorded`, which is an answer of `4` with the closing
    /// line stuck to it.
    mid_line: bool,
}

impl<'a> Headless<'a> {
    /// One, writing its log to `records` and everything a person reads to `prose`.
    pub fn new(on_ask: Grant, records: &'a mut dyn Write, prose: &'a mut dyn Write) -> Self {
        Self {
            on_ask,
            records,
            prose,
            deadline: None,
            ctrl_c: false,
            mid_line: false,
        }
    }

    /// Stops the run after this long, however far it has got.
    ///
    /// note: inside the loop rather than a `timeout` around it, which is where this was and what
    /// it cost: a dropped future never reaches the end of `run`, so the session was never
    /// finished and the last records - the turn it was interrupted in among them - were written
    /// nowhere. A deadline reached here interrupts the turn, lets what arrived be recorded, and
    /// leaves by the ordinary door.
    pub fn deadline(mut self, after: Duration) -> Self {
        self.deadline = Some(after);
        self
    }

    /// Takes `ctrl+c` as "stop" rather than letting it kill the process.
    ///
    /// note: off by default, and that is not timidity. This is a library loop, and taking a
    /// process-wide signal is the caller's decision to make - a host with its own shutdown has
    /// one already and would find this competing with it. `kamchatka --headless` turns it on,
    /// because there it *is* the program.
    ///
    /// note: one stop rather than a kill, for the same reason `esc` is: a run interrupted has a
    /// half-answer, a tool result and a record worth keeping, and the whole point of stopping
    /// cooperatively is that they survive. A second `ctrl+c` leaves at once.
    pub fn stops_on_ctrl_c(mut self) -> Self {
        self.ctrl_c = true;
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
        let mut lines = input.lines();
        let mut reading = true;
        // note: an instant rather than a duration, so that it means the same thing however many
        // times round the loop it is waited on; and taken once the run starts rather than when
        // the driver was built, since a caller may have held it for a while
        let mut ends = self.deadline.map(|after| Instant::now() + after);
        let mut stopping = false;
        // where the log had got to the last time it was written out, so that nothing is written
        // twice and nothing is missed
        let mut written = 0;
        // and the same for the lines the program itself has said
        let mut said = 0;

        loop {
            // before anything else, and wherever the question came from: a turn that stopped to
            // ask, or a `/step` that reached one. It used to be answered in the branch that
            // handled the outcome, which left the other way in unanswered - and then the exit
            // below had to make an exception for a waiting question, so a session whose input had
            // closed sat in `select!` with nothing left that could ever wake it
            //
            // note: `!busy` is load-bearing and was bought by three failing tests. The question is
            // broadcast as `permission.requested` while the turn that raised it is still in
            // flight, so a loop that answered on sight answered one the kernel had not finished
            // asking - the decision was recorded, the outcome then arrived with nothing left
            // waiting, and the turn was never carried on with. Answering only while the kernel
            // rests is the same rule the keys follow, for the same reason
            if !app.busy && app.asked().is_some() {
                self.answer(app)?;
            }
            self.flush(app, &mut written)?;
            self.echo(app, &mut said)?;
            // a session that is not going to be given anything else to do, and is not doing
            // anything, is over. `quit` is `/quit`, which means the same here as at a prompt
            if app.quit || (!reading && !app.busy) {
                break;
            }

            tokio::select! {
                // note: `!busy` is what makes a pipe behave like somebody who waits for the
                // answer before typing the next thing. Without it a script's lines are all read
                // the moment they are written, and two things go wrong that a person at a prompt
                // never sees: a command runs in the middle of the turn before it - a live run put
                // the whole of `/budget` above the answer it was asked after - and a *message*
                // sent into a running turn is held in `App::typed_ahead`, which holds one, so the
                // third line of a three-line script would have quietly replaced the second.
                // Nothing here can be typed during a turn, so nothing is lost by reading it after
                line = lines.next_line(), if reading && !app.busy => match line {
                    Ok(Some(line)) => {
                        // the lines it said are printed by `echo` below, which is watching
                        // `App::loose` for the ones that arrive with no line to answer either;
                        // the page is this call's alone and has no other way out
                        let opened = app.submit(line.trim_end()).await.page;
                        if let Some(Overlay::Text { title, pages, page, .. }) = opened {
                            let body = pages.get(page).map(|it| it.body.as_str()).unwrap_or("");
                            self.fresh_line()?;
                            writeln!(self.prose, "--- {title} ---\n{body}")
                                .map_err(|e| e.to_string())?;
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
                _ = tokio::signal::ctrl_c(), if self.ctrl_c => {
                    match stopping {
                        // the second one: whatever is still running is somebody else's problem now
                        true => break,
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
                    app.on_outcome(outcome);
                }
            }
        }

        // note: the session is ended here rather than by the caller, and it is the one piece of
        // lifecycle this loop owns. `session.finished` is a record like any other, and a caller
        // that ended the session after this returned would have written every record but the last
        // one - which a live run against a local model is exactly how this was found: sixteen
        // records on the stream under a closing line that said seventeen.
        app.kernel.finish();
        self.flush(app, &mut written)?;
        self.echo(app, &mut said)?;
        // and the last answer's own line, which nothing else is going to end: a model that stops
        // mid-sentence - or on a closing fence, which is where this was found - leaves the caller's
        // parting line stuck to the end of it
        self.fresh_line()?;

        Ok(())
    }

    /// Says what the program has said since the last look, and prints whatever a command opened.
    ///
    /// note: this is what stops `/budget` from being silent. A command answers through
    /// [`App::say`] for a line and an overlay for a page of text, because the screen is what reads
    /// both - so a caller with no screen has to read them too, and the alternative was a mode in
    /// which messages worked and every verb typed at it did nothing visible.
    ///
    /// note: only [`Speaker::Note`] and [`Speaker::Error`] - what the *program* said. The model's
    /// own words are printed from the fragments as they arrive, and they land in the same list, so
    /// echoing those as well would print every answer twice.
    ///
    /// note: pages are not read from here. A command's page comes back from `submit` itself, and
    /// the overlay it also sets is left where it is: nothing in a headless run draws one, and
    /// taking it would be this loop keeping a screen's state tidy on a screen's behalf.
    fn echo(&mut self, app: &App, said: &mut usize) -> Result<(), String> {
        let fresh = app.loose[(*said).min(app.loose.len())..]
            .iter()
            .map(|entry| (entry.speaker, entry.text.clone()))
            .collect::<Vec<_>>();
        for (speaker, text) in fresh {
            if matches!(speaker, Speaker::Note | Speaker::Error) {
                self.fresh_line()?;
                writeln!(self.prose, "· {text}").map_err(|e| e.to_string())?;
            }
        }
        *said = app.loose.len();

        self.prose.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Answers every question waiting on somebody who is not there, and carries on.
    ///
    /// note: it answers all of them rather than one, because a model that asks for three things at
    /// once produces three questions and the flag is the same answer to each. The turn is started
    /// again afterwards for the same reason the keys do it: a decision leaves the kernel resting,
    /// and nothing else is going to drive it - unless somebody asked to drive with `/step`, in
    /// which case answering must not quietly run the rest of the turn here either.
    fn answer(&mut self, app: &mut App) -> Result<(), String> {
        for pending in app.kernel.pending_permissions() {
            let tool = pending.tool.clone();
            app.kernel
                .decide(pending.id, self.on_ask)
                .map_err(|e| format!("could not answer for `{tool}`: {e}"))?;
            self.fresh_line()?;
            writeln!(
                self.prose,
                "· {tool}: {}, because nobody is here to be asked",
                self.on_ask
            )
            .map_err(|e| e.to_string())?;
        }
        if !app.stepping {
            app.start_turn();
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
    /// note: and three rather than six. `App::on_event` already says something about a stop, a
    /// compaction and a failure, and those go out through `echo` - so an arm here for any of them
    /// printed the same news twice in two wordings. A live run ended `out of time; stopping`,
    /// `stopped; whatever arrived is kept` and `stopped`, which is one piece of information and
    /// three lines. What is left is what nothing else says: the model's words, which `App` files
    /// under a speaker `echo` skips precisely so that this one can stream them, and the two tool
    /// lines, which the terminal draws from the context and a headless run has no other sight of.
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
