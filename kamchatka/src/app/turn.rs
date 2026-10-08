//! Driving the kernel: starting a turn or a step, the task it runs in, what it reports when it
//! stops, and the requests a command sends out and finishes when they come back.

use std::time::Instant;

use nachalnik::{ContextItem, State};

use super::{
    App, Overlay, Speaker,
    text::{one_line, panicked, plural},
};

/// What the kernel's task reports when it stops, or what a command's request came back with.
pub enum Outcome {
    /// The turn ended in this state.
    Stopped(State),
    /// One transition happened, and produced this state.
    Stepped(State),
    /// It could not be finished.
    Failed(String),
    /// A command's request came back, and the command can be finished; see [`App::in_flight`].
    ///
    /// note: on the same channel as a turn's end rather than one of its own, because every loop
    /// already waits on this one and hands what it reads to [`App::on_outcome`] - so a loop learns
    /// nothing new to finish a command, and a command cannot be finished by a loop that forgot to
    /// listen for it. It is not a turn ending: nothing about the turn changes.
    Returned(Returned),
}

/// What a command's request came back with, for [`App::on_outcome`] to finish the command with.
///
/// note: opaque, because what it holds is the rest of the command - a closure over what came
/// back - and nothing but the `App` that sent the request has any business running it.
pub struct Returned {
    /// Which errand it answers; `None` for a switch, which is never stopped and so never stale.
    errand: Option<u64>,
    /// The rest of the command.
    finish: Box<dyn FnOnce(&mut App) + Send>,
}

/// A command's request that is still out, and how to stop waiting for it.
pub(super) struct Errand {
    /// Which one, so that an answer to one that was stopped is told from the one awaited.
    id: u64,
    /// What it is, for the line saying it was stopped.
    pub(super) what: &'static str,
    /// The request itself, which stopping aborts.
    pub(super) task: tokio::task::AbortHandle,
}

impl App {
    /// Starts, or carries on with, a turn.
    ///
    /// note: it refuses once the ceiling has been reached, and that refusal is what makes
    /// [`App::spend`] a bound rather than a report. Stopping the turn that crossed the line is
    /// only half of it: a loop that hands in the next line - a script, a person, an agent driving
    /// this from somewhere else - would start spending again, and every caller goes through here.
    pub fn start_turn(&mut self) {
        self.launch(false);
    }

    /// Performs exactly one transition of the state machine, and stops.
    ///
    /// note: this is the runtime's own shape, made visible. A turn is a loop over `step`, and
    /// running it a transition at a time is the only way to stand in [`State::Ready`] and look at
    /// what the model has asked for *before* any of it runs - which the kernel documents as a
    /// resting state on purpose, and which a whole turn walks straight through.
    pub fn start_step(&mut self) {
        self.launch(true);
    }

    /// A turn, or with `stepping` one transition of it, run in the background.
    pub(super) fn launch(&mut self, stepping: bool) {
        if self.busy || self.broke() || self.no_model() {
            return;
        }

        // with the message that starts the turn in it: a snapshot taken before this is the
        // session before the question, and a resume from one would lose the question
        self.keep_record();
        self.busy = true;
        self.oversized = false;
        self.since = Instant::now();
        self.stepping = stepping;
        self.interrupting = false;
        self.paused = false;
        #[cfg(feature = "shell-advisor")]
        if let Some(advised) = &self.advisor {
            advised.resume();
        }
        self.failed = None;
        let (kernel, outcomes) = (self.kernel.clone(), self.outcomes.clone());
        let turn = tokio::spawn(async move {
            // note: a stop asked for between the kernel finishing a turn and this `App` hearing
            // that it had is still standing, because the kernel keeps an interrupt on a resting
            // session for the next attempt to spend - and that attempt is this one, which would
            // then do nothing and say nothing. The ceiling does it whenever the response that
            // crosses it is the turn's last. `Finished` only: the kernel puts the flag down on the
            // way in, so one standing there came after the turn it was for, whereas one standing
            // at `Ready` is a stop for the calls this launch would otherwise carry on with
            if kernel.is_interrupted() && matches!(kernel.state(), State::Finished { .. }) {
                let _ = kernel.step().await;
            }
            let outcome = match stepping {
                true => kernel.step().await.map(Outcome::Stepped),
                false => kernel.turn().await.map(Outcome::Stopped),
            };
            outcome.unwrap_or_else(|e| Outcome::Failed(e.to_string()))
        });
        // note: the turn on a task of its own and the outcome sent from this one, because a turn
        // that panics sends nothing from inside itself. The kernel answers a tool's panic as a
        // failed call, so what reaches here is a panic in the kernel or in something this program
        // handed it, and unanswered it left `busy` set for good: the headless loop waited for
        // an outcome that never came and read no more input
        tokio::spawn(async move {
            let outcome = turn.await.unwrap_or_else(|e| {
                Outcome::Failed(match e.try_into_panic() {
                    Ok(panic) => format!("the turn panicked: {}", panicked(&*panic)),
                    Err(e) => format!("the turn was stopped: {e}"),
                })
            });
            let _ = outcomes.send(outcome);
        });
    }

    /// What one transition landed in, in a form somebody can act on.
    pub(super) fn stepped(&mut self, state: State) {
        let told = match &state {
            // what stepping is for: the calls are decided and about to run, and nothing has
            // happened yet
            State::Ready { calls } => {
                let waiting: Vec<String> = self
                    .kernel
                    .pending_calls()
                    .iter()
                    .map(|call| format!("    {} {}", call.tool, one_line(&call.args.to_string())))
                    .collect();
                format!(
                    "ready: {} call(s) decided, none of them run yet - /step runs them, /stop drops \
                     them\n{}",
                    calls.len(),
                    waiting.join("\n")
                )
            }
            State::Deciding { calls } => format!("deciding: {} waiting on you", calls.len()),
            State::Executing { calls } => format!("executing: {} running", calls.len()),
            State::Finished { stop, .. } => format!("finished: the model stopped, {stop:?}"),
            other => other.name().to_owned(),
        };

        self.say(Speaker::Note, format!("step → {told}"));
    }

    /// Says whatever the provider and the advisor have put up since the last look, and whether
    /// there was anything.
    ///
    /// note: each holds one notice and a newer one replaces it, so a loop has to look often -
    /// while a turn runs, and not only when it ends - or all but the last retry of a turn is lost
    /// and the last arrives after the failure it led up to. The loops call this on a tick; the
    /// places below call it where a notice is known to be about something that has just finished.
    pub fn take_notices(&mut self) -> bool {
        let mut heard = false;
        if let Some(notice) = self.provider.take_notice() {
            self.say(Speaker::Note, notice);
            heard = true;
        }
        // note: the advisor's beside the provider's. It is a second thing this session depends on
        // and cannot see
        #[cfg(feature = "shell-advisor")]
        if let Some(notice) = self.advisor.as_ref().and_then(|advised| advised.notice()) {
            self.say(Speaker::Note, notice);
            heard = true;
        }

        heard
    }

    /// Whether a command is still out at the endpoint: a switch settling, a listing, or a
    /// compaction pass being worked out.
    ///
    /// note: what a loop asks before handing in the next line, and what [`App::submit`] holds a
    /// line for. A command that waits on an endpoint used to wait *in* `submit`, holding the `App`
    /// and so the loop: a served session answered nobody, a drawn one stopped redrawing, and a
    /// headless deadline could not be reached. It is sent out instead and finished by
    /// [`App::on_outcome`] when it comes back - but the line after it must still not act on a
    /// session part-way through it: `/models` and then `/model` from the list, or a message on the
    /// line after a switch. So lines wait while one is out, and the loop goes on drawing, reading
    /// the kernel, and taking `ctrl+c`, which stops a listing or a pass. A switch is never stopped:
    /// half an `/endpoint` is a worse thing to leave than a wait.
    pub fn in_flight(&self) -> bool {
        self.settling.is_some() || self.errand.is_some()
    }

    /// Whether the session is doing anything: a turn running, or a command still out.
    ///
    /// note: what a client is told as `busy`, rather than the turn alone. A client whose input has
    /// closed leaves on `busy: false`, and a piped `/models` or `/compact` is answered the moment
    /// it is sent - so told only about the turn, it left before the list or the pass came back,
    /// and wrote out none of what it had asked for.
    pub fn working(&self) -> bool {
        self.busy || self.in_flight()
    }

    /// Whether a turn is running or a call is waiting to be answered: what
    /// [`text::MID_TURN`] says.
    ///
    /// note: a turn resting on a question is not `busy`, and it is still mid-turn: the call it is
    /// waiting on has a result to come, and anything put in the context now lands between the
    /// two.
    pub(super) fn mid_turn(&self) -> bool {
        self.busy || !self.kernel.pending_permissions().is_empty()
    }

    /// Whether there is anything for [`App::interrupt`] to stop: a turn, or a listing or a pass
    /// being worked out.
    #[cfg(feature = "tui")]
    pub(super) fn stoppable(&self) -> bool {
        self.busy || self.errand.is_some()
    }

    /// Hands in the lines [`App::submit`] held while a command was in flight, in order, until one
    /// of them sends out another.
    ///
    /// note: for the loops a line arrives at whenever somebody sends one: the drawn one, from the
    /// keys, and a served one, from its clients - each of which was answered `queued` when it
    /// was held, so that a client's next command, an interrupt among them, is not stuck behind it.
    /// The headless loop reads no line while a command is in flight.
    ///
    /// note: each is handed in as whoever sent it, keys or none. A page one of them opens for
    /// somebody with no screen to open it on is said instead, since the answer that would have
    /// carried it has gone. `true` where it handed anything in, which is a loop's cue to draw.
    pub async fn release(&mut self) -> bool {
        let mut released = false;
        // and not a switch while the turn it waits for is still running; see `App::submit`
        while !self.in_flight()
            && !(self.mid_turn()
                && self
                    .held
                    .front()
                    .is_some_and(|(line, _)| super::command::switches(line)))
            && let Some((line, keys)) = self.held.pop_front()
        {
            let had = std::mem::replace(&mut self.keys, keys);
            let reply = Box::pin(self.submitted(&line, true)).await;
            self.keys = had;
            if !keys && let Some(Overlay::Text { title, pages, .. }) = reply.page {
                let pages: Vec<String> = pages
                    .iter()
                    .map(|page| match page.name.is_empty() {
                        true => page.body.clone(),
                        false => format!("-- {} --\n{}", page.name, page.body),
                    })
                    .collect();
                self.say(
                    Speaker::Note,
                    format!("--- {} ---\n{}", title.trim(), pages.join("\n")),
                );
            }
            released = true;
        }

        released
    }

    /// Sends a command's request out, and has `then` finish the command when it comes back.
    pub(super) fn errand<T: Send + 'static>(
        &mut self,
        what: &'static str,
        work: impl Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut App, T) + Send + 'static,
    ) {
        self.errands += 1;
        let id = self.errands;
        let (_, task) = self.send_out(Some(id), what, work, then);
        self.errand = Some(Errand { id, what, task });
    }

    /// Sends a `/model` or `/endpoint` switch out, to settle while the loop carries on.
    pub(super) fn switch(&mut self, work: impl Future<Output = ()> + Send + 'static) {
        let (settling, _) = self.send_out(None, "the switch", work, |_, ()| {});
        self.settling = Some(settling);
    }

    /// Runs `work` as a task, and sends what it came back with to the loop as an
    /// [`Outcome::Returned`].
    ///
    /// note: a task watching a task, so that the answer is sent however the work ended. A request
    /// that panicked would otherwise send nothing, and every line after it would be held for ever
    /// behind a command that is never coming back; one that was stopped sends nothing on purpose,
    /// because whoever stopped it has already let go of it.
    pub(super) fn send_out<T: Send + 'static>(
        &mut self,
        errand: Option<u64>,
        what: &'static str,
        work: impl Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut App, T) + Send + 'static,
    ) -> (tokio::task::JoinHandle<()>, tokio::task::AbortHandle) {
        let outcomes = self.outcomes.clone();
        let inner = tokio::spawn(work);
        let stop = inner.abort_handle();
        let outer = tokio::spawn(async move {
            let finish: Box<dyn FnOnce(&mut App) + Send> = match inner.await {
                Ok(got) => Box::new(move |app: &mut App| then(app, got)),
                Err(e) if e.is_cancelled() => return,
                Err(_) => Box::new(move |app: &mut App| {
                    app.say(Speaker::Error, format!("{what} failed before it came back"))
                }),
            };
            let _ = outcomes.send(Outcome::Returned(Returned { errand, finish }));
        });

        (outer, stop)
    }

    /// Finishes the command a request came back for, unless it was stopped meanwhile.
    pub(super) fn returned(&mut self, returned: Returned) {
        match returned.errand {
            None => {
                self.settling = None;
                (returned.finish)(self);
                // the switch's own notice, which is about the switch: left for the next look it
                // lands after whatever the next line does
                self.take_notices();
            }
            Some(id) if self.errand.as_ref().is_some_and(|errand| errand.id == id) => {
                self.errand = None;
                (returned.finish)(self);
            }
            Some(_) => {}
        }
    }

    /// Waits for a `/model` or `/endpoint` still settling, and says what the switch had to say.
    ///
    /// note: the notice is taken here, the moment the switch is done, because it is about that
    /// switch: left for the next look, it lands after whatever the next line does, and a script's
    /// last line never has a next look at all.
    pub(super) async fn settled(&mut self, within: Option<std::time::Duration>) {
        let Some(settling) = self.settling.take() else {
            return;
        };
        match within {
            Some(bound) => {
                let _ = tokio::time::timeout(bound, settling).await;
            }
            None => {
                let _ = settling.await;
            }
        }
        self.take_notices();
    }

    /// Takes in the end of a turn.
    pub fn on_outcome(&mut self, outcome: Outcome) {
        if let Outcome::Returned(returned) = outcome {
            return self.returned(returned);
        }
        self.busy = false;
        self.close();
        self.keep_record();

        // note: before anything this says about the turn, because most of what a provider puts
        // here is *about* the turn that just ended - "the model was cut off mid-answer; what had
        // arrived is kept" is the account of the answer above it, and reads as a remark about the
        // next one if it lands after. The loops also look on a tick, and taking it twice costs
        // nothing.
        //
        // note: here rather than only in those loops, so that a notice is not something only this
        // program's own loops receive: an embedder driving `App` directly would otherwise never
        // hear that an answer was cut off.
        self.take_notices();

        // note: a turn that stopped to ask a question has not ended - the call it is asking about
        // still has a result to come - and a message pushed now would land between the call and
        // that result, which is a place a request cannot have one
        //
        // note: a failure has ended it as far as a waiting message is concerned, and so has a step
        // that came to rest. Left waiting, the message is overtaken by the next one sent, which
        // finds the session resting and runs at once - and goes in after that one's turn, or never
        // if there is no next turn. A step that stopped at `Ready` has calls still to run, and the
        // message waits for their results rather than landing between them and their calls
        let ended = match &outcome {
            Outcome::Stopped(state) => !matches!(state, State::Deciding { .. }),
            Outcome::Stepped(state) => matches!(state, State::Idle | State::Finished { .. }),
            Outcome::Failed(_) => true,
            // taken at the top, and never a turn's
            Outcome::Returned(_) => false,
        };
        // a `Stepped` outcome is somebody driving this a transition at a time, and a failure is
        // not the moment to start something else; either way what was typed waits for `/continue`
        let interrupted = std::mem::take(&mut self.interrupting);
        let carry_on = ended && !interrupted && matches!(outcome, Outcome::Stopped(_));
        match outcome {
            Outcome::Failed(e) => self.say_error(e),
            Outcome::Returned(_) => {}
            // note: a turn stopping to ask says nothing here, and opens nothing. The question is
            // drawn from `pending_permissions()` every frame, so there is no moment at which it
            // has to be put on the screen and none at which it has to be taken off, so nothing
            // can be left standing over a question that was answered somewhere else
            //
            // note: unless every question was answered while this outcome was on its way, in which
            // case the turn is carried on here. `permission.requested` is broadcast while the turn
            // that raised it is still unwinding, so an answer inside that window is recorded and
            // then goes nowhere: `App::decide` calls `start_turn`, `start_turn` refuses because the
            // old turn is still marked as running, and the session stops for good with every
            // question answered and nothing to answer. The window is narrow but real - it is
            // whatever the gap is between a client's socket and this loop. `headless.rs` stays
            // out of it by only answering while the kernel rests; the keys and a socket cannot,
            // because a person answers when they answer, so it is closed here instead, once, for
            // all three
            Outcome::Stopped(State::Deciding { .. })
                if !self.stepping && self.kernel.pending_permissions().is_empty() =>
            {
                self.start_turn();
                self.interrupting = interrupted;
            }
            Outcome::Stopped(State::Deciding { .. }) => {}
            // a turn that stops in `Idle` either ran out of requests or was asked to stop, and
            // the difference matters to whoever is reading the screen
            Outcome::Stopped(State::Idle) if !interrupted => {
                self.paused = true;
                let budget = self
                    .kernel
                    .config()
                    .max_requests_per_turn
                    .map(|max| plural(max, "request"))
                    .unwrap_or_else(|| "the requests".into());
                self.say(
                    Speaker::Note,
                    format!("the turn paused after {budget}; /continue to carry on"),
                );
            }
            Outcome::Stopped(_) => {}
            Outcome::Stepped(state) => self.stepped(state),
        }

        // a message somebody sent into this turn has waited for it to end; now the oldest goes in,
        // and unless the turn was stopped, stepped or failed it gets a turn of its own. The rest
        // wait for that one, as they waited for this
        //
        // note: `caught_up` because the line saying it was waiting is a live one - it was said
        // when there was no item to say it from - and pushing is what gives it one. The item is
        // drawn in its place, at the end of the conversation, which is where the request has it
        if ended && let Some(message) = self.typed_ahead.pop_front() {
            let id = self.kernel.push(ContextItem::user(message));
            self.caught_up(id);
            if carry_on {
                self.start_turn();
            }
        }

        // the counter has just been told what the last request really cost, so the figures on the
        // older items are out of date. Bringing them into line is a decision, not a side effect
        self.kernel.recount();
    }

    /// Writes the session down as far as it has got, where a record is being kept.
    ///
    /// note: every record the log has grown, and the snapshot too when the session is resting: a
    /// snapshot carries every item's content and a turn announces something every few
    /// milliseconds, so one per event mid-turn would write the whole context over and over for a
    /// snapshot that the end of the turn rewrites anyway. At rest the events are a person's, and
    /// each of them - an exclusion, a note, an undo - is a change the snapshot should show.
    ///
    /// note: a record that cannot be written stops being kept, and says so once. Saying it on every
    /// event would be a red line per event for the rest of the session, and the end of the run
    /// still tries once more, the way a session with no recorder is written.
    pub(super) fn keep_record(&mut self) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        let kept = match self.busy {
            true => recorder.append(&self.kernel),
            false => recorder.checkpoint(&self.kernel),
        };
        if let Err(e) = kept {
            self.recorder = None;
            self.say(
                Speaker::Error,
                format!(
                    "the record stopped being written, and will be tried again at the end: {e}"
                ),
            );
        }
    }
}
