//! The commands that act on the context or say what is in it: `/undo` and `/redo`, `/attach`,
//! `/note`, `/exclude`, `/pin` and `/restore`, `/seams`, `/compact`, `/copy` and `/budget`.

use nachalnik::{ContextId, ContextItem, ContextKind, ContextState, selectors::Selector};

use crate::app::{
    App, Proposed, Speaker,
    text::{MID_TURN, short, thousands},
};

/// What `/undo` and `/redo` say while a turn is under way, which the kernel refuses to rewind;
/// see `nachalnik::Kernel::undo`.
const BUSY_UNDOING: &str =
    "not while a turn is under way: answer or cancel its calls, or let it finish, and try again";

impl App {
    /// One operation taken back, or with `redo` put back, and a line saying which.
    ///
    /// note: `u` and `U` on the context tab and a line typed at the prompt all reach this function,
    /// as with `/cleanup` and <kbd>ctrl+l</kbd>, and for the same reason: a session driven down a
    /// pipe or from a browser has no keys of this program's to press.
    pub(in crate::app) fn undo(&mut self, redo: bool) {
        let done = match redo {
            true => self.kernel.redo(),
            false => self.kernel.undo(),
        };
        let note = match (done, redo) {
            (Ok(true), false) => "undone",
            (Ok(true), true) => "redone",
            (Ok(false), false) => "there is nothing to undo",
            (Ok(false), true) => "there is nothing to redo",
            (Err(_), _) => BUSY_UNDOING,
        };
        self.say(Speaker::Note, note);
    }

    /// Reads a file into the context as text or bytes, depending on the file, and asks whatever was
    /// typed after the path.
    ///
    /// note: refused while a turn is running, for the reason [`App::submit`] gives about a message.
    /// An item pushed now lands between an assistant's call and its result, a shape most of these
    /// APIs reject. Unlike a message, queueing it gains nothing: a file attached to steer a turn
    /// that has already decided what to read arrives too late.
    ///
    /// note: not pinned, unlike `-f`. A file named on the command line is session setup and should
    /// last to the end. One attached at the prompt is as ordinary as a message: it should age and
    /// be compacted like one. `p` pins it if it is meant to last.
    ///
    /// note: `Shedder` treats it as compactable: a picture goes to a marker once the model has seen
    /// it, and a text file goes with the exchange it was brought in for.
    pub(super) fn attach(&mut self, rest: &str) {
        if rest.is_empty() {
            self.say(
                Speaker::Error,
                "`/attach` takes a path, and then anything you want to ask about it: `/attach \
                 report.pdf what is wrong with this?`. A file it has no media type for goes in \
                 as text, which is right for source and markdown and wrong for a PDF",
            );
            return;
        }
        if self.mid_turn() {
            self.say(Speaker::Error, MID_TURN);
            return;
        }

        // note: the whole input is tried as a path first, because a path with a space is an
        // ordinary path and splitting on the first space would turn `/attach my report.pdf` into a
        // complaint about `my`. If that is not a file, the path is the longest part up to a space
        // and the rest is the question. If no part is a path, the refusal names the first word
        let (path, asked) = match std::fs::metadata(rest).is_ok() {
            true => (rest, ""),
            false => (rest.match_indices(' ').rev())
                .map(|(at, _)| (&rest[..at], rest[at + 1..].trim_start()))
                .find(|(path, _)| std::fs::metadata(path).is_ok())
                .unwrap_or_else(|| rest.split_once(' ').unwrap_or((rest, ""))),
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
        // note: nothing is said about what went in. The chat derives a line for a reference from
        // the item itself - which file, what it is, what it costs, and whether anything could price
        // it - so a sentence here would be a second account that can go out of date. See
        // `App::as_conversation`. What is said is what the item costs the session, which that line
        // cannot show
        let id = self.kernel.push(item);
        if let Some(caution) = self
            .kernel
            .item(id)
            .and_then(|it| crate::attach::caution(&it))
        {
            self.say(Speaker::Error, caution);
        }

        // The question, if any, is asked exactly as typed: one item, then the loop. The file is
        // already in the context, so it goes out with it rather than after
        if !asked.is_empty() {
            self.ask(asked);
            self.start_turn();
        }
    }

    /// Puts something into the context that the model should have, without asking it to answer.
    ///
    /// note: a message starts a turn, so telling the model a fact it will need later costs a
    /// request and an answer. Saying it with the next question buries it; saying it afterwards is
    /// too late.
    ///
    /// note: a [`ContextItem::memory`] - a `Reference` whose source is `memory` - rather than a
    /// user message. The difference is not the wire: a reference projects as a user-role message,
    /// so a note and then a question is two user messages either way, and every endpoint here takes
    /// that. The difference is what the item is to everything that reads it. The model gets `note:`
    /// in front of the words, so it can tell a fact it was handed from a thing it was asked.
    /// `/exclude memories` names every one of them and nothing else. The chat draws it as what went
    /// in rather than as something said. And `context`'s `note` already writes the same kind of
    /// item from the model's hand.
    ///
    /// note: not pinned, exactly as `/attach` is not. Whether to keep a note from compaction is a
    /// judgement about the note; `p` pins one.
    pub(super) fn note(&mut self, rest: &str) {
        if rest.is_empty() {
            self.say(
                Speaker::Error,
                format!(
                    "`/note` takes whatever the model should know without being asked to answer \
                     it: `/note the CI runner has no network`. It goes in with the next request \
                     rather than starting one, and {} keeps it from being compacted",
                    match self.keys {
                        true => "`p` on the context tab",
                        false => "`/pin` with its number",
                    }
                ),
            );
            return;
        }
        // the same refusal as `/attach`, for the same reason: an item pushed mid-turn changes the
        // request the model is already answering
        if self.mid_turn() {
            self.say(Speaker::Error, MID_TURN);
            return;
        }

        // note: nothing is said about what went in, for the reason `/attach` says nothing: the chat
        // derives a line from the item itself, and a sentence here would be a second account that
        // can go out of date. See `App::as_conversation`
        self.kernel
            .push(ContextItem::memory("note", rest.to_owned()).because("written at the prompt"));
    }

    /// Excludes, pins or restores whatever a selector names.
    ///
    /// note: with nothing to act on, this shows the language rather than reporting that the empty
    /// string is not a selector. Otherwise the grammar is only in the crate documentation.
    pub(super) fn by_selector(&mut self, command: &str, input: &str) {
        if input.is_empty() {
            self.preview(
                format!("/{command} takes any of these"),
                format!(
                    "{}\n\n  What it matched is reported before anything is sent, and every change \
                     is one\n  `/undo`{} away from being undone.",
                    crate::help::SELECTORS,
                    match self.keys {
                        true => " - or one `u` on the context tab -",
                        false => "",
                    }
                ),
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
            // note: a note, where a restore used to clear one, because the compactor reads it: an
            // item somebody brought back is left alone until the context is full, rather than
            // elided again before the next request. See `tools::Shedder`
            _ => (
                ContextState::Active,
                Some(format!("restored at the terminal, by `{input}`")),
            ),
        };
        let changed = self.kernel.set_state(ids, state, note);
        self.say(
            Speaker::Note,
            format!("{} item(s) are now {state}", changed.len()),
        );
    }

    /// Shows what is plugged into each of the runtime's six seams.
    ///
    /// note: the crate's headline claim is six replaceable parts. `Kernel::policy`, `projector`,
    /// `counter` and `compactor` hand back trait objects, so each of those traits names itself; an
    /// unnamed trait object is not worth asking for. This checks the claim against the kernel
    /// rather than restating what this program set up at startup.
    pub(super) fn seams(&mut self) {
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
                    crate::endpoint::shown(&self.provider.endpoint()),
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
                // `--compact-context-at 1` leaves none installed
                None => "none: nothing is ever dropped to make room".to_owned(),
            },
        );

        self.preview("what is plugged into the runtime", body);
    }

    /// Lists what a compaction pass would take, and asks whether to take it.
    ///
    /// note: the compactor the kernel runs before a request is the same object, asked by hand. This
    /// adds what an automatic pass cannot have: the list, before anything happens, with the
    /// identifiers to pin from. A pass that tells somebody afterwards what they have lost is too
    /// late; the information must come while it is still a decision.
    ///
    /// note: it exists for the session that cannot ask the model to tidy up, which is the one most
    /// likely to need tidying. `context` is the model's tool for this, and reaching it costs a
    /// request - the request that is failing. Without this, a context too big to send has only one
    /// way out: through the thing that no longer works.
    ///
    /// note: the question stands in the prompt's place like a tool's, and for the same reason it is
    /// pinned rather than modal: the context tab is a keystroke away while it waits, `p` means "not
    /// that one", and `y` afterwards runs the pass again. The prompt is not available while it
    /// waits, so keeping an item is `p` rather than `/pin`.
    pub(super) async fn compact(&mut self) {
        // one question at a time, because there is one place to put one. A turn is a poor moment to
        // read the context list, because it is being added to while the list is read
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

        let (items, budget) = (self.kernel.items(), self.kernel.budget());
        let keys = self.keys;
        self.errand(
            "the compaction pass",
            async move {
                let plan = compactor.plan(&items, &budget).await;
                (plan, items, budget, compactor)
            },
            move |app, (plan, items, budget, compactor)| {
                app.planned(plan, &items, &budget, &*compactor, keys)
            },
        );
    }

    /// Asks about the pass `/compact` worked out, or takes it for somebody who cannot be asked.
    ///
    /// note: taken where whoever asked has no keys of this program's to answer with - down a pipe,
    /// or from a client. A line answered with a question nobody can reach has been refused the
    /// thing it asked for. The list is said first, so what was taken was said.
    pub(super) fn planned(
        &mut self,
        plan: Option<nachalnik::CompactionPlan>,
        items: &[std::sync::Arc<ContextItem>],
        budget: &nachalnik::Budget,
        compactor: &dyn nachalnik::Compactor,
        keys: bool,
    ) {
        let Some(plan) = plan else {
            // note: `under` rather than `nothing it may take`, because finding no plan while the
            // context is under the point the compactor makes room at is different from finding no
            // item eligible; saying the second tells somebody the wrong item was in the way. It
            // names the threshold where that is known, and the target where only the target is.
            // The threshold is where `Shedder` starts, and a context between the two is one it
            // leaves
            let marks = |fraction: Option<f64>| {
                budget
                    .limit
                    .zip(fraction)
                    .map(|(limit, fraction)| (limit as f64 * fraction) as usize)
            };
            let said = match (marks(self.compact_threshold), marks(self.compact_target)) {
                (Some(threshold), _) if budget.used() < threshold => format!(
                    "{} has nothing to do: the next request is ~{} tokens, under the ~{} it \
                     starts making room at",
                    short(compactor.name()),
                    thousands(budget.used()),
                    thousands(threshold),
                ),
                (None, Some(target)) if budget.used() <= target => format!(
                    "{} has nothing to do: the next request is ~{} tokens, under the ~{} it \
                     takes the context to",
                    short(compactor.name()),
                    thousands(budget.used()),
                    thousands(target),
                ),
                _ => format!(
                    "{} found nothing it may take: the next request is ~{} tokens{}",
                    short(compactor.name()),
                    thousands(budget.used()),
                    match budget.limit {
                        Some(limit) => format!(" of {}", thousands(limit)),
                        None => String::new(),
                    },
                ),
            };
            self.say(Speaker::Note, said);
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

        // note: what the items are holding, not what the request would fall by. An elided item
        // leaves a marker behind, and the marker costs what it costs, so the two figures differ by
        // that much per item. It is worth stating rather than rounding away, because this question
        // is somebody deciding whether the trade is worth it
        let holding: usize = plan
            .remove
            .iter()
            .chain(&plan.elide)
            .filter_map(|id| items.iter().find(|item| item.id == *id))
            .map(|item| item.tokens)
            .sum();

        // said as well as asked, so the scrollback keeps the fact that it was proposed. The panel
        // goes the moment it is answered; otherwise a session read back afterwards would show a
        // compaction with nothing in front of it
        self.say(
            Speaker::Note,
            format!(
                "{} would take {count} item(s) holding {} tokens",
                short(compactor.name()),
                thousands(holding),
            ),
        );
        if !keys {
            for row in rows {
                self.say(Speaker::Note, row);
            }
            // the report is said by `Event::Compacted`, like any other pass
            self.kernel.apply_compaction(plan);
            return;
        }
        self.proposed = Some(Proposed {
            rows,
            count,
            holding,
        });
    }

    /// `/copy` hands the last thing the model said to the terminal; `/copy N` hands item N.
    ///
    /// note: the last answer with nothing after it, because that is what somebody reaches for after
    /// reading one and wanting it elsewhere. Anything else on this screen is a row on the context
    /// tab with `y` on it, and the tab shows what each item is before you copy it. A command that
    /// took a selector would be a second way to say what the tab already says better.
    pub(super) fn copy_command(&mut self, rest: &str) {
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
                            "`/copy` takes an item number, and `{number}` is not one{}",
                            match self.keys {
                                true => "; `y` on the context tab copies the row it is on",
                                false => "",
                            }
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
    /// note: the status line can only afford one number, and it shows the estimate. The estimate is
    /// produced by a counter without the model's tokenizer, so it is wrong. This is where the two
    /// numbers sit side by side, with the correction the counter worked out from the difference. A
    /// budget nobody can check is decoration.
    pub(super) fn budget(&mut self) {
        let budget = self.kernel.budget();
        // note: both figures come from `Going`, and from the same one, because they are the two
        // halves of one sentence. `App::withheld` says why that is not `tokens_withheld`, and the
        // context tab draws from the same answer
        let going = self.going();
        let (withheld, out) = self.withheld(&going);

        let anchored = self.anchored(&going, &budget);
        let mut lines = vec![format!(
            "the next request: ~{} tokens, {} of context and {} of tool definitions",
            thousands(budget.used()),
            thousands(budget.context_tokens),
            thousands(budget.tool_tokens),
        )];
        // note: the two figures answer the same question by different methods, and which one
        // somebody is reading matters more than either. The line above estimates the whole request
        // from scratch; this one starts from what the provider charged for the last one and
        // estimates only what has changed since, so its error is a few percent of the change rather
        // than of the context. This is what the status line shows, and this is the only place the
        // difference is explained
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
        // note: a pass runs when a request is built rather than when a turn ends, so a tool loop
        // leaves the context over the limit and the next request brings it back under. Every figure
        // above is true of the context, not of what goes out.
        //
        // note: the compactor is not named here. `name()` defaults to the type path, so it would
        // read `kamchatka::tools::shedder::Shedder` in the middle of a sentence. `/seams` answers
        // which compactor, in a table where a full path is the useful form
        if budget.fraction_used().is_some_and(|used| used >= 1.0)
            && self.kernel.compactor().is_some()
        {
            lines.push(
                "over the limit, so the compactor runs before the next request is sent: it may \
                 bring that figure down, and it may find that everything it would take is pinned"
                    .to_owned(),
            );
        }
        // note: printed after the two figures it qualifies because it decides how to read them.
        // Every number above is a floor when this is not zero, and a genuinely small context looks
        // like a low percentage too, so the person cannot tell from the context alone.
        //
        // note: it says which figure is a floor because the answer changes once a request has gone
        // out. The estimate is the counter guessing at the whole request from text, and it has no
        // number for a picture. The anchored figure starts from what the provider charged for a
        // request that carried the pieces already in the context, so from that response on it has
        // them inside it. It still estimates what has changed since, which is text until the next
        // response anchors it again, so a picture added after the last one is not in the figure
        // being called whole.
        //
        // note: the counter is not named either. `TokenCounter::name` defaults to the type path,
        // which would put `nachalnik::tokens::Calibrating<nachalnik::tokens::BytesPerToken>` in
        // the middle of a line. `/seams` answers which counter, in a table where a full path is
        // the useful form
        if !budget.fully_counted() {
            lines.push(match anchored {
                Some(_) => format!(
                    "unpriced: {} piece(s) of content the counter would not put a number on, so \
                     the estimate above is a floor; the anchored figure has what was in the \
                     context when the last request went out, and the provider counted it",
                    budget.uncounted,
                ),
                None => format!(
                    "unpriced: {} piece(s) of content the counter would not put a number on, so \
                     every figure above is a floor and the real request is larger",
                    budget.uncounted,
                ),
            });
        }
        if withheld != 0 {
            // note: "tokens the next request does not carry", not "tokens in N items the model is
            // not being shown". Two of the four ways of being held back leave the item in the
            // request: an elided item is there as a line saying it used to be something else, and
            // an assistant turn whose thinking this endpoint will not take back is there in full
            // apart from the thinking. The other wording would say a turn is not being shown when
            // it can be read on the chat tab
            lines.push(format!(
                "held back: {} tokens the next request does not carry, in {out} item(s) - \
                 excluded, elided to a marker, or thinking the endpoint will not take \
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
                // note: this figure says what a change costs, not what the request cost. Both
                // dialects report it: `prompt_tokens_details` and `cachedContentTokenCount`. It
                // belongs beside the real cost because the front of a request is the tool
                // definitions and the oldest messages, so anything that rewrites them is paid for
                // in full on the next request, and this is the number saying how much that would be
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

        // note: the installed counter is asked what it has learned, not a counter the program keeps
        // a typed handle to. A counter that learns nothing has nothing to report, so it produces a
        // sentence rather than a missing line.
        //
        // note: small requests teach it nothing and are not counted here, so this can be lower than
        // the number of requests a session has sent
        lines.push(match self.kernel.counter().calibration() {
            None => "the counter installed here does not correct itself, so every figure above \
                     is whatever it estimates and nothing has told it otherwise"
                .to_owned(),
            Some(learned) if learned.observations == 0 => {
                "the counter has not been corrected yet: it is guessing at four bytes a token, \
                 and no request so far has been big enough to learn anything from"
                    .to_owned()
            }
            // note: the line shows the two figures and the scale, and no percentage. `scale - 1` is
            // the error as a fraction of the estimate; "reading N% low" is read as a fraction of
            // the truth. The two are far apart for the same pair of numbers. Both are true, and a
            // sentence could only assert one, so it asserts neither. The guess and the charge are
            // what somebody wants, and the scale between them is already on the line.
            //
            // note: the count is the counter's, not this run's, and this is the one place here the
            // two could be taken for each other. A snapshot carries what the counter learned, so
            // the count is every request the session has sent. `/spend` counts the other thing: its
            // total is the program's and starts again in every process
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
}
