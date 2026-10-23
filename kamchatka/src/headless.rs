//! Driving a session with nothing drawing it.
//!
//! This loop waits for a line; the loop in `main.rs` draws a frame and waits for a key. Both use
//! the same [`App`]: `submit` takes a line, a command or a message, and `on_event` takes what the
//! kernel says back.
//!
//! note: records go to one writer and prose to another. The records are the session log, one JSON
//! object per line, taken from [`Kernel::history_since`](nachalnik::Kernel::history_since) rather
//! than the broadcast, so they are the same bytes `/save` writes, in the same order, and they
//! include `session.started`, which is emitted while the kernel is still being constructed and
//! which no subscriber can catch. The prose is what the model said and what the program said about
//! the run. One goes to a pipe and the other to a terminal, so they are two writers rather than a
//! verbosity flag.
//!
//! note: this does not answer a question by asking somebody; there is nobody to ask. The answer
//! handed to [`Headless::new`] is the one every question gets, decided in advance on the command
//! line and recorded as a decision. `Grant::Deny` is the program's default because a run nobody is
//! watching should not be able to do a thing nobody has allowed.

use std::{io::Write, time::Duration};

use nachalnik::{Delta, Event, Grant, Kernel, Verdict};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt},
    sync::{broadcast, mpsc},
    time::{Instant, MissedTickBehavior},
};

use crate::app::{App, NOTICES, Outcome, Overlay, Speaker, text::plural};

/// Why a headless run stopped before it had worked through its input, for its exit status.
///
/// note: a run cut short used to leave with the status of one that finished, so a script could not
/// tell a session that did its work from one a deadline, a ceiling or a signal ended; the line
/// saying which was on standard error, where a script does not look. A failed turn is not one of
/// these: it is the error [`Headless::run`] returns, and the program's `1`.
///
/// note: `#[non_exhaustive]`, as every public enum in this workspace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    /// `--max-run-seconds` ran out.
    Deadline,
    /// The spend ceiling was reached.
    Spent,
    /// The last turn stopped at `--max-requests-per-turn`, with the model not done.
    Paused,
    /// `ctrl+c`.
    Interrupted,
    /// `SIGTERM`.
    Terminated,
    /// `SIGHUP`: the terminal went away.
    HungUp,
    /// The run ended as it should have, and its record could not be written.
    ///
    /// note: last in line, since it is found last: a run that stopped short for any of the reasons
    /// above leaves with that status whether or not it was written down.
    Unrecorded,
}

impl Stop {
    /// The exit status a run that stopped this way leaves with.
    ///
    /// note: the conventions a script already knows where there is one - `timeout(1)`'s `124` for a
    /// deadline, and `128` plus the signal for the three a signal ends: `130` for `ctrl+c`, `143`
    /// for `SIGTERM` and `129` for `SIGHUP` - and small numbers that nothing else here uses for the
    /// three that are this program's own. `1` is a failed turn and `2` is an argument clap refused.
    pub fn code(self) -> u8 {
        match self {
            Self::Spent => 3,
            Self::Paused => 4,
            Self::Unrecorded => 5,
            Self::Deadline => 124,
            Self::Interrupted => 130,
            Self::Terminated => 143,
            Self::HungUp => 129,
        }
    }
}

/// A session driven by lines rather than by keys.
pub struct Headless<'a> {
    /// What a question nobody is there to answer is answered with.
    on_ask: Grant,
    /// The session log, one JSON record per line: the same bytes `/save` writes.
    records: &'a mut dyn Write,
    /// The model's own words, and what the program has to say about the run.
    prose: Prose<&'a mut dyn Write>,
    /// How long the whole run may take, if anything says.
    deadline: Option<Duration>,
    /// Whether a `ctrl+c` stops the run rather than killing the process.
    ctrl_c: bool,
    /// Whether `SIGTERM` and `SIGHUP` end the session rather than the process.
    terminated: bool,
    /// Why the last run stopped short, if it did; see [`Headless::stopped`].
    stopped: Option<Stop>,
    /// Whether any of the answer being written has been printed as it arrived.
    streamed: bool,
    /// Fragments of the answer that went by too fast to print, counted until the answer they belong
    /// to has been said.
    ///
    /// note: held rather than said where it happened, because what was lost is worth saying beside
    /// the answer it was part of. A line about fragments going past between two other lines is
    /// about the typing; one arriving after the answer has finished is about an answer already
    /// read.
    missed: u64,
    /// Whether a model is answering, which is the only thing this loop prints fragments of.
    ///
    /// note: a `Lagged` carries a count and no events, so what went past cannot be sorted into
    /// kinds afterwards, and this decides whether a count is believed. `Requesting` and `Executing`
    /// are different states, so while a model is being answered no tool is running, and a count
    /// arriving in that window is fragments of words. A count arriving under a tool is that tool's
    /// output, which this loop does not print and the log does not hold.
    answering: bool,
}

impl<'a> Headless<'a> {
    /// One, writing its log to `records` and everything a person reads to `prose`.
    pub fn new(on_ask: Grant, records: &'a mut dyn Write, prose: &'a mut dyn Write) -> Self {
        Self {
            on_ask,
            records,
            prose: Prose::new(prose),
            deadline: None,
            ctrl_c: false,
            terminated: false,
            stopped: None,
            streamed: false,
            missed: 0,
            answering: false,
        }
    }

    /// Stops the run after this long.
    ///
    /// note: inside the loop rather than a `timeout` around it. A dropped future never reaches the
    /// end of `run`, so the session never finishes and its last records are never written. A
    /// deadline reached here interrupts the turn and records what arrived.
    ///
    /// note: a command waiting on an endpoint - `/models`, or a compaction pass - is stopped the
    /// same way, because it is sent out rather than awaited where its line was read; see
    /// `App::in_flight`. A `/model` or `/endpoint` switch finishes instead, under the bound the
    /// turn is waited for with, because a half-applied `/endpoint` is worse to leave behind than a
    /// late deadline.
    pub fn deadline(mut self, after: Duration) -> Self {
        self.deadline = Some(after);
        self
    }

    /// Takes `ctrl+c` as "stop" rather than letting it kill the process.
    ///
    /// note: off by default. This is a library loop, so taking a process-wide signal is the
    /// caller's decision, and a host with its own shutdown would find this competing with it.
    /// `kamchatka --headless` turns it on, because there it is the program.
    ///
    /// note: one stop rather than a kill, for the same reason as `esc`: an interrupted run has a
    /// half-answer, a tool result and a record worth keeping, and cooperative stopping lets them
    /// survive. A second `ctrl+c` leaves at once.
    pub fn stops_on_ctrl_c(mut self) -> Self {
        self.ctrl_c = true;
        self
    }

    /// Takes `SIGTERM` and `SIGHUP` as `/quit` rather than killing the process; see
    /// [`crate::stopping::Terminated`].
    ///
    /// note: off by default, for the same reason as `stops_on_ctrl_c`.
    pub fn leaves_when_terminated(mut self) -> Self {
        self.terminated = true;
        self
    }

    /// Why the last [`Headless::run`] stopped before finishing its input, or `None` if it finished.
    /// The program's exit status is made of this.
    pub fn stopped(&self) -> Option<Stop> {
        self.stopped
    }

    /// Reads lines, drives the kernel, and returns when there is nothing left of either.
    ///
    /// note: it ends when the input is closed and nothing is in flight, rather than at the end of
    /// the first turn. A piped question and a session held open by a script use the same code path,
    /// differing only in where the input came from.
    ///
    /// note: returns `Ok` for a run cut short and for one that finished, since neither is an error.
    /// [`Headless::stopped`] says which once this has returned.
    pub async fn run(
        &mut self,
        app: &mut App,
        events: &mut broadcast::Receiver<Event>,
        finished: &mut mpsc::UnboundedReceiver<Outcome>,
        input: impl AsyncBufRead + Unpin,
    ) -> Result<(), String> {
        // note: this loop defines a caller with no keys, so it says so rather than being told by
        // whoever built it. An embedder that drives `Headless` gets the same `/help` the program
        // does. See `App::keys`
        app.keys = false;
        self.stopped = None;
        // This is what a question comes to here, so nothing sends the model to a tool whose every
        // call would be refused for want of somebody to ask; see `Careful::reachable`
        app.policy.unanswered(Some(match self.on_ask {
            Grant::Allow => Verdict::Allow,
            Grant::Deny => Verdict::Deny,
        }));
        app.refresh_full_notice();

        let mut lines = Typed::new(input);
        let mut reading = true;
        // note: an instant rather than a duration, so it means the same thing however many times
        // round the loop it is waited on; taken once the run starts rather than when the driver was
        // built, since a caller may have held it for a while. One too far off to be an instant at
        // all is no deadline
        let mut ends = self
            .deadline
            .and_then(|after| Instant::now().checked_add(after));
        let mut stopping = false;
        // note: the deadline leaves the loop, as `/quit` does, rather than waiting for the turn to
        // stop. A turn whose tool does not look at its interrupt went on for as long as the tool
        // ran, past the deadline; left here, it is waited for under `LEAVING`
        let mut expired = false;
        // subscribed once, because a second press while the first is being handled means leave; see
        // `crate::stopping`
        //
        // note: only where asked for. Subscribing installs the process-wide handler for the life of
        // the process. A subscription beside a switched-off branch takes SIGINT from a caller who
        // never asked for it: the handler is installed, the branch never polls it, and the signal
        // goes nowhere. Subscribing also needs the runtime's signal driver, which comes with the io
        // driver, so subscribing unasked panics a host that built its runtime with `enable_time`
        // alone
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
        // note: the last one rather than any, because a failed turn carried on from is a recovered
        // session, and reporting it as failure makes every script treat one provider hiccup as a
        // dead session
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
        // whether it has said that messages are being passed over, for want of a ceiling to spend
        // or of room in a request
        let mut passing = false;
        // what a provider says while it waits, which no event carries
        let mut notices = tokio::time::interval(NOTICES);
        notices.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // note: an async block rather than a bare loop, so every exit - a `break`, an error writing
        // the prose or reading the input, or `/quit` - ends below, where a running turn is stopped
        // and waited for before `session.finished`
        let driven: Result<(), String> = async {
            loop {
                // before anything else, and wherever the question came from: a turn that stopped to
                // ask, or a `/step` that reached one. Answered in the branch that handles the
                // outcome, a question from `/step` would go unanswered, and a session whose input
                // had closed would sit in `select!` with nothing left that could ever wake it
                //
                // note: `!busy` is required. The question is broadcast as `permission.requested`
                // while the turn that raised it is still in flight, so a loop that answered on
                // sight would answer one the kernel had not finished asking - the decision
                // recorded, the outcome then arriving with nothing left waiting, and the turn never
                // carried on with. Answering only while the kernel rests is the rule the keys
                // follow, for the same reason
                if !app.busy && app.asked().is_some() {
                    self.answer(app)?;
                }
                // note: answer a running command's question on sight, busy or not, because it is
                // not the kernel's. It arrives while the call runs and holds the call until
                // answered, so waiting for the kernel to rest would mean waiting for the command
                // waiting on this
                if app.reached().is_some() {
                    self.answer_reaching(app)?;
                }
                self.flush(app, &mut written)?;
                self.echo(app, &mut said, &mut cleared)?;
                // a stop is said once, and a later one is a new stop
                if !app.overspent() && !app.oversized() {
                    passing = false;
                }
                // a session with nothing else to do and nothing in progress is over. `quit` is
                // `/quit`, as at a prompt. A command still in flight is something in progress: a
                // script whose last line is `/models` is asking for the list. Unless it was told to
                // stop, when what remains is a switch - a listing or a pass was stopped with the
                // turn - and the exit waits for that, under its own bound
                // a switch held for the end of a turn, which a line read while a question was
                // open can be; see `App::submit`. Nothing else is held here, because no line is
                // read while a command is out
                app.release().await;
                if app.leaving()
                    || expired
                    || (!reading && !app.busy && (!app.in_flight() || self.stopped.is_some()))
                {
                    break;
                }

                tokio::select! {
                    // note: `!busy` makes the pipe wait for the answer before reading the next
                    // line. Without it a script's lines are all read as soon as they are written,
                    // and two things go wrong: a command runs in the middle of the turn before it,
                    // so `/budget` lands above the answer it was asked after; and a message sent
                    // into a running turn waits in `App::typed_ahead` while the commands after it
                    // run at once, so the lines happen in an order nobody wrote. Nothing here can
                    // be typed during a turn, so nothing is lost by reading it after.
                    //
                    // note: also not while a command is out at the endpoint, for the same reason:
                    // the line after `/models` is often the `/model` it was asked for. See
                    // `App::in_flight`
                    line = lines.next_line(), if reading && !app.busy && !app.in_flight() => match line {
                        // note: as at the prompt, enter on an empty line is ignored and spaces
                        // round a line are trimmed. Otherwise a blank line down a pipe is sent as
                        // an empty message and answered, and `  /help` is a message here and a
                        // command there
                        Ok(Some((line, _))) if line.trim().is_empty() => {}
                        // note: an overspent session still reads lines, because the way back is a
                        // line: `/spend N` raises the ceiling and `/spend 0` takes it away, and a
                        // loop that stopped reading would drop the command its stop had just
                        // recommended. A message is different: `App::start_turn` refuses it, but
                        // only once it is in the context, where it would go out unasked with the
                        // next turn the script paid for. So a message is passed over here and
                        // reported once, not refused on every line for the rest of the script.
                        //
                        // note: the same for a session whose last turn was refused for a request
                        // longer than the model takes, while the next one would be too: a message
                        // cannot go out, and kept in the context it makes the request longer still.
                        // The way back is a command here as well - `/exclude`, `/limit` - so those
                        // are still read
                        Ok(Some((line, _)))
                            if (app.overspent() || app.oversized())
                                && !line.trim().starts_with('/') =>
                        {
                            if !std::mem::replace(&mut passing, true) {
                                app.say(
                                    Speaker::Note,
                                    match (app.overspent(), app.kernel.budget().limit) {
                                        (true, _) => "the ceiling is reached, so the messages after \
                                                 this point are passed over unsent; commands are \
                                                 still read, and `/spend N` raises it",
                                        (false, Some(_)) => "the next request is still longer than \
                                                  the model takes, so the messages after this \
                                                  point are passed over unsent; commands are \
                                                  still read, and `/budget` says how much has to \
                                                  go",
                                        // with no limit published there is no figure to name,
                                        // only the request that was refused
                                        (false, None) => "the endpoint refused the last request \
                                                  for its length and publishes no limit, so the \
                                                  messages after this point are passed over \
                                                  unsent until the context is smaller than that \
                                                  request; commands are still read, and \
                                                  `/exclude` makes room",
                                    },
                                );
                            }
                        }
                        Ok(Some((line, mangled))) => {
                            if mangled {
                                app.say(Speaker::Note, not_text(&line));
                            }
                            // the lines said here are printed by `echo` below, which watches
                            // `App::loose` for the ones that arrive with no line to answer; the
                            // page belongs to this call alone and has no other way out
                            let opened = app.submit(line.trim()).await.page;
                            // the line's effect on the context is said with it, not on a later
                            // pass - after the next line, or never if this was the last
                            for event in crate::app::queued(events) {
                                match event {
                                    Ok(event) => {
                                        self.say(&app.kernel, &event)?;
                                        app.on_event(event);
                                    }
                                    Err(missed) => self.lagged(missed),
                                }
                            }
                            if let Some(Overlay::Text { title, pages, .. }) = opened {
                                self.prose.fresh_line()?;
                                writeln!(self.prose, "--- {title} ---").map_err(|e| e.to_string())?;
                                // note: every page is printed, not just the one opened at. A screen
                                // turns pages with `←` and `→`, and there is no key to press down a
                                // pipe, so a caller handed one page of several cannot ask for the
                                // others.
                                //
                                // note: the page name is printed only when there is more than one
                                // page. A page opened by `App::preview` is nameless because there
                                // is one of it, and a rule saying nothing over `/budget` would be
                                // clutter
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
                                // goes either way. Where it is not, the line says so in place of the
                            // command reporting that it did something
                            if let Some(text) = app.clipboard.take()
                                && let Err(why) = crate::clipboard::hand_over(&text)
                            {
                                    app.not_copied(&why);
                            }
                            // note: `App` takes `/compact` when its pass comes back, because there
                            // are no keys here to answer the question with (see `App::planned`).
                            // Its list is said, so `echo` prints it
                        }
                        // stdin has closed. Whatever is running still finishes, and the loop leaves
                        // when it has. A script that pipes one question in and goes away is asking
                        // for the answer, not for the turn to be abandoned
                        Ok(None) => reading = false,
                        Err(e) => return Err(format!("could not read the input: {e}")),
                    },
                    event = events.recv() => match event {
                        Ok(event) => {
                            self.say(&app.kernel, &event)?;
                            app.on_event(event);
                        }
                        // note: records are read out of the log rather than from here, so a
                        // subscriber that fell behind has missed nothing that is written out. What
                        // it missed is the prose, the half nothing else keeps
                            // note: a tool's output arrives as fragments too, and none of them are
                            // ever on the prose. A headless run says what a tool was asked to do
                            // and what its result cost; the output between is the model's to read.
                            // Counting them once reported a loss of something nobody was going to
                            // be shown and pointed at a log without them, because a fragment is not
                            // an event anybody records. A count is only believed while a model is
                            // answering, the one thing this loop prints fragments of
                        Err(broadcast::error::RecvError::Lagged(missed)) => self.lagged(missed),
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    // note: a deadline that is not set waits on a future that never completes,
                    // which is what `select!` does with a branch that must never win. A
                    // precondition is the alternative, but a disabled branch is subtler to reason
                    // about than a future that is honestly never ready
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
                        expired = true;
                        self.stopped.get_or_insert(Stop::Deadline);
                        app.interrupt();
                        self.prose.fresh_line()?;
                        writeln!(self.prose, "· out of time; stopping")
                            .map_err(|e| e.to_string())?;
                    }
                    // note: a run that was not told to take `ctrl+c` waits on a future that is
                    // never ready, as the deadline branch does when no deadline was set, and for
                    // the same reason. A disabled branch would not work: `select!` evaluates the
                    // expression whether or not the branch is enabled, and the expression is where
                    // the subscription would be
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
                                self.stopped.get_or_insert(Stop::Interrupted);
                                app.interrupt();
                                self.prose.fresh_line()?;
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
                        for event in crate::app::queued(events) {
                            match event {
                                Ok(event) => {
                                    self.say(&app.kernel, &event)?;
                                    app.on_event(event);
                                }
                                Err(missed) => self.lagged(missed),
                            }
                        }
                        // a command coming back says nothing about how the last turn went
                        let returned = matches!(outcome, Outcome::Returned(_));
                        match &outcome {
                            Outcome::Failed(e) => failed = Some(e.clone()),
                            Outcome::Returned(_) => {}
                            _ => failed = None,
                        }
                        app.on_outcome(outcome);
                        // what a command did when it came back is said with it, as a line's is: a
                        // pass taken reports itself in an event, and a script whose last line was
                        // `/compact` would otherwise leave before that event was read
                        if returned {
                            for event in crate::app::queued(events) {
                                match event {
                                    Ok(event) => {
                                        self.say(&app.kernel, &event)?;
                                        app.on_event(event);
                                    }
                                    Err(missed) => self.lagged(missed),
                                }
                            }
                        }
                    }
                    // taken as `/quit`, which the check at the top of the loop acts on. It is
                    // reported as it arrives, not at the parting line, because a run ended by a
                    // signal is over at this point. A caller reading the tail of the stream must be
                    // able to tell it from one that finished its own work; a script reads status
                    // `128` and the signal. See `Stop`
                    ending = async {
                        match terminations.as_mut() {
                            Some(terminations) => terminations.which_arrived().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        app.quit = true;
                        self.stopped.get_or_insert(match ending {
                            crate::stopping::Ending::HungUp => Stop::HungUp,
                            _ => Stop::Terminated,
                        });
                        self.prose.fresh_line()?;
                        writeln!(self.prose, "· ended by a termination signal; leaving")
                            .map_err(|e| e.to_string())?;
                    }
                    // answered at the top of the loop, like the kernel's questions
                    Ok(()) = reaching.changed() => {}
                    // note: while a turn runs, because that is when a provider waits and says so.
                    // Read only when the turn ended: a retry's notice came out after the answer it
                    // held up, and of several retries only the last was said
                    _ = notices.tick(), if app.busy => {
                        app.take_notices();
                    }
                }
            }
            Ok(())
        }
        .await;

        // a failure to write the prose here is not a reason to leave a turn running: the records
        // are the part that is kept, and they are read out of the log below
        if !at_once {
            let kernel = app.kernel.clone();
            let waited = app
                .wait_for_turn(events, finished, |event| {
                    let _ = self.say(&kernel, event);
                })
                .await;
            failed = waited.or(failed);
        }
        // and what never went in, which `wait_for_turn` says on the way past and a second
        // `ctrl+c` did not wait for
        app.unsent();

        // note: the session is ended here rather than by the caller; this loop owns that one piece
        // of lifecycle. `session.finished` is a record like any other. A caller that ended the
        // session after this returned would have written every record but the last one down the
        // stream.
        app.kernel.finish();
        let flushed = self.flush(app, &mut written);
        // ends the last answer's own line, because nothing else will: a model that stops
        // mid-sentence, or on a closing fence, leaves the caller's parting line stuck to the end of
        // it. Ended before either error is returned, because the caller has a parting line for a
        // run that failed as well
        let ended = self.prose.fresh_line();
        driven?;
        flushed?;
        ended?;
        self.echo(app, &mut said, &mut cleared)?;

        // note: the first cause wins, since it is the one that cut the run short - a deadline that
        // interrupts the turn which would have crossed the ceiling ended the run by the clock. The
        // two read off the session at the end are the ones with no moment of their own here
        if self.stopped.is_none() {
            self.stopped = match (app.overspent(), app.paused()) {
                (true, _) => Some(Stop::Spent),
                (false, true) => Some(Stop::Paused),
                (false, false) => None,
            };
        }

        // note: the reason is already in the prose from when it happened; the caller needs the exit
        // code. A piped run whose model could never be reached must not exit `0`, or a script
        // reports a session that never happened as a success
        match failed {
            Some(_) => Err("the last turn failed".to_owned()),
            None => Ok(()),
        }
    }

    /// Says what the program has said since the last look, and prints whatever a command opened.
    ///
    /// note: this stops `/budget` from being silent. A command answers through [`App::say`] for a
    /// line and an overlay for a page of text, because the screen reads both. A caller with no
    /// screen must read them too, or every verb typed at it does nothing visible.
    ///
    /// note: only [`Speaker::Note`] and [`Speaker::Error`] - what the program said. The model's own
    /// words are printed from the fragments as they arrive and land in the same list, so echoing
    /// them as well would print every answer twice.
    ///
    /// note: pages are not read from here. A command's page comes back from `submit` itself, and
    /// the overlay it sets is left where it is: nothing in a headless run draws one, and taking it
    /// would be this loop keeping a screen's state tidy on a screen's behalf.
    ///
    /// note: which lines those are, and why `said` counts the filtered sequence rather than the
    /// list, are both [`App::notes`], because the other loop with no screen - the one in
    /// [`crate::remote`] - needs the same answer, and a rule stated twice tends to diverge.
    fn echo(&mut self, app: &App, said: &mut usize, cleared: &mut u64) -> Result<(), String> {
        // `/cleanup` empties the sequence this watermark indexes rather than shortening it, so the
        // mark goes back to zero. Nothing is printed: a pipe cannot take back output it has already
        // been handed, and the lines that would have said it are the ones that just went. See
        // `App::cleared`
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
            self.prose.fresh_line()?;
            writeln!(self.prose, "· {text}").map_err(|e| e.to_string())?;
        }

        self.prose.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Answers every question that is waiting for somebody who is not there, and carries on.
    ///
    /// note: it answers all of them rather than one, because a model that asks for three things at
    /// once produces three questions and the flag is the same answer to each.
    ///
    /// note: it goes through [`App::decide`] rather than the kernel, because answering is more than
    /// the kernel's decision. The sandbox must be told about a granted command that reaches the
    /// network; otherwise `--on-ask allow` would allow a `curl` and then run it with the network
    /// cut and no record of why. The turn must also be driven on afterwards: a decision leaves the
    /// kernel resting with nothing to drive it. If somebody asked to drive it one transition at a
    /// time with `/step`, answering must not silently run the rest of the turn. [`App::decide`]
    /// does all of this once for every caller.
    fn answer(&mut self, app: &mut App) -> Result<(), String> {
        for pending in app.kernel.pending_permissions() {
            let tool = pending.tool.clone();
            app.decide(pending.id, self.on_ask, false)
                .map_err(|e| format!("could not answer for `{tool}`: {e}"))?;
            self.prose.fresh_line()?;
            writeln!(
                self.prose,
                "· {tool}: {}, because nobody is here to be asked",
                self.on_ask
            )
            .map_err(|e| e.to_string())?;
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
            self.prose.fresh_line()?;
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

    /// Says that fragments of an answer went by too fast to print, if any did.
    ///
    /// note: said once, not once per moment the loop was behind. The count is everything that went
    /// past; otherwise a session falling behind on a long answer would be mostly notices.
    ///
    /// note: no word about the records. What is lost is unrecoverable from here: a fragment is in
    /// no record, and a record names a turn rather than copying it, so a notice claiming the log
    /// has it would send a reader after something that was never in it.
    fn said_missed(&mut self) -> Result<(), String> {
        let missed = std::mem::take(&mut self.missed);
        if missed == 0 {
            return Ok(());
        }
        self.prose.fresh_line()?;
        writeln!(
            self.prose,
            "· {missed} fragment(s) of an answer went by too fast to print; nothing here holds \
             them, and the record names that answer without holding it"
        )
        .map_err(|e| e.to_string())
    }

    /// Writes out every record the session has grown since the last time.
    fn flush(&mut self, app: &App, written: &mut u64) -> Result<(), String> {
        // nothing new, which is most events: see `Recorder::append_into`
        if app.kernel.last_seq() <= *written {
            return Ok(());
        }
        for record in app.kernel.history_since(*written) {
            let line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
            writeln!(self.records, "{line}").map_err(|e| e.to_string())?;
            *written = record.seq;
        }
        self.records.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Takes in that `missed` events went by unread; see the note where the stream is read.
    fn lagged(&mut self, missed: u64) {
        if self.answering {
            self.missed += missed;
        }
    }

    /// The part of an event a person watching wants to see go by.
    ///
    /// note: four, rather than `text::trace_line` over everything. The whole trace is in the
    /// records, and a session that printed all of it would bury the answer somebody is waiting for
    /// under the forty lines it took to get there.
    ///
    /// note: and no more than four. `App::on_event` already says something about a stop, a
    /// compaction and a failure, and those go out through `echo`, so an arm here for any of them
    /// would print the same news twice in two wordings. What is left is what nothing else says: the
    /// model's words, which `App` files under a speaker `echo` skips so that this one can stream
    /// them, and the two tool lines and a reference going in, which the terminal draws from the
    /// context and a headless run has no other sight of. A request prints nothing and ends the line
    /// the last answer left open, so that one answer does not run into the next.
    ///
    /// note: the model's words are the fragments, and the recorded turn where there were none. A
    /// provider that does not stream, or an endpoint that ignores being asked to, answers in one
    /// piece with no fragment ahead of it; the screen draws the item either way, but a run printing
    /// only fragments would print nothing of the answer.
    fn say(&mut self, kernel: &Kernel, event: &Event) -> Result<(), String> {
        // note: before anything else, because a `Lagged` arriving after this event has to be read
        // against it. A tool and a model are never both running, so what is in flight is the whole
        // of what a count could be about. A count arriving under a tool is that tool's output,
        // which nothing here prints
        self.answering = matches!(
            event,
            Event::ModelRequested { .. } | Event::ModelDelta { .. }
        );
        // no newline after a fragment: it arrives in pieces and is a sentence being written.
        // Everything below it is a whole line, so each ends that one first
        if let Event::ModelDelta {
            delta: Delta::Text(text),
        } = event
        {
            self.streamed = true;

            return self.prose.write_answer(text);
        }
        // a new request is a new answer and starts on a line of its own: the last one very likely
        // ended mid-line, and the first fragment of this one was written straight after it.
        //
        // note: the fragments that went by are said against the last answer, not this one, so what
        // went missing is named before the next thing starts rather than attributed to a sentence
        // that had not been written yet
        if matches!(event, Event::ModelRequested { .. }) {
            self.streamed = false;
            self.said_missed()?;

            return self.prose.fresh_line();
        }
        if let Event::ModelFinished { item, .. } = event {
            if std::mem::take(&mut self.streamed) {
                return self.said_missed();
            }
            let said = kernel
                .item(*item)
                .map(|turn| turn.content.to_text().into_owned())
                .unwrap_or_default();
            self.prose.write_answer(&said)?;

            return self.said_missed();
        }
        if let Some(line) = crate::app::text::went_in(event) {
            self.prose.fresh_line()?;

            return writeln!(self.prose, "· {line}").map_err(|e| e.to_string());
        }
        if !matches!(
            event,
            Event::ToolRequested { .. } | Event::ToolFinished { .. }
        ) {
            return Ok(());
        }
        self.prose.fresh_line()?;

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
                "· {tool}: {}{}",
                plural(*tokens, "token"),
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

/// What a person reads, and whether it is part-way through a line somebody else would finish.
///
/// note: the model's answer arrives in fragments and is printed as it does, so the last thing
/// written is nearly always half a sentence with no newline after it. Everything else that writes
/// here is a whole line, and without this the two run together: an answer of `4` would have the
/// closing line stuck to the end of it. The headless loop and a `--connect` client both read it, so
/// they share this.
pub(crate) struct Prose<W> {
    out: Printable<W>,
    mid_line: bool,
}

impl<W: Write> Prose<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out: Printable(out),
            mid_line: false,
        }
    }

    /// Ends whatever half-written line the model left, so a whole one can follow it.
    pub(crate) fn fresh_line(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.mid_line) {
            writeln!(self.out).map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Writes some of the model's answer, which is not a whole line and ends none.
    ///
    /// note: flushed as it is written, because the point of printing an answer as it arrives is
    /// lost on a writer that holds it until the line ends.
    pub(crate) fn write_answer(&mut self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        self.mid_line = !text.ends_with('\n');

        write!(self.out, "{text}")
            .and_then(|()| self.out.flush())
            .map_err(|e| e.to_string())
    }
}

impl<W: Write> Write for Prose<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.out.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// A person's half of the output with the control characters taken out, bar the newline and the
/// tab.
///
/// note: the prose is somebody's terminal, and most of what goes into it is not this program's to
/// vouch for - the model's words, a provider's error, an item's content read back. An escape
/// sequence among them is the terminal's to act on: clear the screen, retitle the window, set the
/// clipboard, draw a line that looks like one of this program's. The screen does not have the
/// problem, because the drawing library drops them, and this drops the same ones, the same way.
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

/// Lines read from an input, whatever bytes they contain.
///
/// note: it does not use `AsyncBufReadExt::lines`, which fails the whole stream at the first line
/// that is not UTF-8, so one Latin-1 byte in a script ended the session and dropped every line
/// after it. A line is taken whatever it holds, with `U+FFFD` where the bytes were not text, and
/// the caller is told, because what goes on is not quite what was typed.
///
/// note: it is as safe to drop mid-line as `lines` is, for the same reason: `read_until` appends
/// what it has read to a buffer this owns, so a `select!` that takes another branch loses nothing
/// and the next call carries on from where that one stopped.
pub(crate) struct Typed<R> {
    input: R,
    line: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> Typed<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            line: Vec::new(),
        }
    }

    /// The next line without its ending, and whether any of it had to be replaced; `None` once
    /// the input has closed.
    pub(crate) async fn next_line(&mut self) -> std::io::Result<Option<(String, bool)>> {
        if self.input.read_until(b'\n', &mut self.line).await? == 0 && self.line.is_empty() {
            return Ok(None);
        }
        let mut bytes = std::mem::take(&mut self.line);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }

        Ok(Some(match String::from_utf8(bytes) {
            Ok(line) => (line, false),
            Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
        }))
    }
}

/// What is said about a line [`Typed`] had to replace part of, naming it.
pub(crate) fn not_text(line: &str) -> String {
    format!(
        "a line that was not UTF-8 is read with `\u{FFFD}` where the bytes were: {}",
        crate::app::text::one_line(line)
    )
}

/// What a session that is about to be driven by lines should say for itself.
///
/// note: on the prose rather than in the context. It is the counterpart of the greeting a terminal
/// session opens with, and like that one it is addressed to whoever is reading rather than to the
/// model, so it is said through [`App::say`], which puts it where a `/save` will not mistake it for
/// something the model was told.
pub fn opening(app: &mut App, on_ask: Grant) {
    app.say(
        Speaker::Note,
        format!(
            "headless: a line is a message, a line starting with `/` is a command, and a question \
             nobody can be asked is answered `{on_ask}`"
        ),
    );
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    /// Half a line read before the input closed is the last line, not the end of the input.
    ///
    /// note: `read_until` keeps what it read in the buffer when the call is dropped part-way, so
    /// the next call can come back with nothing new and a line already half there - and a reader
    /// that took that for the end would drop what somebody had typed last.
    #[tokio::test]
    async fn half_a_line_left_when_the_input_closes_is_the_last_line() {
        let (mut typing, read) = tokio::io::duplex(64);
        let mut typed = Typed::new(tokio::io::BufReader::new(read));

        typing.write_all(b"look at the").await.expect("typed");
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(50), typed.next_line()).await;
        assert!(waited.is_err(), "a line with no end yet is not a line");

        drop(typing);
        let last = typed.next_line().await.expect("read");
        assert_eq!(last, Some(("look at the".to_owned(), false)));
        assert_eq!(typed.next_line().await.expect("read"), None);
    }

    /// Bytes that are not text still lose their control characters, and keep their lines and tabs.
    ///
    /// note: an escape sequence in prose drives somebody's terminal with the model's words,
    /// whatever else the bytes around it are.
    #[test]
    fn bytes_that_are_not_text_lose_their_control_characters() {
        let mut printed = Printable(Vec::new());
        printed
            .write_all(b"\x1b[2Jred\xff\tand\nmore")
            .expect("written");

        assert_eq!(printed.0, b"[2Jred\xff\tand\nmore");
    }
}
