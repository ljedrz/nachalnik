//! The slash commands: everything typed at the prompt that is not a message.
//!
//! note: a command is answered here and now rather than turned into anything the kernel has to
//! know about. `/context`, `/seams` and `/budget` read public values off a [`nachalnik::Kernel`]
//! and print them; nothing in this file is a capability the runtime had to grow.

use nachalnik::{ContextState, selectors::Selector};

use crate::{app::text::thousands, tools::Limits};

use super::{
    App, Did, Reply, Speaker, Tab,
    text::{nothing_to_send, pretty, request_preview},
};

impl App {
    /// Sends a message, or runs a command: one line of what somebody types at the prompt.
    ///
    /// note: `pub` because the prompt is not the only thing entitled to say a line. Every verb
    /// this program has - `/model`, `/exclude`, `/limit`, `/step`, `/save`, `/load`, `/tools
    /// drop` - is reachable only through here, and while this was `pub(super)` the only way in
    /// was to synthesize a key press. That is why `examples/recorded.rs` re-wires a kernel from
    /// this crate's parts instead of driving an [`App`]: to drive one, it would have had to type.
    /// A caller that is not a person at a terminal hands the same line to the same function.
    ///
    /// note: what it says still goes where the screen reads it - [`App::say`] for a line and
    /// `App::preview` for a page - *and* comes back in the [`Reply`], because those are two
    /// different questions. A screen re-reads [`App::loose`] and [`App::overlay`] every frame and
    /// wants the whole of both; a caller answering one line wants what that line produced, and
    /// was reduced to watching the two of them change to find out. The copy is cheap and the
    /// alternative was a watermark kept by every caller.
    pub async fn submit(&mut self, line: &str) -> Reply {
        let (from, pages) = (self.loose.len(), self.previews);
        // whatever this line turns into, the time before it was somebody deciding what to type.
        // The next line the trace draws is the one that gap belongs to
        self.acted = true;

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
        // message typed to steer a turn does not reach it - it is answered after, not during
        // ... and the same holds while a question is open: the turn is paused rather than over,
        // and the call it is waiting on still has a result to come
        //
        // note: checked before the line is said rather than after, so that the one path which
        // says a message *without* an item to tie it to is the one path that has no item yet.
        // Everywhere else goes through `App::ask`, which cannot forget the third step
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.typed_ahead = Some(line.to_owned());
            self.follow = true;
            // said out loud, because until the turn ends this is the one thing on the screen that
            // the context does not have: a session saved now would not contain it
            self.say(
                Speaker::Note,
                "this goes in when the turn stops, and gets a turn of its own",
            );

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
            "help" | "?" => self.help(),
            "continue" => self.start_turn(),
            // with a message, because otherwise the only way to reach the first transition is to
            // send one - which runs the whole turn, and there is nothing left to step through
            "step" => {
                if !rest.is_empty() {
                    self.ask(rest);
                }
                self.start_step();
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
            // the registry is live rather than fixed at startup, and taking a tool out of it is
            // the plainest demonstration of that: the next request simply does not offer it
            "tools" if rest.starts_with("drop ") => {
                let id = rest.strip_prefix("drop ").unwrap_or_default().trim();
                match self.kernel.remove_tool(id) {
                    Some(_) => self.say(
                        Speaker::Note,
                        format!(
                            "`{id}` is no longer offered; the next request will not mention it"
                        ),
                    ),
                    None => self.say(Speaker::Error, format!("there is no tool called `{id}`")),
                }
            }
            "tools" => {
                let body = self
                    .kernel
                    .tool_specs()
                    .into_iter()
                    .map(|spec| {
                        let capabilities: Vec<_> =
                            spec.capabilities.iter().map(|c| c.to_string()).collect();
                        format!(
                            "{:<24}{}\n{:<24}[{}]\n",
                            spec.id,
                            spec.description,
                            "",
                            capabilities.join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.preview("what the model is offered", body);
            }
            // note: the answer to a result the model has just reported as cut off. It changes the
            // *next* call rather than recovering that one, and does not need to recover it: the
            // whole of a shortened result is archived beside the copy the model was shown, and
            // `space` on the context tab sends that instead
            "limit" => self.limit(rest),
            "spend" => self.spend_command(rest),
            "budget" => self.budget(),
            "seams" => self.seams(),
            "introspect" => self.introspect(),
            // it used to print a line naming the allowed capabilities. The tab is that line, plus
            // the ones that are refused, plus the ones nobody has decided about yet, plus what
            // each of them covers - and every row can be changed where it is read
            "policy" | "permissions" => self.show(Tab::Permissions),
            // note: the same function `-f` goes through. Putting a file in the context at startup
            // and putting one there at the prompt are the same act at two moments, and a second
            // implementation of it is a second place for the media types to go stale
            "attach" => self.attach(rest),
            // note: one word per mechanism, and the mechanism here is a state. `/prune` moved an
            // item to `excluded` and every place the result is read back said `excluded`, so the
            // command is named for that now - and `amend`'s own five moves are named the same way,
            // so the person and the model reach for the same word. The old spellings still work:
            // accepting a word somebody typed costs nothing
            "exclude" | "prune" => self.by_selector("exclude", rest),
            "pin" | "keep" => self.by_selector("pin", rest),
            "restore" => self.by_selector("restore", rest),
            "model" => {
                if !rest.is_empty() {
                    let (provider, model) = (self.provider.clone(), rest.to_owned());
                    self.say(Speaker::Note, format!("switching to {model}"));
                    // and the anchor goes with it: it is one model's tokenizer counting one
                    // model's request, and carrying it across is the corner reporting what the
                    // *last* model would have charged for a request going to a different one.
                    // `App::anchored` has always said it falls back after a change of model;
                    // this is the line that makes that true
                    self.anchor = None;
                    // the new model has a context limit of its own, and finding it out is a round
                    // trip; the screen should not stop for it
                    tokio::spawn(async move { provider.set_model(model).await });

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
                    None => self.say(Speaker::Error, "there is no provider"),
                }
            }
            // the ids an endpoint serves are its own - `google/gemini-3.5-flash` at one address
            // and `gemini-3.5-flash` at another - so after `/provider` there was no way to find
            // out what to hand `/model` except to guess it. The provider has always fetched this
            // list, to say when a model is not on it; this is the same call with the answer shown
            // rather than checked.
            //
            // note: awaited here rather than spawned, unlike the switches below. Those are told to
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
                    match &model {
                        Some(model) => format!("{model} at {url}, from now on"),
                        None => format!(
                            "requests now go to {url}, still asking for {}; the key is the one \
                             this started with",
                            self.kernel
                                .model_info()
                                .map(|info| info.model)
                                .unwrap_or_default()
                        ),
                    },
                );
                // for the reason `/model` drops it, and more so: the same name at a different
                // address is a different model, and this is the command that says so
                self.anchor = None;
                // the new endpoint has a context limit of its own, and a list of what it serves;
                // both are round trips and the screen should not stop for them
                tokio::spawn(async move { provider.set_endpoint(url, model).await });
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
                    // ignored and saying so is the point. Where the endpoint published its
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
            other => self.say(
                Speaker::Error,
                format!("there is no `/{other}`; F1 lists what there is"),
            ),
        }
    }

    /// Offers the model the two tools that read and change its own context, or stops offering
    /// them.
    ///
    /// note: `add_tool` and `remove_tool`, like `/tools drop` - the registry is live and this is
    /// the plainest thing to demonstrate that with. What it also has to move is the handle the
    /// tools reach the kernel through, because that is the piece with an end to it: taking them
    /// away drops it, and with it whatever `amend` had been remembering - which items it pinned,
    /// and the changes it could still walk back. That is the right answer rather than a
    /// shortcoming. The tools that come back are new ones, and they have not done anything yet.
    fn introspect(&mut self) {
        match self.introspect.take() {
            Some(_) => {
                self.kernel.remove_tool("introspect");
                self.kernel.remove_tool("amend");
                self.say(
                    Speaker::Note,
                    "`introspect` and `amend` are no longer offered; the next request will not mention \
                     them",
                );
            }
            None => {
                self.introspect = Some(crate::introspect::install(
                    &self.kernel,
                    self.limits.clone(),
                ));
                self.say(
                    Speaker::Note,
                    "`introspect` and `amend` go into the next request: the model can now read its own \
                     context, preview what it would say, ask a fork of itself, prune what it is \
                     carrying and walk its own changes back. It cannot touch anything you pinned",
                );
            }
        }
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
    /// note: it was pinned, and the argument for it was wrong twice over. A pin here protects
    /// against nothing: `Trim` only ever considers a `ContextKind::ToolResult`, so an attachment
    /// is a `Reference` it was never going to take, pinned or not. And where a compactor *could*
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

    /// Prunes, pins or restores whatever a selector names.
    ///
    /// note: With nothing to act on, this shows the language rather than reporting that the empty
    /// string is not a selector. The grammar has ten forms and the terminal used to advertise two
    /// of them in a help line, so the only way to find the rest was the crate documentation.
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

    /// What the next request is estimated to cost, beside what the last one actually did.
    ///
    /// note: The status line can only afford one number, and it shows the estimate - which is
    /// produced by a counter that does not have the model's tokenizer and is therefore wrong.
    /// This is where the two numbers sit side by side, along with the correction the counter has
    /// worked out for itself from the difference. A budget nobody can check is a decoration.
    /// What is plugged into each of the runtime's six seams, right now.
    ///
    /// note: The crate's headline claim is six replaceable parts, and until this there was no way
    /// to see any of them from here - `Kernel::policy`, `projector`, `counter` and `compactor`
    /// hand back trait objects, and a trait object you cannot name is not worth asking for. Each
    /// of those traits now names itself, so this is the claim, checked against the kernel rather
    /// than restated from what this program set up at startup.
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
                None => "none set".to_owned(),
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

    /// Reports how much of each tool's output the model is shown, or changes one.
    ///
    /// note: this exists because of a session that asked a copy of itself three questions and got
    /// back the copy's deliberation with all three answers cut off the end. The limit was right
    /// for the four other things that tool does and wrong for that one, and there was no way to
    /// say so without restarting - so a person watching a result come back shortened had the
    /// choice of living with it or losing the session.
    ///
    /// note: it changes the next call, not the one already shortened, and the message says which.
    /// Nothing is lost either way: the whole of a shortened result is archived beside the copy the
    /// model was shown, and one keystroke on the context tab sends it instead.
    ///
    /// note: the rows are numbered, and the number is one this command takes - `/limit 3 64000`
    /// and `/limit read 64000` are the same instruction. A tool has no identifier but its name,
    /// which is what the model calls and what `/tools drop` takes, so this number belongs to the
    /// listing rather than to the tool; that is exactly why it is only worth printing if it can
    /// then be typed. The context tab settled the same argument the same way, and `23G` is there
    /// because the number in its first column is the one `/exclude` takes.
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

    fn limit(&mut self, rest: &str) {
        let table = |limits: &Limits| {
            let rows = limits.all();
            // as wide as the widest number, so four tools read `[1]` and a dozen do not jog the
            // column the figures are in
            let wide = rows.len().to_string().len() + 2;
            rows.into_iter()
                .enumerate()
                .map(|(nth, (tool, bytes))| {
                    format!(
                        "{:<wide$} {tool:<14}{:>9} bytes",
                        format!("[{}]", nth + 1),
                        thousands(bytes)
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
                    "`/limit <tool> <bytes>`, or `/limit` on its own to see them",
                );
                return;
            }
            let body = format!(
                "{}\n\nhow much of each tool's output the model is shown. `/limit <tool> <bytes>` \
                 changes one, by name or by the number beside it, from the next call onwards; the \
                 whole of anything already shortened is archived beside it on the context tab, one \
                 `space` from being sent instead.",
                table(&self.limits)
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
                format!(
                    "0 would send the model nothing but a truncation marker; `/tools drop {tool}` \
                     is how a tool stops being offered"
                ),
            );
            return;
        }

        match self.limits.set(tool, bytes) {
            Some(was) => self.say(
                Speaker::Note,
                format!(
                    "`{tool}` was cut at {} bytes and is now cut at {}, from its next call \
                     onwards",
                    thousands(was),
                    thousands(bytes)
                ),
            ),
            None => self.say(
                Speaker::Error,
                format!(
                    "nothing here limits `{tool}`'s output; the ones that are limited are:\n{}",
                    table(&self.limits)
                ),
            ),
        }
    }

    fn budget(&mut self) {
        let budget = self.kernel.budget();
        // note: both figures from `Going`, and both from the same one, because they are the two
        // halves of one sentence. `tokens_withheld` answers this from the states, which counts an
        // excluded, archived or elided item and misses one the projector repaired away - and that
        // one is holding as much as any of them. Counted here, the sentence is true of all four
        // ways of not being sent, and the context tab is drawing from the same answer
        let (withheld, out) = self.withheld(&self.going());

        let going = self.going();
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
        // note: it used to name the counter, and `TokenCounter::name` defaults to the type path -
        // so the sentence read "of content
        // `nachalnik::tokens::Calibrating<nachalnik::tokens::BytesPerToken>` would not put a
        // number on", sixty-two characters of Rust in the middle of a line meant to be read.
        // `/seams` answers which counter, in a table where a full path is the useful form
        if !budget.fully_counted() {
            lines.push(format!(
                "unpriced: {} piece(s) of content the counter would not put a number on, so \
                 every figure above is a floor and the real request is larger",
                budget.uncounted,
            ));
        }
        if withheld != 0 {
            // and "not sending" is not what an elided item has done either: it is in the request,
            // as a line saying it used to be something else
            lines.push(format!(
                "held back: {} tokens in {out} item(s) the model is not being shown - excluded, \
                 archived, or elided to a marker",
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
                // request cost. Both dialects have always reported it - `prompt_tokens_details`
                // and `cachedContentTokenCount` - and nothing read it out to anybody. It belongs
                // beside the real cost because it is the same sentence: the front of a request is
                // the tool definitions and the oldest messages, so anything that rewrites them is
                // paid for in full on the next request, and this is the number saying how much
                // that would be
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
                // does - it was reported, nothing read it out, and on a reasoning model it is most
                // of what the turn cost. A budget that accounts for the request and stays silent
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
        // say "2" in a session that has sent six things
        lines.push(match self.kernel.counter().calibration() {
            None => "the counter installed here does not correct itself, so every figure above \
                     is whatever it estimates and nothing has told it otherwise"
                .to_owned(),
            Some(learned) if learned.observations == 0 => {
                "the counter has not been corrected yet: it is guessing at four bytes a token, \
                 and no request so far has been big enough to learn anything from"
                    .to_owned()
            }
            // note: the two figures and the scale, and no percentage. There was one, and it read
            // "so it was reading 54.3% low" off `scale - 1` - which is the error as a fraction of
            // the *estimate*, where "reading 54.3% low" is read as a fraction of the truth. Those
            // are 54.3% and 35.2% of the same pair of numbers. Both are true and the sentence
            // could only assert one of them, so it asserts neither: the guess and the charge are
            // what somebody wants, and the scale between them is already on the line
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
    /// into a running one - the provider, the policy, the tools, the two introspection tools'
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

        let file = match path {
            "" => "session.json".to_owned(),
            given if given.ends_with(".json") => given.to_owned(),
            given => format!("{given}.json"),
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
        // items are counted rather than after. Counting first and correcting afterwards gave every
        // loaded item a figure from the scale this session happened to be on, while the budget
        // beside it is projected live and so was already on the loaded one: a context that really
        // came to 3,998 tokens read 2,002, and the `held` column disagreed with the `sending`
        // column on the same row by exactly the correction. The front door also recounts, which is
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
                "loaded {} items from session `{}` ({file}); {} of your own {} archived, \
                 anything pinned stayed, and `u` twice puts the rest back",
                loaded.len(),
                snapshot.session,
                standing.len(),
                match standing.len() {
                    1 => "was",
                    _ => "were",
                }
            ),
        );
    }

    fn save(&mut self, path: &str) {
        // both extensions, so that `/save notes.jsonl` does not write `notes.jsonl.jsonl`
        let stem = match path {
            "" => "session",
            given => given
                .strip_suffix(".jsonl")
                .or_else(|| given.strip_suffix(".json"))
                .unwrap_or(given),
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
