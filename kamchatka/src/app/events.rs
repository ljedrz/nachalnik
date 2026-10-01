//! What the events the kernel broadcasts do to the screen: an `impl App` of its own, since
//! [`App::on_event`] is one `match` over every event there is.

use nachalnik::{Delta, Event, Grant, GrantSource};

use super::{
    Anchor, App, Speaker,
    text::{self, moved, thousands, trace_line, unpriced},
};

impl App {
    /// Takes in one event from the runtime.
    pub fn on_event(&mut self, event: Event) {
        // before anything is made of the event, so that whatever else this does with it - and
        // whatever a screen does after - happens to a session already written down
        self.keep_record();
        self.charge_since();

        // a line per streamed fragment would push everything else out of the trace before it could
        // be read - a long `cat` would erase the whole of it, one `tool.output` at a time. The
        // fragments themselves are on the chat tab; the session log
        // has them if `record_progress` is on
        match &event {
            Event::ModelDelta { .. } => {}
            Event::ToolOutput { tool, chunk, .. } => {
                self.streamed_bytes += chunk.len();
                let detail = format!("{tool}, {} bytes so far", thousands(self.streamed_bytes));
                // one line that counts up, rather than one line per chunk
                match self.trace.back_mut() {
                    Some(last) if last.name == "tool.output" => last.detail = detail,
                    _ => self.trace("tool.output", detail),
                }
            }
            _ => {
                let (name, detail) = trace_line(&event);
                self.trace(name, detail);
            }
        }

        match event {
            Event::ModelDelta { delta } => match delta {
                Delta::Text(fragment) => self.append(Speaker::Model, &fragment),
                Delta::Reasoning(fragment) => self.append(Speaker::Reasoning, &fragment),
                // the arguments are shown once they parse, as the call the model actually made
                _ => {}
            },
            Event::ModelRequested {
                repairs,
                skipped,
                items,
                ..
            } => {
                // the last batch's one-off answers, which nothing can still be waiting for: a
                // request is only built from `Idle`, so every call they were given for has run
                self.policy.forget_network_grants();
                self.close();
                // note: split here rather than when the response lands, because *here* is the
                // one moment the two are the same thing: the context has just been projected
                // into that request and has not moved yet. Asked later, an item elided in the
                // meantime would look as though it had gone out as a marker
                let going = self.going();
                self.pending = items.iter().fold(Anchor::default(), |mut anchor, id| {
                    match self
                        .kernel
                        .item(*id)
                        .is_some_and(|it| going.sends_content(&it))
                    {
                        true => anchor.sent.push(*id),
                        false => anchor.markers += going.costs.get(id).copied().unwrap_or(0),
                    }

                    anchor
                });

                // the kernel altering what the model is told is not a detail for the trace pane.
                // One compaction pass can orphan half a dozen calls at once, though, and six
                // notices in a row push the answer off the screen to say one thing - so the
                // conversation gets the fact and the request preview gets the list
                //
                // note: and only when they change. See `App::reported_repairs` - a repair lasts as
                // long as the state that caused it, so the projector re-does it for every request
                // and honestly reports it again
                //
                // note: the count is everything being repaired rather than what is newly so,
                // because it is the number the preview will show. And the wording says the repair
                // stands: in the past tense it reads as something that happened to this one
                // request, which is exactly what somebody then goes looking for the cause of,
                // and there is nothing about this turn to find
                //
                // note: `/request` and not `ctrl+p`, which is the same page and is the key for it
                // on a screen. Everything this program says goes out of a headless run too, where
                // there is no keyboard and the trace holding the list is not printed - so a run
                // driven down a pipe would be told to press a key that does not exist there, about
                // a list it has no other way to see. The command works in both
                if repairs != self.reported_repairs {
                    match repairs.len() {
                        0 => {}
                        1 => self.say(
                            Speaker::Note,
                            format!(
                                "the request is repaired, and will be while this stands: {}",
                                repairs[0]
                            ),
                        ),
                        many => self.say(
                            Speaker::Note,
                            format!(
                                "the request is repaired in {many} places, and will be while they \
                                 stand; `/request` says where"
                            ),
                        ),
                    }
                    self.reported_repairs = repairs.clone();
                }
                for repair in &repairs {
                    self.trace("", format!("repaired: {repair}"));
                }
                for left_out in skipped {
                    self.trace(
                        "",
                        format!("[{}] left out: {}", left_out.id, left_out.reason),
                    );
                }
            }
            Event::ModelFinished {
                item, usage, stop, ..
            } => {
                self.unanswered = None;
                // note: the tokens are real and the words are gone. Some endpoints bill for
                // reasoning and return none of it - `mercury-2.5`'s stream carries no reasoning
                // field at all - so the context tab shows a turn with nothing in it where the
                // thinking was, and the only trace of where the money went is a number in
                // `/budget`. Said once, because it is true of the endpoint rather than of this turn
                if !self.thought_unseen
                    && usage.is_some_and(|it| it.reasoning_tokens.is_some_and(|n| n > 0))
                    && self
                        .kernel
                        .item(item)
                        .is_none_or(|turn| turn.thinking().next().is_none())
                {
                    self.thought_unseen = true;
                    self.say(
                        Speaker::Note,
                        "this model is charged for reasoning it does not send back, so its \
                         thinking is a number in /budget and nowhere else"
                            .to_owned(),
                    );
                }
                // the provider has just said what that request really cost, and what it was
                // made of is still here from the event that sent it. Paired, they are the one
                // exact figure in this program's accounting; see `Anchor`
                if let Some(reported) = usage.and_then(|usage| usage.input_tokens) {
                    self.anchor = Some(Anchor {
                        reported: reported as usize,
                        ..std::mem::take(&mut self.pending)
                    });
                }
                // the turn is recorded, so whatever streamed is now the item's to say. This is
                // also what makes a provider that does not stream work on a screen without being
                // detected: there was nothing on it and there is an item, and the item is what
                // gets drawn either way. A loop that prints fragments has to print the item
                // itself; see `Headless::say`
                self.caught_up(item);
                // note: said, because nothing else says it. The screen draws a turn cut short as a
                // turn, a pipe prints it and exits `0`, and the record's `stop` was the only place
                // it was written down
                let asked = self
                    .kernel
                    .item(item)
                    .is_some_and(|turn| turn.calls().next().is_some());
                if let Some(why) = text::stopped_short(&stop, asked) {
                    self.say(Speaker::Note, why);
                }
            }
            // the same fact from the two places that can know it, and the second line says which
            // of them it is: one is a count and the other is a guess
            Event::ModelFailed { error, overrun } => {
                self.unanswered = Some(error.clone());
                self.close();
                self.say_error(error);
                self.overran(overrun, true);
            }
            Event::StepFailed { error, overrun } => {
                self.close();
                self.say_error(error);
                self.overran(overrun, false);
            }
            // note: a refusal the policy made on its own, which nobody was asked about and which
            // the tool result records only as `the call was not permitted`. When the tool's own
            // capability is `allow` - `shell` usually is - that leaves a refused call with nothing
            // on screen accounting for it, and "why was that refused?" is the question the
            // permissions tab exists to answer
            Event::PermissionDecided {
                call,
                tool,
                grant: Grant::Deny,
                source: GrantSource::Policy,
                ..
            } => {
                if let Some(reason) = self.policy.why(&call) {
                    self.say(Speaker::Note, format!("{tool}: {reason}"));
                }
            }
            // the one event that carries content, and the only place the old text exists at all
            // once the undo window closes; the viewer reads it back off `←` and `→`
            Event::ContextReplaced { id, was, .. } => self.remember(id, was),
            Event::ToolStarted { .. } => self.streamed_bytes = 0,
            // note: nothing. The calls a turn asked for are on the turn's own item and are
            // drawn from it, so they arrive with the answer rather than one event later - and
            // they go when it goes, without anybody having to remember which turn proposed them
            Event::ToolRequested { .. } => {}
            Event::ToolOutput { call, chunk, .. } => self.append_output(&call, &chunk),
            Event::ToolFinished { call, item, .. } => {
                // whatever this call streamed in is dropped for the item, which holds what the
                // model was actually given - and the line about what it cost is read off the item
                // too, truncation included, so a resumed session says the same thing this one does.
                // Another call's, still running, stays
                self.caught_up_with(&call, item);
                // a fork is charged in a kernel of its own, and this is the first moment after
                // it that this session hears anything
                let forked = self.introspect.as_ref().map_or(0, |it| it.forked());
                if forked > 0 {
                    self.count(forked);
                }
            }
            Event::Compacted { report } => {
                let mut note = format!(
                    "compacted: {}, {} → {} tokens{} ({})",
                    moved(&report),
                    report.tokens_before,
                    report.tokens_after,
                    unpriced(&report),
                    report.reason
                );
                if !report.refused.is_empty() {
                    note.push_str(&format!(
                        "; {} refused, because pinned",
                        report.refused.len()
                    ));
                }
                self.say(Speaker::Note, note);
            }
            // note: said, because it is the one thing about compaction a person has to act on. The
            // compactor keeps its promise - `Shedder` takes neither the turn in progress nor what
            // is pinned - so once the rest is gone, only the person or the model can say what of
            // that may go. The model is told by the notice the wiring gave
            // the kernel, which it reads in the next request; see `wiring::full_notice`
            //
            // note: whether the model was told is not said here. The kernel leaves the notice out
            // where it would take the request over the limit, and the notice says itself when it
            // goes in, as every item does
            Event::ContextFull { full: true, .. } => self.say(
                Speaker::Note,
                "the context is full, and the compactor has nothing more it may take: what is left \
                 is the turn in progress and what is pinned. `/exclude` what is no longer needed",
            ),
            Event::ContextFull { full: false, .. } => {
                self.say(Speaker::Note, "the context has room again")
            }
            Event::Interrupted => self.say(Speaker::Note, "stopped"),
            Event::ToolUnknown { tool, .. } => self.say(
                Speaker::Error,
                format!("the model asked for `{tool}`, which is not a tool here"),
            ),
            // note: a file added to the context is not said out loud here. The chat already draws
            // a line for it off the item, and a second one off this event would say it twice, one
            // above the other, for every `-f` and every `/attach`. The derived line is the one
            // that cannot go out of date, so it is the one that stays. See `App::as_conversation`
            _ => {}
        }
    }
}
