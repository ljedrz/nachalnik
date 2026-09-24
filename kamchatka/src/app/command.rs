//! The slash commands: everything typed at the prompt that is not a message.
//!
//! note: a command is answered here and now rather than turned into anything the kernel has to
//! know about. `/context`, `/seams` and `/budget` read public values off a [`nachalnik::Kernel`]
//! and print them; nothing in this file is a capability the runtime had to grow.

use nachalnik::{
    Calibration, ContextId, ContextItem, ContextKind, ContextState, State, StopReason,
    selectors::Selector,
};

use crate::{app::text::thousands, tools::Limits};

use super::{
    App, Did, Proposed, Reply, Speaker, Tab,
    text::{nothing_to_send, plural, pretty, request_preview},
};

impl App {
    /// Sends a message, or runs a command: one line of what somebody types at the prompt.
    ///
    /// note: `pub` because the prompt is not the only thing entitled to say a line. Every verb
    /// this program has - `/model`, `/exclude`, `/limit`, `/step`, `/save`, `/load`, `/tools
    /// toggle` - is reachable only through here, and without it the only way in would be to
    /// synthesize a key press. A caller that is not a person at a terminal hands the same line to
    /// the same function.
    ///
    /// note: what it says still goes where the screen reads it - [`App::say`] for a line and
    /// `App::preview` for a page - *and* comes back in the [`Reply`], because those are two
    /// different questions. A screen re-reads [`App::loose`] and [`App::overlay`] every frame and
    /// wants the whole of both; a caller answering one line wants what that line produced, and
    /// should not have to watch the two of them change to find out. The copy is cheap, and the
    /// alternative is a watermark kept by every caller.
    pub async fn submit(&mut self, line: &str) -> Reply {
        // a switch still in flight is finished before this line is read, so that nothing acts on
        // a session part-way through changing model. See `App::settling`
        if let Some(settling) = self.settling.take() {
            let _ = settling.await;
        }

        let (from, pages) = (self.loose.len(), self.previews);
        // whatever this line turns into, the time before it was somebody deciding what to type.
        // The next line the trace draws is the one that gap belongs to
        self.acted = true;
        // and it is the line `up` puts back, whatever it turns out to be. Kept here rather than
        // read off the newest user item, because the two are not the same thing: a command never
        // becomes an item, and an item can be rewritten afterwards - so the item would answer
        // "what does the context say now", and what somebody pressing `up` is after is the line
        // they typed
        self.last_sent = Some(line.to_owned());

        if let Some(command) = line.strip_prefix('/') {
            self.command(command).await;

            return self.replied(Did::Ran, from, pages);
        }

        // note: a message sent while a turn is running waits for the end of it rather than going
        // into the context there and then. Both of the obvious alternatives are worse. Pushed
        // immediately, it lands *before* the answer the model is still writing - so the next
        // request ends with a model turn, which Google refuses outright with `requests ending with
        // a model turn are not supported`, and which every other provider answers by replying to
        // itself. Pushed mid-tool-loop it is worse still: it lands between an assistant's call and
        // that call's result, which is a shape most of these APIs reject. What it costs is that a
        // message typed to steer a turn does not reach it - it is answered after, not during.
        // The same holds while a question is open: the turn is paused rather than over, and the
        // call it is waiting on still has a result to come
        //
        // note: checked before the line is said rather than after, so that the one path which
        // says a message *without* an item to tie it to is the one path that has no item yet.
        // Everywhere else goes through `App::ask`, which cannot forget the third step
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            let replaced = self.typed_ahead.replace(line.to_owned());
            self.follow = true;
            // said out loud, because until the turn ends this is the one thing on the screen that
            // the context does not have: a session saved now would not contain it
            self.say(
                Speaker::Note,
                "this goes in when the turn stops, and gets a turn of its own",
            );
            // note: and the one it replaced is accounted for. The slot holds one and the newest
            // wins, which is a decision; what it cannot be is silent, because the waiting message
            // is *drawn* at the end of the conversation - so a second one sent into the same turn
            // takes that row away and puts its own there, and nothing else says so. `up` reaches
            // what is waiting now, which is this one and never the one it replaced
            if replaced.is_some() {
                self.say(
                    Speaker::Note,
                    "the message that was waiting is not going in; this one replaces it",
                );
            }

            return self.replied(Did::Queued, from, pages);
        }

        // this is all "sending a message" is: one context item, and then the loop
        let id = self.ask(line);
        self.start_turn();

        self.replied(Did::Asked(id), from, pages)
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
            // note: not after a turn the model ended of its own accord. There is no rest of it to
            // run: the request would be the conversation again with nothing new at its end, which
            // spends a request on the model repeating itself - and a request ending on a model
            // turn is one some endpoints refuse outright. A turn that ran out of room or was cut
            // short does have a rest, and carries on
            "continue" => match self.kernel.state() {
                State::Finished {
                    stop: StopReason::EndTurn | StopReason::Refusal,
                    ..
                } => self.say(
                    Speaker::Note,
                    "the model ended its turn, so there is nothing to continue; a message is what \
                     starts the next one",
                ),
                _ => self.start_turn(),
            },
            // note: the same act as `esc` and `ctrl+c`, reached by typing, which is the only way
            // to reach it from a browser: a page has no keys to send and `Command::Interrupt` is
            // a button nothing was obliged to draw. Two loops here have a line and nothing else,
            // and `/step` points at this name when it declines.
            //
            // note: three answers, because a session has three states here and only one of them
            // is a turn this can stop. A turn resting on a question is not `busy` - resting is
            // what lets anybody answer it - and an interrupt does not reach it either: there is
            // nothing running to notice one, so the question is still there afterwards. What ends
            // that turn is answering, which is `n` at a terminal and the deny button on a page.
            // Saying so is what this branch is for: `nothing is running` would be false in the one
            // state where somebody plainly has something.
            //
            // note: silent where it worked, which is what `esc` is. What says a turn stopped is
            // the turn stopping - the records, the state, the spinner going out - and a line
            // claiming it as well would be this program reporting its own keystroke. The other
            // branch is not silent for the reason a typed command is not a key: a press that
            // lands on nothing is a press somebody can see missing, and a line typed into a
            // session that was not running looks exactly like one that was ignored
            "stop" => match (self.busy, !self.kernel.pending_permissions().is_empty()) {
                (true, _) => self.interrupt(),
                (false, true) => self.say(
                    Speaker::Note,
                    "the turn is waiting on a question, which stopping does not answer; deny it \
                     to end the turn there",
                ),
                (false, false) => self.say(Speaker::Note, "nothing is running"),
            },
            // note: a command as well as `ctrl+l`, because two of the three loops have no keys to
            // press and one of them is the browser, where a pile of notices is the whole screen
            // rather than a quarter of a tall one. It says nothing when it is done, which is
            // `clear_notices`' own rule and the one place this program is deliberately silent: a
            // line reporting that the lines are gone is the first line of the pile it just cleared
            //
            // note: **not** `/clear`. Everywhere else that word is typed at an agent it means the
            // conversation, and this takes away the program's own lines and deliberately leaves
            // the conversation alone - so the one thing somebody would be typing it for is the one
            // thing it does not do. It is the same trap `/load` declines to set by not calling
            // itself `/resume`, and `/clear` is answered by `no_such_command`
            "cleanup" => self.clear_notices(),
            // with a message, because otherwise the only way to reach the first transition is to
            // send one - which runs the whole turn, and there is nothing left to step through
            "step" => {
                // note: the guard `App::submit` puts on a message, for the reason its note gives -
                // an item pushed into a running turn lands between a call and its result, which
                // most of these APIs refuse outright. Without it, `/step something` typed into a
                // running turn would put the something in the context before `start_step` had
                // said whether it could step at all, and then decline the step in silence. A bare
                // `/step` is left alone: advancing a paused turn is what it is for
                match (rest.is_empty(), self.busy || self.asked().is_some()) {
                    (false, true) => self.say(
                        Speaker::Note,
                        "a turn is running, so this message is not going in - send it on its own \
                         and it waits for the end of the turn, or `/stop` first",
                    ),
                    (empty, _) => {
                        if !empty {
                            self.ask(rest);
                        }
                        self.start_step();
                    }
                }
            }
            "request" => self.preview("the next request", request_preview(&self.kernel)),
            "payload" => {
                let body = match self.kernel.preview_payload() {
                    Ok(Some(payload)) => pretty(&payload),
                    Ok(None) => "this provider cannot render a request without sending it".into(),
                    Err(e) => nothing_to_send(&self.kernel, &e.to_string()),
                };
                self.preview("the payload, as it would go out", body);
            }
            "raw" => {
                let body = match self.kernel.last_response().and_then(|r| r.raw.clone()) {
                    Some(raw) => pretty(&raw),
                    None => "nothing has been answered yet".into(),
                };
                self.preview("the provider's last answer, verbatim", body);
            }
            // the registry is live rather than fixed at startup, and taking a tool out of it and
            // putting it back is the plainest demonstration of that: the next request simply does
            // not mention it, and the one after that does again
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
            // whole of a shortened result is archived beside the copy the model was shown, and
            // `space` on the context tab sends that instead
            "limit" => self.limit(rest),
            "spend" => self.spend_command(rest),
            "budget" => self.budget(),
            // note: a command as well as the `y` on the context tab, because on the chat tab the
            // keys belong to the prompt - a bare `y` there is a `y` typed into a message, which
            // is the rule the permission question is built around too. This is the same act
            // reached from where somebody is standing when they want it
            "copy" => self.copy_command(rest),
            "compact" => self.compact().await,
            "seams" => self.seams(),
            // the tab rather than a line naming the allowed capabilities: it has the ones that are
            // refused as well, the ones nobody has decided about yet, and what each of them covers
            // - and every row can be changed where it is read
            "policy" | "permissions" => self.show(Tab::Permissions),
            // note: the same function `-f` goes through. Putting a file in the context at startup
            // and putting one there at the prompt are the same act at two moments, and a second
            // implementation of it is a second place for the media types to go stale
            "attach" => self.attach(rest),
            // the same act as `/attach` with nothing to open: something the model should have,
            // put where it will read it, without asking it to say anything back
            "note" => self.note(rest),
            // note: one word per mechanism, and the mechanism here is a state. The command moves
            // an item to `excluded` and every place the result is read back says `excluded`, so it
            // is named for that - and `context`'s own moves are named the same way, so the person
            // and the model reach for the same word. The old spellings still work: accepting a
            // word somebody typed costs nothing
            "exclude" | "prune" => self.by_selector("exclude", rest),
            "pin" | "keep" => self.by_selector("pin", rest),
            "restore" => self.by_selector("restore", rest),
            "model" => {
                if !rest.is_empty() {
                    let (provider, model) = (self.provider.clone(), rest.to_owned());
                    self.say(Speaker::Note, format!("switching to {model}"));
                    self.forget_the_last_model();
                    // the new model has a context limit of its own, and finding it out is a round
                    // trip; the screen should not stop for it, and the next line does
                    let kernel = self.kernel.clone();
                    self.settling = Some(tokio::spawn(async move {
                        provider.set_model(model).await;
                        // a session started without `-m` has held no provider until now, and this
                        // is what ends that - after the switch rather than before it, so that
                        // nothing reads a model whose name is still the empty one it was built
                        // with. Where the kernel has one already it is this same object, switched in
                        // place, so the kernel is told rather than handed it again: setting it
                        // would ask it what it was after the switch, and record the change as from
                        // the new model to itself
                        match kernel.model_info() {
                            None => {
                                kernel.set_provider(provider);
                            }
                            Some(_) => {
                                kernel.provider_changed();
                            }
                        }
                    }));

                    return;
                }
                match self.kernel.model_info() {
                    Some(info) => self.say(
                        Speaker::Note,
                        format!(
                            "{} at {} ({}), {} tokens of context",
                            info.model,
                            self.provider.endpoint(),
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
            // the ids an endpoint serves are its own - `google/gemini-3.5-flash` at one address
            // and `gemini-3.5-flash` at another - so after `/provider` this is how to find out what
            // to hand `/model` rather than guess it. The provider already fetches this list, to
            // say when a model is not on it; this is the same call with the answer shown rather
            // than checked.
            //
            // note: awaited here rather than spawned, unlike the two switches. Those are told to
            // go and do something and the screen carries on; this one *is* the answer, and a
            // person who asked for a list is waiting for it either way
            "models" => {
                let provider = self.provider.clone();
                let listed = provider.models().await;
                if listed.is_empty() {
                    self.say(
                        Speaker::Error,
                        format!("{} lists no models", provider.host()),
                    );
                    return;
                }

                let filter = rest.trim().to_lowercase();
                let shown: Vec<&String> = listed
                    .iter()
                    .filter(|name| filter.is_empty() || name.to_lowercase().contains(&filter))
                    .collect();
                if shown.is_empty() {
                    self.say(
                        Speaker::Note,
                        format!("none of the {} listed match `{filter}`", listed.len()),
                    );
                    return;
                }

                // the one in use is marked where it stands rather than pulled to the top, so the
                // list keeps the order the endpoint gave it
                let current = self.kernel.model_info().map(|info| info.model);
                let body = shown
                    .iter()
                    .map(|name| {
                        let mark = match &current {
                            Some(model) if nachalnik_providers::same_model(name, model) => "▸",
                            _ => " ",
                        };
                        format!("{mark} {name}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                // no address in the title: it is one `/provider` away, and the room it costs is
                // the room the line below needs to say what to do with any of this
                let title = match filter.is_empty() {
                    true => format!(" {} models", shown.len()),
                    false => format!(" {} of {} matching `{filter}`", shown.len(), listed.len()),
                };
                self.preview(format!("{title} · /model ID switches "), body);
            }
            // the other half of `/model`: the same model name means a different model at a
            // different address, and comparing what is hosted with what is on this machine is two
            // endpoints rather than two names
            "provider" | "endpoint" => {
                if rest.is_empty() {
                    let endpoint = self.provider.endpoint();
                    self.say(Speaker::Note, format!("requests go to {endpoint}"));
                    return;
                }
                // `URL MODEL`, because a model belongs to the address that serves it: switching
                // one and keeping the other is how a session ends up asking the ollama on this
                // machine for `gemini-3.6-flash`. Given no model the old name is kept, and the new
                // endpoint is asked whether it has one by that name
                let (url, model) = match rest.split_once(char::is_whitespace) {
                    Some((url, model)) => (url.to_owned(), Some(model.trim().to_owned())),
                    None => (rest.to_owned(), None),
                };
                let provider = self.provider.clone();
                self.say(
                    Speaker::Note,
                    match (&model, self.kernel.model_info()) {
                        (Some(model), _) => format!("{model} at {url}, from now on"),
                        (None, Some(info)) => format!(
                            "requests now go to {url}, still asking for {}; the key is the one \
                             this started with",
                            info.model
                        ),
                        // a session that has not picked one yet: there is no name to carry over,
                        // and saying it is "still asking for" nothing reads as a model called
                        // nothing rather than as the gap it is
                        (None, None) => format!(
                            "requests now go to {url}; there is still no model, and `/models` \
                             lists what this one serves"
                        ),
                    },
                );
                // for the reason `/model` drops them, and more so: the same name at a different
                // address is a different model, and this is the command that says so
                self.forget_the_last_model();
                // the new endpoint has a context limit of its own, and a list of what it serves;
                // both are round trips, the screen should not stop for them, and the next line does
                // and then the kernel is told, as `/model` tells it, so that the record says the
                // session is talking to somebody else from here on
                let kernel = self.kernel.clone();
                self.settling = Some(tokio::spawn(async move {
                    provider.set_endpoint(url, model).await;
                    kernel.provider_changed();
                }));
            }
            "params" => {
                if let Some((key, value)) = rest.split_once(' ') {
                    match serde_json::from_str(value.trim()) {
                        Ok(value) => {
                            let mut params = self.kernel.params();
                            params.insert(key.trim().to_owned(), value);
                            self.kernel.set_params(params);
                        }
                        Err(e) => {
                            self.say(Speaker::Error, format!("{key} needs a JSON value: {e}"));
                            return;
                        }
                    }
                }
                let params = self.kernel.params();
                let json = serde_json::to_string(&params).unwrap_or_default();
                self.say(Speaker::Note, format!("parameters: {json}"));

                // note: before the listing is consulted, because this is not a claim about what
                // the model takes - it is one about what this program can read back. A parameter
                // that makes the stream send the whole answer again is one whose effect lands in
                // the transcript, the context and the log, and an endpoint publishing no list at
                // all (ollama, a bare proxy) returns early below and would never have been told
                for (name, does) in nachalnik_providers::openai::NOT_A_STREAM {
                    if params.get(name).is_some_and(|set| set != false) {
                        self.say(Speaker::Error, format!("{name} {does}"));
                    }
                }

                // an empty list means the endpoint published none, not that the model takes none;
                // ollama and a bare OpenAI-compatible proxy both say nothing here, and inventing
                // a restriction out of their silence would be worse than saying nothing back
                let Some(info) = self
                    .kernel
                    .model_info()
                    .filter(|it| !it.parameters.is_empty())
                else {
                    return;
                };
                let takes = |key: &str| info.parameters.iter().any(|name| name == key);

                // the failure worth naming: a parameter the model does not take is not refused.
                // It is sent, it is ignored, and the run it was supposed to change is the same
                // run it would have been - a `seed` that buys no reproducibility, silently
                let ignored: Vec<&str> = params
                    .keys()
                    .map(String::as_str)
                    .filter(|key| !takes(key))
                    .collect();
                if !ignored.is_empty() {
                    // note: two messages, because the list supports two different claims. Where it
                    // is everything the model takes, a parameter missing from it is sent and
                    // ignored, and that is what the error says. Where the endpoint published its
                    // *sampling* parameters only, the same absence settles nothing:
                    // `reasoning_effort` is not among `mercury-2.5`'s and is read anyway, so
                    // reporting it as ignored would be this program inventing a restriction out of
                    // a list that never claimed to be complete. It says what it actually knows,
                    // and an error is downgraded to a note with it - not knowing is not a fault
                    let (speaker, said) = match self.provider.lists_every_parameter() {
                        true => (
                            Speaker::Error,
                            format!(
                                "{} does not list {}: sent, and ignored",
                                info.model,
                                ignored.join(", ")
                            ),
                        ),
                        false => (
                            Speaker::Note,
                            format!(
                                "{} publishes its sampling parameters only, so nothing here says \
                                 what becomes of {}: sent, and unchecked",
                                info.model,
                                ignored.join(", ")
                            ),
                        ),
                    };
                    self.say(speaker, said);
                }

                let spare: Vec<&str> = info
                    .parameters
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !params.contains_key(*name))
                    .collect();
                if !spare.is_empty() {
                    let all = match self.provider.lists_every_parameter() {
                        true => "also takes",
                        false => "also takes, of the ones it publishes",
                    };
                    self.say(
                        Speaker::Note,
                        format!("{} {all}: {}", info.model, spare.join(", ")),
                    );
                }
            }
            "save" => self.save(rest),
            // note: not aliased `/resume`. `--resume` at startup is the *other* answer to the
            // same file - a fresh session built around the snapshot - and two things a keystroke
            // apart that differ in what happens to the context you already have is a trap
            "load" => self.load(rest),
            // note: `/help` and not `F1`, which is the same panel and is the key for it on a
            // screen. A line typed at a headless run that is not a command is answered here too,
            // and there is no function key down a pipe - so an answer naming `F1` would point
            // somewhere that run cannot go
            other => self.say(Speaker::Error, no_such_command(other)),
        }
    }

    /// Stops offering one of the tools, or offers it again.
    ///
    /// note: one word covers both directions and every tool there is, including the ones an MCP
    /// server brought - see [`App::toggle`]. A command that could only drop a tool would have no
    /// way back: a session that dropped `fs` would have dropped it.
    ///
    /// note: it names what it did in terms of the *next request*, because that is when it takes
    /// effect and because it is the thing a person is usually doing this for. Nothing about the
    /// session as it stands changes.
    fn toggle_tool(&mut self, id: &str) {
        if id.is_empty() {
            self.say(
                Speaker::Error,
                "`/tools toggle ID` stops offering a tool, or offers it again; `/tools` lists \
                 them and marks the ones that are off",
            );
            return;
        }

        match self.toggle(id) {
            Some(true) => self.say(
                Speaker::Note,
                format!("`{id}` is offered again, from the next request"),
            ),
            Some(false) => self.say(
                Speaker::Note,
                format!("`{id}` is no longer offered; the next request will not mention it"),
            ),
            None => self.say(Speaker::Error, format!("there is no tool called `{id}`")),
        }
    }

    /// What the model is offered, and what this session is holding back.
    ///
    /// note: both, since `toggle` goes both ways: a list of what is on tells somebody how to turn
    /// a tool off and nothing about how to get it back, and the name of a tool that is off is
    /// precisely what they would have to remember. They are one list with a mark rather than two,
    /// because what is being read is one question - what can this model do - and a second list
    /// under a heading is a second place to look for a name.
    fn tools(&mut self) {
        let offered = self
            .kernel
            .tool_specs()
            .into_iter()
            .map(|spec| (true, spec));
        let shelved = self
            .shelved
            .values()
            .map(|tool| (false, tool.spec()))
            .collect::<Vec<_>>();
        let mut all: Vec<_> = offered.chain(shelved).collect();
        all.sort_by(|(_, one), (_, two)| one.id.cmp(&two.id));

        let body = all
            .into_iter()
            .map(|(on, spec)| {
                let capabilities: Vec<_> =
                    spec.capabilities.iter().map(|c| c.to_string()).collect();
                let mark = match on {
                    true => "▸",
                    false => "·",
                };
                format!(
                    "{mark} {:<22}{}\n{:<24}[{}]\n",
                    spec.id,
                    spec.description,
                    "",
                    capabilities.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.preview(
            "what the model is offered · /tools toggle ID turns one off",
            body,
        );
    }

    /// Reads a file into the context, pinned, as text or as bytes depending on what it is - and
    /// asks whatever was typed after the path.
    ///
    /// note: refused while a turn is running, for the reason [`App::submit`] gives at length about
    /// a message. An item pushed now lands between an assistant's call and that call's result,
    /// which is a shape most of these APIs reject outright - and unlike a message there is nothing
    /// to be gained by queueing it, because a file attached to steer a turn that has already
    /// decided what to read is a file that arrives too late to be what it was for.
    ///
    /// note: **not** pinned, where `-f` is, and the two differ because the acts differ. A file
    /// named on the command line is part of how the session was set up - it is meant to still be
    /// there at the end. One attached at the prompt is a thing brought into a conversation, as
    /// ordinary as a message, and it should get old and be compacted like one. `p` pins it if
    /// this one is meant to last.
    ///
    /// note: a pin would be wrong twice over. It protects against nothing: `Trim` only ever
    /// considers a `ContextKind::ToolResult`, so an attachment is a `Reference` it will never
    /// take, pinned or not. And where a compactor *could*
    /// take one, silently making it the one thing in the context that cannot be compacted is the
    /// decision least likely to be what somebody attaching a 200-page PDF wanted.
    fn attach(&mut self, rest: &str) {
        if rest.is_empty() {
            self.say(
                Speaker::Error,
                "`/attach` takes a path, and then anything you want to ask about it: `/attach \
                 report.pdf what is wrong with this?`. A file it has no media type for goes in \
                 as text, which is right for source and markdown and wrong for a PDF",
            );
            return;
        }
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.say(
                Speaker::Error,
                "not while a turn is running or a call is waiting to be answered",
            );
            return;
        }

        // note: the whole of it is tried as a path first, because a path with a space in it is an
        // ordinary path and splitting on the first space would turn `/attach my report.pdf` into
        // a complaint about a file called `my`. Only when that is not a file does the first word
        // become the path and the rest the question - which is the usual way to want this, and
        // the reason it is one command rather than two things to type
        let (path, asked) = match std::fs::metadata(rest).is_ok() {
            true => (rest, ""),
            false => rest.split_once(' ').unwrap_or((rest, "")),
        };
        let item = match crate::attach::attached(path) {
            Ok(item) => item.because("attached at the prompt"),
            // `{e:#}` for the whole chain: what could not be done, and then the operating
            // system's own account of why
            Err(e) => {
                self.say(Speaker::Error, format!("{e:#}"));
                return;
            }
        };
        // note: nothing is said about what went in. The chat derives a line for a reference off
        // the item itself - which file, what it is, what it costs, and whether anything here
        // could price it - so a sentence here would be a second account of one item, written
        // somewhere it can go out of date. See `App::as_conversation`
        self.kernel.push(item);

        // and the question, if there was one, exactly as typing it would have: one item, then
        // the loop. The file is already in the context, so it goes out with it rather than after
        if !asked.is_empty() {
            self.ask(asked);
            self.start_turn();
        }
    }

    /// Drops the two figures that were one model's tokenizer counting one model's request.
    ///
    /// note: the anchor is what the provider charged for the last request, and carrying it across
    /// is the corner reporting what the *last* model would have charged for a request going to a
    /// different one. `App::anchored` says it falls back after a change of model; this is the line
    /// that makes that true.
    ///
    /// note: and the correction, for the same reason. `Calibrating` is the ratio between what this
    /// counter guessed and what a provider billed, cumulative over every observation - so a scale
    /// learnt from one tokenizer goes on correcting the next one's figures, and a fresh
    /// observation from the new model is averaged into the old model's totals rather than
    /// replacing them, settling on a scale that is neither model's. It is reset through
    /// `Kernel::recalibrate` because that recounts the items as well, which is the half that keeps
    /// the `sending` column and the budget on one scale.
    ///
    /// note: and what has been said once about the endpoint - that it hides its reasoning, that it
    /// reports no usage - because it was said about the endpoint that is gone, and the next one may
    /// be just the same.
    fn forget_the_last_model(&mut self) {
        self.anchor = None;
        self.kernel.recalibrate(Calibration::default());
        (self.thought_unseen, self.unreported) = (false, false);
    }

    /// Puts something into the context that the model should have and does not have to answer.
    ///
    /// note: the gap this fills is a shape rather than a feature. A message starts a turn, so
    /// telling a model as a message a fact it will need in four turns' time costs a request, an
    /// answer, and an "understood" nobody wanted. Saying it *with* the next question buries it,
    /// and saying it afterwards is too late.
    ///
    /// note: a [`ContextItem::memory`] - a `Reference` whose source is `memory` - rather than a
    /// user message. The difference is *not* the wire: a reference projects as a user-role message
    /// exactly as an attached file does, so a note and then a question is two user messages either
    /// way, and every endpoint here takes that. The difference is what the item is to everything
    /// that reads it. The model gets `note:` in front of the words, so it can tell a fact it was
    /// handed from a thing it was asked; `/exclude memories` names every one of them and nothing
    /// else; the chat draws it as what went in rather than as something said; and the runtime's
    /// own taxonomy already had the word, with `context`'s `note` writing the same kind of item
    /// from the model's hand.
    ///
    /// note: not pinned, exactly as `/attach` is not. What is worth keeping from compaction is a
    /// judgement about the note rather than about notes, and `p` is one key on the row.
    fn note(&mut self, rest: &str) {
        if rest.is_empty() {
            self.say(
                Speaker::Error,
                "`/note` takes whatever the model should know without being asked to answer it: \
                 `/note the CI runner has no network`. It goes in with the next request rather \
                 than starting one, and `p` on the context tab keeps it from being compacted",
            );
            return;
        }
        // the same refusal `/attach` gives, for the same reason: an item pushed mid-turn changes
        // the request the model is already answering
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.say(
                Speaker::Error,
                "not while a turn is running or a call is waiting to be answered",
            );
            return;
        }

        // note: nothing is said about what went in, for the reason `/attach` says nothing: the
        // chat derives a line from the item itself, and a sentence here would be a second account
        // of one item written where it can go out of date. See `App::as_conversation`
        self.kernel
            .push(ContextItem::memory("note", rest.to_owned()).because("written at the prompt"));
    }

    /// Excludes, pins or restores whatever a selector names.
    ///
    /// note: with nothing to act on, this shows the language rather than reporting that the empty
    /// string is not a selector. Otherwise the only place to find the grammar is the crate
    /// documentation.
    fn by_selector(&mut self, command: &str, input: &str) {
        if input.is_empty() {
            self.preview(
                format!("/{command} takes any of these"),
                crate::help::SELECTORS,
            );
            return;
        }

        let ids = match input.parse::<Selector>() {
            Ok(selector) => selector.matches(&self.kernel.items()),
            Err(e) => {
                self.say(Speaker::Error, e.to_string());
                return;
            }
        };
        if ids.is_empty() {
            self.say(Speaker::Note, format!("nothing matches `{input}`"));
            return;
        }

        let (state, note) = match command {
            "exclude" => (
                ContextState::Excluded,
                Some(format!("at the terminal, by `{input}`")),
            ),
            "pin" => (ContextState::Pinned, None),
            _ => (ContextState::Active, None),
        };
        let changed = self.kernel.set_state(ids, state, note);
        self.say(
            Speaker::Note,
            format!("{} item(s) are now {state}", changed.len()),
        );
    }

    /// What is plugged into each of the runtime's six seams, right now.
    ///
    /// note: the crate's headline claim is six replaceable parts. `Kernel::policy`, `projector`,
    /// `counter` and `compactor` hand back trait objects, and a trait object you cannot name is not
    /// worth asking for, so each of those traits names itself. This is the claim, checked against
    /// the kernel rather than restated from what this program set up at startup.
    fn seams(&mut self) {
        let kernel = &self.kernel;
        let tools = kernel.tool_specs();
        let body = format!(
            "provider     {}\n\
             tools        {} offered: {}\n\
             policy       {}\n\
             projector    {}\n\
             counter      {}\n\
             compactor    {}\n\
             \n\
             Every one of these is a trait object the kernel holds, and every one of them can be\n\
             replaced while a session is running. Nothing here is the terminal's own bookkeeping:\n\
             it is what the kernel answers when asked.",
            match kernel.model_info() {
                Some(info) => format!(
                    "{} at {} ({})",
                    info.model,
                    self.provider.endpoint(),
                    info.provider
                ),
                None => "none until `/model ID` picks one".to_owned(),
            },
            tools.len(),
            match tools.is_empty() {
                true => "-".to_owned(),
                false => tools
                    .iter()
                    .map(|spec| spec.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            kernel.policy().name(),
            kernel.projector().name(),
            kernel.counter().name(),
            match kernel.compactor() {
                Some(compactor) => compactor.name().to_owned(),
                // `--compact 1` leaves none installed, and "nothing will be dropped" is a fact
                // worth being able to check rather than infer from a flag
                None => "none: nothing is ever dropped to make room".to_owned(),
            },
        );

        self.preview("what is plugged into the runtime", body);
    }

    /// Says what the session has been charged, and changes the ceiling that stops it.
    ///
    /// note: named `spend_command` because `App::spend` is the ceiling itself and a method may not
    /// be both. The command is `/spend`, which is the word on the command line too.
    ///
    /// note: it exists because a ceiling a session cannot see is a session that stops for no
    /// reason anybody at it can read, and because somebody who set one and meant to set a larger
    /// one should not have to start again. `0` takes it away, the way `--requests 0` does - the
    /// same spelling for the same idea, rather than a second word for "none".
    fn spend_command(&mut self, rest: &str) {
        let spent = thousands(self.spent() as usize);
        if !rest.trim().is_empty() {
            let Ok(tokens) = rest.trim().parse::<u64>() else {
                self.say(
                    Speaker::Error,
                    format!("`{}` is not a number of tokens", rest.trim()),
                );
                return;
            };
            self.set_spend((tokens > 0).then_some(tokens));
            let said = match self.spend() {
                // a ceiling under what has already gone is a session that stops here, and saying
                // only what the new number is would leave somebody to find that out by being
                // refused. It is a legitimate thing to want - one way to stop a session is to tell
                // it that it has spent enough - so it is answered rather than argued with
                Some(limit) if self.overspent() => format!(
                    "the ceiling is {} tokens and {spent} have been spent, so nothing more will \
                     be sent",
                    thousands(limit as usize)
                ),
                Some(limit) => format!(
                    "the ceiling is {} tokens; {spent} have been spent",
                    thousands(limit as usize)
                ),
                None => format!("no ceiling; {spent} tokens have been spent"),
            };
            self.say(Speaker::Note, said);

            return;
        }

        let said = match self.spend() {
            Some(limit) => format!(
                "{spent} tokens spent of {}, as the provider has reported them. `/spend N` changes \
                 the ceiling and `/spend 0` takes it away",
                thousands(limit as usize)
            ),
            None => format!(
                "{spent} tokens spent, as the provider has reported them. There is no ceiling; \
                 `/spend N` sets one, and the session stops when it is reached"
            ),
        };
        self.say(Speaker::Note, said);
    }

    /// Shows the output limits, or changes one.
    ///
    /// note: a limit can be right for the other things a tool does and wrong for one call - a
    /// copy of the session asked three questions answers at the end of its deliberation, which is
    /// the part a limit cuts off. Without this, a person watching a result come back shortened
    /// can live with it or lose the session.
    ///
    /// note: it changes the next call, not the one already shortened, and the message says which.
    /// Nothing is lost either way: the whole of a shortened result is archived beside the copy the
    /// model was shown, and one keystroke on the context tab sends it instead.
    ///
    /// note: a row is a **subject** - `fs:read`, `context:look` - which is the same string the
    /// permissions tab is keyed on, so a person who has read one table can read the other and
    /// `--allow fs:grep` and `/limit fs:grep` name the same thing. A tool id is not enough once
    /// one tool does five things of five different sizes.
    ///
    /// note: the table is the `Limits` map rather than the registry, and it holds a row for every
    /// subject this program's tools declare, whether or not this session offers them. That is
    /// right, because a limit set before a tool arrives is in force when it does. A row nobody has
    /// is marked, for the reason
    /// `introspect::if_offered` exists on the other side of the screen: a name in an answer reads
    /// as a thing that is there.
    ///
    /// note: what "offered" means is what the registered tools *declare*, rather than a tool id
    /// read off the front of a row. Those are not the same question, and the difference shows:
    /// `shell` declares `exec:run`, so splitting that row on its colon and looking for a tool
    /// called `exec` would mark the one row that is certainly offered.
    fn limit(&mut self, rest: &str) {
        let offered: Vec<String> = self
            .kernel
            .tool_specs()
            .iter()
            .flat_map(|spec| spec.capabilities.iter().map(|it| it.to_string()))
            .collect();
        let table = |limits: &Limits| {
            let rows = limits.all();
            // as wide as the widest number, so eight tools read `[1]` and a dozen do not jog the
            // column the figures are in
            let wide = rows.len().to_string().len() + 2;
            rows.into_iter()
                .enumerate()
                .map(|(nth, (tool, bytes))| {
                    format!(
                        "{:<wide$} {tool:<18}{:>9} bytes{}",
                        format!("[{}]", nth + 1),
                        thousands(bytes),
                        match offered.contains(&tool) {
                            true => "",
                            false => "   · not offered",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut words = rest.split_whitespace();
        let (Some(named), Some(bytes)) = (words.next(), words.next()) else {
            if !rest.trim().is_empty() {
                self.say(
                    Speaker::Error,
                    // `<subject>`, which is what the rows are and what the table two lines down
                    // calls them. A limit is not a tool's, because one tool does five things
                    "`/limit <subject> <bytes>`, or `/limit` on its own to see them",
                );
                return;
            }
            let body = format!(
                "{}\n\nhow much of a call's output the model is shown, keyed by the same subject \
                 its permission is. `/limit <subject> <bytes>` changes one, by name or by the \
                 number beside it, from the next call onwards; the whole of anything already \
                 shortened is archived beside it on the context tab, one `space` from being sent \
                 instead.{}",
                table(&self.limits),
                match self
                    .limits
                    .all()
                    .iter()
                    .any(|(subject, _)| !offered.contains(subject))
                {
                    // said once, under the table, rather than argued on every marked row - and
                    // true whichever rows are marked, since which tools a session is missing is
                    // not something this sentence gets to assume
                    true =>
                        " A row marked `not offered` is a limit held for something no tool here \
                         declares; it is in force the moment one does, and `/tools toggle` is \
                         what offers back a tool that was turned off.",
                    false => "",
                }
            );
            self.preview("the output limits", body);
            return;
        };

        // the number the listing prints, or the name the model calls. A row out of range falls
        // through as a name and is answered with the listing, which is where the range is
        let rows = self.limits.all();
        let tool = match named.parse::<usize>() {
            Ok(nth) if (1..=rows.len()).contains(&nth) => rows[nth - 1].0.clone(),
            _ => named.to_owned(),
        };
        let tool = tool.as_str();

        let Ok(bytes) = bytes.parse::<usize>() else {
            self.say(
                Speaker::Error,
                format!("`{bytes}` is not a number of bytes"),
            );
            return;
        };
        // a limit of nothing is a tool whose every answer is a marker, which is not a limit
        // anybody means; dropping the tool is what "say nothing" is spelled
        if bytes == 0 {
            self.say(
                Speaker::Error,
                "0 would send the model nothing but a truncation marker; `/tools toggle ID` is \
                 how a tool stops being offered",
            );
            return;
        }

        match self.limits.set(tool, bytes) {
            Some(was) => self.say(
                Speaker::Note,
                format!(
                    "`{tool}` was cut at {} bytes and is now cut at {}, from its next call \
                     onwards{}",
                    thousands(was),
                    thousands(bytes),
                    // a limit that took, on something nothing here declares - which is a real
                    // thing to set up ahead of a toggle and a confusing thing to be told nothing
                    // about, since "from its next call onwards" implies there will be one
                    match offered.iter().any(|it| it == tool) {
                        true => String::new(),
                        false => format!(
                            ". nothing in this session declares `{tool}`, so it has no next call \
                             until something does"
                        ),
                    }
                ),
            ),
            None => self.say(
                Speaker::Error,
                format!("nothing here limits `{tool}`; `/limit` on its own lists what is limited"),
            ),
        }
    }

    /// Lists what a compaction pass would take, and asks whether to take it.
    ///
    /// note: the compactor the kernel runs before a request is the same object, asked by hand.
    /// What this adds is the half an automatic pass cannot have: the list, before anything
    /// happens, with the identifiers to pin from. A pass that announces itself afterwards leaves
    /// somebody reading what they have lost; this is the same information one step earlier, where
    /// it is still a decision.
    ///
    /// note: it exists for the session that cannot ask the model to tidy up, which is the one
    /// most likely to need tidying. `context` is the model's tool for this, and reaching it costs
    /// a request - the request that is failing. Without this, a context too big to send has only
    /// one way out, and it is through the thing that no longer works.
    ///
    /// note: the question stands in the prompt's place like a tool's, and for the same reason it
    /// is pinned rather than modal: the context tab is a keystroke away while it waits, `p` there
    /// is the answer to "not that one", and `y` afterwards works the pass out again. What it
    /// costs is that the prompt is not available while it waits, so keeping an item is `p` rather
    /// than `/pin` - which is the gesture the question names.
    async fn compact(&mut self) {
        // one question at a time, because there is one place to put one. A turn is also a poor
        // moment to be reading a list of what the context holds, since it is being added to while
        // the list is read
        if self.busy || self.asking() {
            self.say(
                Speaker::Error,
                "something is already waiting for an answer; `/compact` when it is done",
            );
            return;
        }

        let Some(compactor) = self.kernel.compactor() else {
            self.say(
                Speaker::Error,
                "no compactor is installed, so there is nothing to ask one for. \
                 `/exclude SELECTOR` takes items out by hand",
            );
            return;
        };

        let items = self.kernel.items();
        let budget = self.kernel.budget();
        let Some(plan) = compactor.plan(&items, &budget).await else {
            self.say(
                Speaker::Note,
                format!(
                    "{} found nothing it may take: the next request is ~{} tokens{}",
                    compactor.name(),
                    thousands(budget.used()),
                    match budget.limit {
                        Some(limit) => format!(" of {}", thousands(limit)),
                        None => String::new(),
                    },
                ),
            );
            return;
        };

        let described = |ids: &[ContextId], doing: &str| -> Vec<String> {
            ids.iter()
                .map(|id| match items.iter().find(|item| item.id == *id) {
                    None => format!("[{id}] there is no such item"),
                    Some(item) => format!(
                        "[{id}] {doing} · {} · {} · {} tokens",
                        item.label,
                        item.kind.name(),
                        thousands(item.tokens),
                    ),
                })
                .collect()
        };

        let mut rows = described(&plan.elide, "elide, leaving a marker in its place");
        rows.extend(described(
            &plan.remove,
            "exclude, out of the request entirely",
        ));
        // counted before the summary row, which is a line about something being *added*: a pass
        // that takes one result and writes a marker in its place takes one item, and a question
        // that said two would be overstating what it is asking for
        let count = rows.len();
        if let Some(summary) = &plan.summary {
            rows.push(format!("and {} goes in, in their place", summary.label));
        }

        // note: what the items are *holding*, not what the request would fall by. An elided item
        // leaves a marker behind and the marker costs what it costs, so the two figures differ by
        // that much per item - which is worth stating rather than rounding away, since this whole
        // question is somebody deciding whether the trade is worth it
        let holding: usize = plan
            .remove
            .iter()
            .chain(&plan.elide)
            .filter_map(|id| items.iter().find(|item| item.id == *id))
            .map(|item| item.tokens)
            .sum();

        self.proposed = Some(Proposed {
            rows,
            count,
            holding,
        });
        // said as well as asked, so the scrollback keeps the fact that it was proposed at all:
        // the panel goes the moment it is answered, and a session read back afterwards would
        // otherwise show a compaction with nothing in front of it
        self.say(
            Speaker::Note,
            format!(
                "{} would take {count} item(s) holding {} tokens",
                compactor.name(),
                thousands(holding),
            ),
        );
    }

    /// `/copy` hands the last thing the model said to the terminal; `/copy N` hands item N.
    ///
    /// note: the last answer with nothing after it, because that is what somebody is reaching for
    /// when they have just read one and want it somewhere else. Anything else on this screen is a
    /// row on the context tab with `y` on it, and a command that took a selector would be a second
    /// way to say what that tab already says better - it shows what each item *is* before you
    /// copy it.
    fn copy_command(&mut self, rest: &str) {
        let named = rest.trim().trim_start_matches('[').trim_end_matches(']');
        let id = match named {
            "" => self
                .kernel
                .items()
                .iter()
                .rev()
                .find(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
                .map(|item| item.id),
            number => match number.parse::<u64>() {
                Ok(number) => Some(ContextId(number)),
                // note: the number rather than a selector, and it says so rather than reading a
                // word as a label and copying whatever that found. A paste is not a thing somebody
                // checks before using
                Err(_) => {
                    return self.say(
                        Speaker::Note,
                        format!(
                            "`/copy` takes an item number, and `{number}` is not one; `y` on the \
                             context tab copies the row it is on"
                        ),
                    );
                }
            },
        };

        match id {
            Some(id) => self.copy(id),
            None => self.say(Speaker::Note, "the model has not said anything yet"),
        }
    }

    /// What the next request is estimated to cost, beside what the last one actually did.
    ///
    /// note: the status line can only afford one number, and it shows the estimate - which is
    /// produced by a counter that does not have the model's tokenizer and is therefore wrong.
    /// This is where the two numbers sit side by side, along with the correction the counter has
    /// worked out for itself from the difference. A budget nobody can check is a decoration.
    fn budget(&mut self) {
        let budget = self.kernel.budget();
        // note: both figures from `Going`, and both from the same one, because they are the two
        // halves of one sentence. `tokens_withheld` answers this from the states, which counts an
        // excluded, archived or elided item and misses one the projector repaired away - and that
        // one is holding as much as any of them. Counted here, the sentence is true of all four
        // ways of not being sent, and the context tab is drawing from the same answer
        let going = self.going();
        let (withheld, out) = self.withheld(&going);

        let anchored = self.anchored(&going, &budget);
        let mut lines = vec![format!(
            "the next request: ~{} tokens, {} of context and {} of tool definitions",
            thousands(budget.used()),
            thousands(budget.context_tokens),
            thousands(budget.tool_tokens),
        )];
        // note: the two figures are answers to the same question by different methods, and
        // which one somebody is reading matters more than either. The line above is the counter
        // estimating the whole request from scratch; this one starts from what the provider
        // charged for the last one and estimates only what has changed since, so its error is a
        // few percent of the change rather than of the context. It is what the status line
        // shows, and saying so here is the only place the difference is explained
        match anchored {
            Some(anchored) => lines.push(format!(
                "anchored on the last response: ~{} tokens - the provider's own {} for the \
                 request it answered, plus what the context has done since. This is the figure \
                 in the corner",
                thousands(anchored),
                thousands(
                    budget
                        .reported
                        .and_then(|usage| usage.input_tokens)
                        .unwrap_or_default() as usize
                ),
            )),
            None => lines.push(
                "nothing to anchor on yet: no response has reported what a request cost, so \
                 every figure here is the counter estimating the whole of it"
                    .to_owned(),
            ),
        }
        let draft = self.drafted();
        if draft != 0 {
            lines.push(format!(
                "and ~{} tokens of message typed but not sent, which the corner is counting and \
                 the context is not",
                thousands(draft)
            ));
        }
        lines.push(match budget.limit {
            Some(limit) => format!(
                "the limit: {}, which the next request would fill {:.1}% of",
                thousands(limit),
                budget.fraction_used().unwrap_or_default() * 100.0
            ),
            None => "the limit: unknown, so there is nothing to measure against".to_owned(),
        });
        // note: the corner says the short version of this; here there is room for why. A pass
        // runs when a request is *built* rather than when a turn ends, so a tool loop leaves the
        // context over the limit and the next request is the thing that brings it back under -
        // which makes every figure above true of the context and not of what goes out
        //
        // note: the compactor is not named here, though it could be. `name()` defaults to the
        // type path, so this would read `kamchatka::tools::trim::Trim` in the middle of a
        // sentence - and `/seams` is the place that answers "which one", in a table where a
        // full path is the useful form
        if budget.fraction_used().is_some_and(|used| used >= 1.0)
            && self.kernel.compactor().is_some()
        {
            lines.push(
                "over the limit, so the compactor runs before the next request is sent: it may \
                 bring that figure down, and it may find that everything it would take is pinned"
                    .to_owned(),
            );
        }
        // note: printed straight after the two figures it qualifies, because it decides how to
        // read them. Every number above is a floor when this is not zero, and the person has no
        // way to tell that from a context that is genuinely small - both look like a low
        // percentage.
        //
        // note: the counter is not named either. `TokenCounter::name` defaults to the type path,
        // which would put `nachalnik::tokens::Calibrating<nachalnik::tokens::BytesPerToken>` in
        // the middle of a line meant to be read. `/seams` answers which counter, in a table where
        // a full path is the useful form
        if !budget.fully_counted() {
            lines.push(format!(
                "unpriced: {} piece(s) of content the counter would not put a number on, so \
                 every figure above is a floor and the real request is larger",
                budget.uncounted,
            ));
        }
        if withheld != 0 {
            // note: "tokens the next request does not carry", rather than "tokens in N items the
            // model is not being shown". Two of the four ways of being held back leave the item in
            // the request: an elided one is there as a line saying it used to be something else,
            // and an assistant turn whose thinking this endpoint will not take back is there in
            // full apart from the thinking. The second wording, about a turn that is mostly what
            // it thought, would tell somebody they are not being shown a turn they can read on the
            // chat tab
            lines.push(format!(
                "held back: {} tokens the next request does not carry, in {out} item(s) - \
                 excluded, archived, elided to a marker, or thinking the endpoint will not take \
                 back",
                thousands(withheld)
            ));
        }

        match budget.reported.and_then(|usage| usage.input_tokens) {
            Some(reported) => {
                let mut line = format!(
                    "the last request really cost {}, as the provider counted it",
                    thousands(reported as usize)
                );
                // note: the one figure here that says what a *change* costs rather than what the
                // request cost. Both dialects report it - `prompt_tokens_details` and
                // `cachedContentTokenCount`. It belongs beside the real cost because it is the
                // same sentence: the front of a request is the tool definitions and the oldest
                // messages, so anything that rewrites them is paid for in full on the next
                // request, and this is the number saying how much that would be
                if let Some(cached) = budget.reported.and_then(|usage| usage.cached_input_tokens) {
                    line.push_str(&match (cached, reported) {
                        (0, _) => ", none of it from the provider's cache".to_owned(),
                        (cached, 0) => format!(", {} of it cached", thousands(cached as usize)),
                        (cached, reported) => format!(
                            ", {} of it ({:.0}%) served from the provider's cache - which is what \
                             a change to the front of the request would cost again",
                            thousands(cached as usize),
                            cached as f64 / reported as f64 * 100.0,
                        ),
                    });
                }
                lines.push(line);

                // note: the output side belongs here for the same reason the cached figure above
                // does - it is reported, and on a reasoning model it is most of what the turn
                // cost. A budget that accounts for the request and stays silent
                // about the answer is half a budget
                if let Some(usage) = budget.reported {
                    lines.push(format!(
                        "and generated {}",
                        crate::app::text::charged(&usage)
                    ));
                }
            }
            None => lines.push("nothing has been sent yet, so there is no real figure".to_owned()),
        }

        // note: whichever counter is installed is asked what it has learned, rather than one this
        // program kept a typed handle to; a counter that learns nothing says so by having nothing
        // to report, and is a sentence rather than a missing line
        //
        // note: small requests teach it nothing and are not counted here, which is why this can
        // be lower than the number of requests a session has sent
        lines.push(match self.kernel.counter().calibration() {
            None => "the counter installed here does not correct itself, so every figure above \
                     is whatever it estimates and nothing has told it otherwise"
                .to_owned(),
            Some(learned) if learned.observations == 0 => {
                "the counter has not been corrected yet: it is guessing at four bytes a token, \
                 and no request so far has been big enough to learn anything from"
                    .to_owned()
            }
            // note: the two figures and the scale, and no percentage. `scale - 1` is the error as a
            // fraction of the *estimate*, where "reading N% low" is read as a fraction of the
            // truth, and the two are far apart for the same pair of numbers. Both are true and a
            // sentence could only assert one of them, so it asserts neither: the guess and the
            // charge are what somebody wants, and the scale between them is already on the line
            Some(learned) => format!(
                "the counter has learned from {} request(s) and scaled itself by {:.3}: its own \
                 guesses came to {} tokens where the provider counted {}",
                learned.observations,
                learned.scale,
                thousands(learned.estimated as usize),
                thousands(learned.reported as usize),
            ),
        });

        self.preview("the budget", lines.join("\n\n"));
    }

    /// Brings a saved session's context back, setting aside whatever is in this one.
    ///
    /// note: not a swap of the kernel. `Kernel::resume` is a constructor, and everything plugged
    /// into a running one - the provider, the policy, the tools, the introspection tools'
    /// handle, the subscription this screen is drawing from - is wired to *this* kernel; a second
    /// one built here would arrive with none of it. `kamchatka -r` is the swap, and it is a
    /// restart because that is what a swap is.
    ///
    /// note: so this is a context operation, and it follows the rule every other one here does:
    /// nothing is destroyed. What was in the context is archived rather than dropped, keeps its
    /// numbers and its contents, and `u` twice puts the whole thing back - once for the items
    /// that came in and once for the ones that were set aside. The loaded items are new items
    /// and are numbered as such: they are what that session said, in this session.
    fn load(&mut self, path: &str) {
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.say(
                Speaker::Error,
                "not while a turn is running or a call is waiting to be answered",
            );
            return;
        }

        // the same spellings `/save` takes, because a pair of commands that accept different ones
        // is a pair that does not round-trip: `/save notes.jsonl` writes `notes.json` beside the
        // log, and `/load notes.jsonl` would otherwise go looking for `notes.jsonl.json`
        let file = match path {
            "" => "session.json".to_owned(),
            given => format!("{}.json", without_suffix(given)),
        };
        let snapshot: nachalnik::Snapshot = match std::fs::read(&file)
            .map_err(|e| format!("could not read {file}: {e}"))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|e| format!("{file} is not a session: {e}"))
            }) {
            Ok(snapshot) => snapshot,
            Err(e) => return self.say(Speaker::Error, e),
        };
        if snapshot.items.is_empty() {
            self.say(Speaker::Error, format!("{file} holds no context"));
            return;
        }
        // refused for the reason `-r` refuses one; see `nachalnik::Snapshot::problems`
        let problems = snapshot.problems();
        if !problems.is_empty() {
            self.say(
                Speaker::Error,
                format!("{file} will not be loaded: {}", problems.join("; ")),
            );
            return;
        }

        // set aside first, so that the calls in the loaded turns are the only ones the projector
        // can pair a loaded result with. Archived items are not projected, so an old copy of the
        // same conversation cannot answer the new one's calls
        //
        // note: except what is pinned. A pin is the person saying this stays, and `--system` is
        // pinned - a load that quietly archived the system instruction would be answering a
        // question about a saved conversation by revoking the one thing the session was told to
        // hold on to
        let standing: Vec<_> = self
            .kernel
            .items()
            .iter()
            .filter(|item| item.is_projected() && item.state != ContextState::Pinned)
            .map(|item| item.id)
            .collect();
        self.kernel.set_state(
            standing.iter().copied(),
            ContextState::Archived,
            Some(format!("set aside for the session loaded from {file}")),
        );

        // what the counter had learned, which is the one piece of a seam's state a snapshot
        // carries; without it the next few requests would be spent relearning what this file
        // already knows.
        //
        // note: `Kernel::recalibrate` rather than reaching through to the counter, and before the
        // items are counted rather than after. Counting first and correcting afterwards would give
        // every loaded item a figure from the scale this session happened to be on, while the
        // budget beside it is projected live and so is already on the loaded one - and the `held`
        // column would disagree with the `sending` column on the same row by exactly the
        // correction. The front door also recounts, which is
        // what brings the items already here - the ones the load is about to set aside, and which
        // `held back` adds to the loaded ones - onto the same scale
        if let Some(calibration) = snapshot.calibration {
            self.kernel.recalibrate(calibration);
        }

        let ids = self.kernel.push_all(snapshot.items);
        // the turns just pushed carry call identifiers this kernel never issued, and nothing else
        // would tell it so: a provider that numbers its calls from zero every turn would hand one
        // of them back, the kernel would have nothing to compare it against, and the next request
        // would carry the same `tool_call_id` twice. `-r` gets this from `Kernel::resume`; this is
        // the same fact, said to a kernel that is already running
        self.kernel.reserve_calls(snapshot.used_calls);
        self.kernel.set_params(snapshot.params);

        let loaded: Vec<_> = ids.iter().filter_map(|id| self.kernel.item(*id)).collect();
        self.say(
            Speaker::Note,
            format!(
                "loaded {} from session `{}` ({file}); {} of your own {} archived, \
                 anything pinned stayed, and `u` twice puts the rest back",
                plural(loaded.len(), "item"),
                snapshot.session,
                standing.len(),
                match standing.len() {
                    1 => "was",
                    _ => "were",
                }
            ),
        );
    }

    /// Writes the session log and a snapshot that can be resumed from, at a path somebody gave.
    ///
    /// note: two files, because they answer different questions: the log says what happened, and
    /// the snapshot is what can be picked back up. An event names an item rather than carrying
    /// it, so the log alone cannot rebuild a context - keeping only one of them means losing
    /// either the story or the state.
    ///
    /// note: the snapshot is what `/load` reads back into a running session and what
    /// `kamchatka -r` starts from.
    fn save(&mut self, path: &str) {
        let stem = match path {
            "" => "session",
            given => without_suffix(given),
        };
        // note: a directory is a place to put it rather than a name for it. Taken whole as the
        // stem, `/save sessions/` would write `sessions/.json` and `sessions/.jsonl` - two
        // dotfiles, invisible to `ls`, under a confirmation that prints the path and so reads as
        // though it had worked. The session's own name is what goes in a directory, which is
        // what this program already does when it writes a session out on its own.
        let stem = match stem.ends_with(std::path::MAIN_SEPARATOR)
            || std::path::Path::new(stem).is_dir()
        {
            true => format!(
                "{}{}{}",
                stem.trim_end_matches(std::path::MAIN_SEPARATOR),
                std::path::MAIN_SEPARATOR,
                self.kernel.session_name()
            ),
            false => stem.to_owned(),
        };
        let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));

        // said rather than asked about: writing the same session again is the ordinary case and
        // a prompt every time would be noise, but a typo landing on somebody else's file should
        // not pass in silence
        let replacing: Vec<&str> = [log.as_str(), state.as_str()]
            .into_iter()
            .filter(|path| std::path::Path::new(path).exists())
            .collect();

        let written = self.write_session(&log, &state);

        match written {
            Ok(records) => {
                if !replacing.is_empty() {
                    self.say(
                        Speaker::Note,
                        format!("replaced {}", replacing.join(" and ")),
                    );
                }
                self.say(
                    Speaker::Note,
                    format!(
                        "{records} records in {log}, and a session in {state} (`/load {state}` \
                         brings it back here, `kamchatka -r {state}` starts a session from it)"
                    ),
                );
            }
            Err(e) => self.say(Speaker::Error, e),
        }
    }
}

/// What a line beginning with `/` that is not a command is answered with.
///
/// note: here rather than an arm of its own in the dispatch above, because `/clear` is not a
/// command and an arm would make it one - `every_command_that_exists_is_in_the_help` reads those
/// arms and is right to: a name the prompt answers to is one `/help` has to list. These are names
/// the prompt *refuses*, and the refusal says where the thing went.
///
/// note: `/clear` is the one there is, and it is worth a sentence because it is what somebody
/// arriving from any other agent types. There the word means the conversation; here the thing
/// with that shape is `/exclude all` and the thing that had that name is `/cleanup`, so a refusal
/// that said only "there is no `/clear`" would leave them looking for both. It is the trap
/// `/load` declines to set by not calling itself `/resume`.
fn no_such_command(name: &str) -> String {
    match name {
        "clear" => "there is no `/clear`; `/cleanup` takes this program's own lines off the \
                    chat, and `/exclude all` takes the conversation out of the next request - \
                    which is a state change, so it comes back"
            .to_owned(),
        other => format!("there is no `/{other}`; `/help` lists what there is"),
    }
}

/// A session's path with the extension taken off, whichever of the two it was spelled with.
///
/// note: shared by `/save` and `/load` because a pair of commands that accept different spellings
/// is a pair that does not round-trip. A session is two files - the snapshot and the log - so
/// `/save notes.jsonl` writes `notes.json` beside `notes.jsonl`, and `/load` taking that same
/// argument at its word would go looking for `notes.jsonl.json`.
///
/// note: the suffix is matched without regard to case, the way `attach::media_type` reads an
/// extension - and the stem is left exactly as it was typed, because that half really does name a
/// different file wherever the filesystem cares. What it buys is `notes.JSON` on a filesystem that
/// does not, which is the default on macOS and Windows.
fn without_suffix(path: &str) -> &str {
    for suffix in [".jsonl", ".json"] {
        let Some(at) = path.len().checked_sub(suffix.len()) else {
            continue;
        };
        if path
            .get(at..)
            .is_some_and(|end| end.eq_ignore_ascii_case(suffix))
        {
            return &path[..at];
        }
    }

    path
}
