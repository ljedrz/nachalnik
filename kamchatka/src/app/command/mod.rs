//! The slash commands: everything typed at the prompt that is not a message.
//!
//! note: a command is answered here and now rather than turned into anything the kernel has to know
//! about. `/seams`, `/budget` and `/request` read public values off a [`nachalnik::Kernel`] and
//! print them; nothing in this file is a capability the runtime had to grow.
//!
//! note: this file reads the line and dispatches it, and answers the commands that are about the
//! session itself; the rest are answered by topic, in `model.rs`, `context.rs` and `tools.rs`.

use nachalnik::{Role, State, StopReason};

use crate::app::text::thousands;

use super::{
    App, Did, Reply, Speaker,
    text::{nothing_to_send, one_line, plural, pretty, request_preview},
};

mod context;
mod model;
mod tools;

impl App {
    /// Sends a message, or runs a command: one line of what somebody types at the prompt.
    ///
    /// note: `pub` because the prompt is not the only caller. Every verb - `/model`, `/exclude`,
    /// `/limit`, `/step`, `/save`, `/load`, `/tools toggle` - is reachable only through here;
    /// without it, the only way in would be to synthesize a key press. A non-terminal caller hands
    /// the same line to the same function.
    ///
    /// note: output goes to [`App::say`] for a line and `App::preview` for a page, and comes back
    /// in the [`Reply`], because those answer two different questions. A screen re-reads
    /// [`App::loose`] and [`App::overlay`] every frame and wants the whole of both; a caller
    /// answering one line wants what that line produced without watching both change. The copy is
    /// cheap; the alternative is a watermark kept by every caller.
    pub async fn submit(&mut self, line: &str) -> Reply {
        self.submitted(line, false).await
    }

    /// [`App::submit`], for a line that was held and is now `released`: one that has already
    /// waited its turn in the queue, and is not held behind what is still in it.
    pub(super) async fn submitted(&mut self, line: &str, released: bool) -> Reply {
        let (from, pages) = (self.loose.len(), self.previews);
        // a command still out at the endpoint finishes before this line is read, so nothing acts on
        // a session part-way through changing model, or on a list nobody has been shown yet. Held
        // rather than waited for, so the loop goes on; see `App::in_flight` and `App::release`.
        //
        // note: except `/stop`, which ends the wait. Held, it stopped a turn only once the listing
        // it was held behind came back, which is what it was typed to avoid
        if self.in_flight() && line.trim() != "/stop" {
            self.held
                .push_back((line.to_owned(), self.keys, self.remote));
            self.say(
                Speaker::Note,
                "this waits for the command before it to come back; `ctrl+c` stops a listing or a \
                 compaction pass",
            );

            return self.replied(Did::Queued, from, pages);
        }
        // note: a switch of model or endpoint typed into a running turn waits for the end of it, as
        // a message does, rather than changing the model under a request in flight. The rest of the
        // turn would go to a model it did not start with, and the counter's calibration was reset
        // under it. Held with the lines that wait for a command, and released by the same loop once
        // the turn is over; see `App::release`. What is typed after it waits behind it, so a
        // message sent after a switch is answered by the model switched to
        if !released
            && line.trim() != "/stop"
            && (self.held_switch() || self.mid_turn() && switches(line))
        {
            self.held
                .push_back((line.to_owned(), self.keys, self.remote));
            self.say(
                Speaker::Note,
                match switches(line) && self.held.len() == 1 {
                    true => "this switches when the turn ends",
                    false => {
                        "this waits for the switch before it, which happens when the turn ends"
                    }
                },
            );

            return self.replied(Did::Queued, from, pages);
        }
        // whatever this line turns into, the time before it was somebody deciding what to type.
        // The next line the trace draws is the one that gap belongs to
        self.acted = true;
        // and it is the line `up` puts back, whatever it turns out to be. Kept here rather than
        // read off the newest user item, because they are not the same thing: a command never
        // becomes an item, and an item can be rewritten afterwards. The item answers "what does the
        // context say now"; somebody pressing `up` wants the line they typed
        self.last_sent = Some(line.to_owned());

        if let Some(command) = line.strip_prefix('/') {
            self.command(command).await;

            return self.replied(Did::Ran, from, pages);
        }

        // note: a message sent while a turn is running waits for the end of it rather than going
        // into the context there and then. Pushed immediately, it lands before the answer the model
        // is still writing, so the next request ends with a model turn; Google refuses that with
        // `requests ending with a model turn are not supported`, and other providers answer by
        // replying to themselves. Pushed mid-tool-loop, it lands between an assistant's call and
        // that call's result, a shape most of these APIs reject. The cost is that a message typed
        // to steer a turn is answered after it, not during. The same holds while a question is
        // open: the turn is paused rather than over, and the call it is waiting on still has a
        // result to come.
        //
        // note: checked before the line is said rather than after, so the only path that says a
        // message without an item to tie it to is the only path with no item yet. Everywhere else
        // goes through `App::ask`, which cannot forget the third step
        if self.mid_turn() {
            self.typed_ahead.push_back(line.to_owned());
            self.follow = true;
            // said out loud, because until the turn ends this is the one thing on the screen the
            // context does not have: a session saved now would not contain it
            let ahead = self.typed_ahead.len() - 1;
            self.say(
                Speaker::Note,
                match ahead {
                    0 => "this goes in when the turn stops, and gets a turn of its own".to_owned(),
                    _ => format!(
                        "this goes in after the {} waiting ahead of it, and gets a turn of its own",
                        plural(ahead, "message")
                    ),
                },
            );

            return self.replied(Did::Queued, from, pages);
        }

        // note: a session resting with messages still waiting keeps their order. The turn they
        // waited for was stopped or failed, which takes one in without a turn and leaves the rest.
        // This line goes behind them and the oldest goes in now, rather than this one overtaking
        // every line sent before it
        if let Some(oldest) = self.typed_ahead.pop_front() {
            self.typed_ahead.push_back(line.to_owned());
            self.say(
                Speaker::Note,
                format!(
                    "this goes in after the {} waiting ahead of it, and the first of those goes \
                     in now",
                    plural(self.typed_ahead.len(), "message")
                ),
            );
            self.ask(&oldest);
            self.start_turn();

            return self.replied(Did::Queued, from, pages);
        }

        // this is all "sending a message" is: one context item, and then the loop
        let id = self.ask(line);
        self.start_turn();

        self.replied(Did::Asked(id), from, pages)
    }

    /// Whether a switch of model or endpoint is held for the end of the turn.
    pub(super) fn held_switch(&self) -> bool {
        self.held.iter().any(|(line, ..)| switches(line))
    }

    /// What the lines said and the pages opened since `from` and `pages` add up to.
    fn replied(&self, did: Did, from: usize, pages: usize) -> Reply {
        Reply {
            did,
            said: self.loose[from.min(self.loose.len())..].to_vec(),
            // the page this call opened, which is the overlay only if this call is what put it
            // there; see the note on `App::previews`
            page: (self.previews > pages)
                .then(|| self.overlay.clone())
                .flatten(),
        }
    }

    /// Why a turn started without a message would send the model nothing new, if it would.
    ///
    /// note: read off the projection rather than off the state, because the state does not know. A
    /// request ending on the model's own answer, with no call left for a result to follow, is that
    /// answer asked for again: the model repeats itself at the price of a request, and some
    /// endpoints refuse a request ending on a model turn outright. `/load` and `-r` leave the
    /// machine `Idle` over a conversation ending that way, and a bare `/step` after a finished turn
    /// is the same request as a `/continue` after one. A turn whose answer was excluded ends on the
    /// question again, and has something to answer.
    ///
    /// note: except an answer that was cut short - out of room, or interrupted - while the machine
    /// still names it as where the turn stopped. A request ending on that answer is how carrying on
    /// from it is built. A saved answer does not say why it ended, so after a `/load` it counts as
    /// finished: asking for the rest is one message, and a repeat is a request.
    fn nothing_to_answer(&self) -> Option<&'static str> {
        let state = self.kernel.state();
        // resting on calls or on a question, a step runs the one or waits on the other, and sends
        // nothing either way
        if !matches!(state, State::Idle | State::Finished { .. }) {
            return None;
        }
        let projection = self.kernel.project();
        // note: said here rather than left to the kernel's `EmptyProjection`, which comes back as a
        // failed turn. A headless run ending on one exits `1` for a request never sent
        let Some(last) = projection.messages.last() else {
            return Some(
                "nothing in the context would be sent, so there is nothing to answer; a message \
                 is what starts a turn",
            );
        };
        if last.role != Role::Assistant || last.calls().next().is_some() {
            return None;
        }

        match state {
            State::Finished { item, stop }
                if projection.included.last() == Some(&item)
                    && !matches!(stop, StopReason::EndTurn | StopReason::Refusal) =>
            {
                None
            }
            _ => Some(
                "the conversation ends on the model's own answer, so there is nothing to \
                 continue; a message is what starts the next turn",
            ),
        }
    }

    /// Runs the rest of the turn unless there is nothing to answer: `/continue`, and `ctrl+r` where
    /// there are keys to press.
    pub(super) fn carry_on(&mut self) {
        match self.nothing_to_answer() {
            Some(why) => self.say(Speaker::Note, why),
            None => self.start_turn(),
        }
    }

    /// Runs one slash command.
    async fn command(&mut self, line: &str) {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();

        match command {
            "quit" | "exit" | "q" => self.quit = true,
            // note: a flag and not the work, for the reason `quit` is one. What a restart rebuilds
            // is the `App` this is a method on - a kernel, a policy, the tools and the handle they
            // reach it through - and a method cannot replace the thing it was called on. The loop
            // owns it and the loop puts the new one in its place; see `App::restart`
            "restart" => self.restart(),
            "help" | "?" => self.help(),
            // note: the same function `u` and `U` reach on the context tab, and the same line
            // afterwards. A run with no keys to press - down a pipe, through `--connect`, from a
            // browser - had no way to take a change back, and the program's undo messages named the
            // key rather than the line. The key is the shorthand; the command is the verb.
            //
            // note: a count after either is refused rather than dropped. `/undo 5` answered
            // `undone` for the one change it took back, which reads as five
            "undo" | "redo" if !rest.is_empty() => self.say(
                Speaker::Error,
                format!(
                    "`/{command}` takes one change at a time and reads nothing after it, so \
                     `{rest}` was not read and nothing was {}; `/{command}` once for each change",
                    match command {
                        "undo" => "undone",
                        _ => "redone",
                    }
                ),
            ),
            "undo" => self.undo(false),
            "redo" => self.undo(true),
            // note: not where there is nothing new for the model to answer; see
            // `App::nothing_to_answer`
            "continue" => self.carry_on(),
            // note: the same act as `esc` and `ctrl+c`, reached by typing, which is the only way to
            // reach it from a browser: a page has no keys to send and `Command::Interrupt` is a
            // button nothing was obliged to draw. Two loops here have a line and nothing else, and
            // `/step` points at this name when it declines.
            //
            // note: four answers, because a session has four states here and two of them have
            // something to stop. A turn resting in `Ready` has calls decided and nothing running to
            // interrupt, and stopping it drops them; see `App::drop_decided`. A turn resting on a
            // question is not `busy` - resting is what lets anybody answer it - and an interrupt
            // does not reach it either: nothing is running to notice one, so the question is still
            // there afterwards. What ends that turn is answering, which is `n` at a terminal and
            // the deny button on a page. This branch says so, because `nothing is running` would be
            // false in a state where somebody plainly has something.
            //
            // note: silent where it interrupted, as `esc` is. What says a turn stopped is the turn
            // stopping - the records, the state, the spinner going out - and a line claiming it as
            // well would be this program reporting its own keystroke. The other branch is not
            // silent, because a typed command is not a key: a press that lands on nothing is a
            // press somebody can see missing, and a line typed into a session that was not running
            // looks exactly like one that was ignored
            "stop" => match (self.busy, !self.kernel.pending_permissions().is_empty()) {
                // a listing or a pass still out is stopped with the turn, or alone
                (true, _) => self.interrupt(),
                _ if self.errand.is_some() => self.interrupt(),
                (false, true) => self.say(
                    Speaker::Note,
                    "the turn is waiting on a question, which stopping does not answer; deny it \
                     to end the turn there",
                ),
                (false, false) if self.ready() => self.drop_decided(),
                (false, false) => self.say(Speaker::Note, "nothing is running"),
            },
            // note: a command as well as `ctrl+l`, because two of the three loops have no keys to
            // press and one of them is the browser, where a pile of notices is the whole screen
            // rather than a quarter of a tall one. It says nothing when it is done, which is
            // `clear_notices`' own rule: a line reporting that the lines are gone would be the
            // first line of the next pile
            //
            // note: **not** `/clear`. Everywhere else that word is typed at an agent it means the
            // conversation, and this takes away the program's own lines and leaves the conversation
            // alone, so the one thing somebody would be typing it for is the one thing it does not
            // do. It is the same trap `/load` avoids by not calling itself `/resume`, and `/clear`
            // is answered by `no_such_command`
            "cleanup" => self.clear_notices(),
            // with a message, because otherwise the only way to reach the first transition is to
            // send one, which runs the whole turn and leaves nothing to step through
            "step" => {
                // note: the guard `App::submit` puts on a message, for the reason its note gives -
                // an item pushed into a running turn lands between a call and its result, which
                // most of these APIs refuse outright. Without it, `/step something` typed into a
                // running turn would put the something in the context before `start_step` had
                // said whether it could step at all, and then decline the step in silence. A bare
                // `/step` is left alone: advancing a paused turn is what it is for
                //
                // note: but not one that would send a request with nothing new in it, for the
                // reason `/continue` declines one: one step from a finished turn is the same
                // request as the rest of it
                match (rest.is_empty(), self.busy || self.asked().is_some()) {
                    (false, true) => self.say(
                        Speaker::Note,
                        "a turn is running, so this message is not going in - send it on its own \
                         and it waits for the end of the turn, or `/stop` first",
                    ),
                    (false, false) => {
                        self.ask(rest);
                        self.start_step();
                    }
                    (true, true) => self.start_step(),
                    (true, false) => match self.nothing_to_answer() {
                        Some(why) => self.say(Speaker::Note, why),
                        None => self.start_step(),
                    },
                }
            }
            "request" => self.preview("the next request", request_preview(&self.kernel, self.keys)),
            "payload" => {
                let body = match self.kernel.preview_payload() {
                    Ok(Some(payload)) => pretty(&payload),
                    Ok(None) => "this provider cannot render a request without sending it".into(),
                    Err(e) => nothing_to_send(&self.kernel, &e.to_string(), self.keys),
                };
                self.preview("the payload, as it would go out", body);
            }
            "raw" => {
                let raw = self.kernel.last_response().and_then(|r| r.raw.clone());
                let body = match (&self.unanswered, raw) {
                    (Some(error), Some(raw)) => format!(
                        "the last request failed: {error}\n\nthe answer before it:\n{}",
                        pretty(&raw)
                    ),
                    (Some(error), None) => format!("the last request failed: {error}"),
                    (None, Some(raw)) => pretty(&raw),
                    (None, None) => "nothing has been answered yet".into(),
                };
                self.preview("the provider's last answer, verbatim", body);
            }
            // the registry is live rather than fixed at startup: taking a tool out and putting it
            // back means the next request does not mention it, and the one after that does again
            "tools" => match rest.split_once(char::is_whitespace).unwrap_or((rest, "")) {
                ("", _) => self.tools(),
                ("toggle", id) => self.toggle_tool(id.trim()),
                _ => self.say(
                    Speaker::Error,
                    "`/tools` lists them; `/tools toggle ID` stops offering one, or offers it \
                     again",
                ),
            },
            // note: the answer to a result the model has just reported as cut off. It changes the
            // *next* call rather than recovering that one, and does not need to recover it: the
            // whole of a shortened result is excluded beside the copy the model was shown, and
            // `space` on the context tab sends that instead
            "limit" => self.limit(rest),
            "spend" => self.spend_command(rest),
            "requests" => self.requests_command(rest),
            "budget" => self.budget(),
            // note: a command as well as the `y` on the context tab, because on the chat tab the
            // keys belong to the prompt - a bare `y` there is a `y` typed into a message, which is
            // the rule the permission question is built around too
            "copy" => self.copy_command(rest),
            "compact" => self.compact().await,
            "seams" => self.seams(),
            "web" => self.web_page(),
            // the tab rather than a line naming the allowed capabilities: it has the ones that are
            // refused as well, the ones nobody has decided about yet, and what each of them covers.
            // Every row can be changed where it is read.
            //
            // note: the tab and the page give the same answer, because a caller with no screen asks
            // the same question as somebody at a desk. The rules are printed from what the tab
            // draws, so the two cannot drift apart. The tab is still switched because a person at
            // one is asking to be there rather than to be told
            "permissions" => self.permissions_page(),
            // note: the same function `-f` goes through. Putting a file in the context at startup
            // and at the prompt are the same act; a second implementation is a second place for the
            // media types to go stale
            "attach" => self.attach(rest),
            // the same act as `/attach` with nothing to open: something the model should have,
            // put where it will read it, without asking it to say anything back
            "note" => self.note(rest),
            // note: one word per mechanism, and the mechanism here is a state. The command moves an
            // item to `excluded`, and every read back says `excluded`, so it is named for that.
            // `context`'s moves are named the same way, so the person and the model reach for the
            // same word
            "exclude" => self.by_selector("exclude", rest),
            "pin" => self.by_selector("pin", rest),
            "restore" => self.by_selector("restore", rest),
            "model" => {
                if !rest.is_empty() {
                    let (provider, model) = (self.provider.clone(), rest.to_owned());
                    self.say(Speaker::Note, format!("switching to {model}"));
                    self.forget_the_last_model();
                    // the new model has a context limit of its own, and finding it out is a round
                    // trip; the screen should not stop for it, and the next line does
                    let kernel = self.kernel.clone();
                    self.switch(async move {
                        provider.set_model(model).await;
                        // a session started without `-m` has held no provider until now, and this
                        // ends that - after the switch rather than before it, so nothing reads a
                        // model whose name is still empty. Where the kernel has one already, it is
                        // this same object, switched in place, so the kernel is told rather than
                        // handed it again: setting it would ask what it was after the switch and
                        // record the change as from the new model to itself
                        match kernel.model_info() {
                            None => {
                                kernel.set_provider(provider);
                            }
                            Some(_) => {
                                kernel.provider_changed();
                            }
                        }
                    });

                    return;
                }
                match self.kernel.model_info() {
                    Some(info) => self.say(
                        Speaker::Note,
                        format!(
                            "{} at {} ({}), {} tokens of context",
                            info.model,
                            crate::endpoint::shown(&self.provider.endpoint()),
                            info.provider,
                            info.context_limit
                                .map(thousands)
                                .unwrap_or_else(|| "an unknown number of".into()),
                        ),
                    ),
                    None => self.say(
                        Speaker::Error,
                        format!(
                            "no model yet: `/model ID` picks one, and `/models` lists what {} \
                             serves",
                            self.provider.host()
                        ),
                    ),
                }
            }
            // the ids an endpoint serves are its own - `google/gemini-3.5-flash` at one address and
            // `gemini-3.5-flash` at another - so after `/endpoint` this finds what to hand `/model`
            // rather than guessing. The provider already fetches this list to say when a model is
            // not on it; this is the same call with the answer shown rather than checked.
            //
            // note: sent out as the two switches are, and finished in `listed` when it comes back,
            // so a slow endpoint holds nothing but the lines after this one
            "models" => {
                let provider = self.provider.clone();
                let (filter, keys) = (rest.trim().to_lowercase(), self.keys);
                self.errand(
                    "the list of models",
                    async move { provider.models().await },
                    move |app, listed| app.show_models(listed, &filter, keys),
                );
            }
            // the other half of `/model`: the same model name means a different model at a
            // different address, and comparing what is hosted with what is on this machine is two
            // endpoints rather than two names
            "endpoint" => self.endpoint(rest),
            "params" => self.params(rest),
            "save" => self.save(rest),
            // note: not aliased `/resume`. `--resume` at startup is the other answer to the same
            // file: a fresh session built around the snapshot. Two things a keystroke apart that
            // differ in what happens to the context you already have is a trap
            "load" => self.load(rest),
            // note: `/help` and not `F1`, the same panel and its key on a screen. A line typed at a
            // headless run that is not a command is answered here too, and there is no function key
            // down a pipe, so an answer naming `F1` would point somewhere that run cannot go
            other => self.say(Speaker::Error, no_such_command(other)),
        }
    }
}

/// Whether `line` switches the model or the endpoint: `/model` or `/endpoint` with something after
/// it. Bare, each only says what is in use.
pub(super) fn switches(line: &str) -> bool {
    let (command, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    matches!(command, "/model" | "/endpoint") && !rest.trim().is_empty()
}

/// What a line beginning with `/` that is not a command is answered with.
///
/// note: here rather than an arm of its own in the dispatch above, because `/clear` is not a
/// command and an arm would make it one: `every_command_that_exists_is_in_the_help` reads those
/// arms, and a name the prompt answers to is one `/help` has to list. These are names the prompt
/// refuses, and the refusal says where the thing went.
///
/// note: `/clear` is what somebody arriving from any other agent types. There it means the
/// conversation; here `/exclude all` has that shape and `/cleanup` has that name, so a refusal that
/// said only "there is no `/clear`" would leave them looking for both. `/load` does not call itself
/// `/resume`, so it does not set that trap.
///
/// note: the name goes into the refusal through `one_line`, as every other line this program quotes
/// back does. A word with nothing else on the line is as long as somebody cares to make it, so a
/// mistyped command of a hundred thousand characters was once a hundred thousand characters of
/// refusal, where every other notice is cut at 96.
fn no_such_command(name: &str) -> String {
    match name {
        "clear" => "there is no `/clear`; `/cleanup` takes this program's own lines off the \
                    chat, and `/exclude all` takes the conversation out of the next request - \
                    which is a state change, so it comes back"
            .to_owned(),
        other => format!(
            "there is no `/{}`; `/help` lists what there is",
            one_line(other)
        ),
    }
}

impl App {
    /// The page's address and a QR code of it, for a phone to open; see `qr`.
    ///
    /// note: a command as well as a line at startup, because the screen is drawn over whatever was
    /// printed before it, and the address is what somebody wants when they pick up the phone rather
    /// than when they start the program.
    pub fn web_page(&mut self) {
        let Some(address) = self.web.clone() else {
            self.say(
                Speaker::Note,
                "no page is being served; `--web ADDRESS:PORT` serves one",
            );
            return;
        };
        // note: the code is `webui`'s, like the page; a build without it never has an address
        #[cfg(feature = "webui")]
        let code = crate::qr::Code::new(&address).map(|code| code.lines().join("\n"));
        #[cfg(not(feature = "webui"))]
        let code: Option<String> = None;
        let body = match code {
            Some(code) => format!(
                "{address}\n\n{code}\n\nPoint a phone's camera at the code, or type the address. \
                 The eight characters after the port are the page's key, and it refuses an \
                 address without them."
            ),
            None => format!(
                "{address}\n\nThe eight characters after the port are the page's key, and it \
                 refuses an address without them."
            ),
        };
        self.preview("the page", body);
    }
}
