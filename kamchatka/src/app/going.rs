//! What the next request does with each context item, read off the projection rather than off
//! the states.
//!
//! note: its own file because four modules read it and only one of them is the screen -
//! `introspect::context` answers the model's `look` and `budget` out of it, `remote::protocol`
//! puts the same figures on a wire, and `app::views` and `app::transcript` draw them. It sat in
//! `app/mod.rs`, which is this terminal's own state, and it is not that: it is a reading of the
//! kernel, and the model's account of its own budget and the person's are one piece of
//! arithmetic on purpose.

use std::collections::BTreeMap;

use nachalnik::{ContextId, ContextItem, Kernel};

/// What the next request does with each context item.
///
/// note: the two halves are one answer, taken from one projection, because they have to agree:
/// every item is either in the request for some number of tokens or out of it for a reason, and a
/// screen that worked the two out separately would have rows that are neither.
///
/// note: `#[non_exhaustive]`, and this one earned it: `holds` was added to it after the rest, and
/// a struct of public fields that anybody may build with a literal cannot gain one without
/// breaking them. Nothing should build one - it is an answer rather than a request, and
/// [`Going::of`] is where it comes from - so saying so costs the caller nothing and makes the
/// next field a patch instead of a major. It was the first struct in this workspace to carry the
/// attribute and it is not the only one now; `nachalnik`'s own answers took it the day after.
#[non_exhaustive]
pub struct Going {
    /// What each item in the request costs it.
    pub costs: BTreeMap<ContextId, usize>,
    /// What each item holds, counted at the same moment as [`Going::costs`] and with the same
    /// counter.
    ///
    /// note: not [`ContextItem::tokens`], which is the figure taken when the item arrived - and
    /// the difference is not pedantry. A `Calibrating` counter learns a new scale from every
    /// response, and the items already in the context keep the figure they were counted with
    /// until something recounts them, which `App` does when a *turn* ends. Every reading taken
    /// inside a turn therefore had a stored figure on one scale and a freshly projected message
    /// on another, and the few percent between them arrived in the `held` column as tokens held
    /// back that nothing was holding: a live session against Gemini - whose projector carries
    /// thinking and ordered blocks back in full, so it holds nothing at all - reported 1,264
    /// tokens held across eighteen rows. The `context` tool is worse off again, because it is
    /// only ever called from inside a turn.
    pub holds: BTreeMap<ContextId, usize>,
    /// Why each item that is not in the request was left out, in the projector's own words.
    pub left_out: BTreeMap<ContextId, String>,
    /// What the model reads in place of an item whose content is not going but which is still
    /// in the request - that is, an elided one.
    ///
    /// note: taken out of the projection rather than assembled from the item's state and note,
    /// for the same reason [`Going::left_out`] is. The brackets are the projector's, the words
    /// inside them are whoever elided it, and a screen that put its own version of the sentence
    /// next to the one the model is reading would be showing a third thing that is neither.
    pub marker: BTreeMap<ContextId, String>,
}

impl Going {
    /// What the next request does with each item: what it costs, or why it is not in it.
    ///
    /// note: not [`ContextItem::tokens`], which is what an item *holds*. An elided one holds a
    /// thousand tokens and costs the dozen its marker takes; an archived one holds whatever it
    /// holds and costs nothing. A pane that showed the held figure under a column headed `tokens`
    /// was answering a question nobody asked while the status line beside it answered the right
    /// one, and the two disagreed by exactly the elided items.
    ///
    /// note: read out of the projection rather than worked out here, because what an elided item
    /// costs is the marker the *projector* writes, in the brackets the projector chooses. A
    /// client that computed it would be keeping a second copy of a decision that is not its own.
    ///
    /// note: and `left_out` comes from the projection for a sharper reason than tidiness. Whether
    /// an item is going cannot be read off its *state*: a projector repairs a request to keep it
    /// valid, and an item it repairs away is `Active`, holding everything it holds, and not in the
    /// request. Restoring the whole of a truncated output beside the copy the model was shown
    /// makes one - the pair answer one call, so the whole takes the call and the short copy is
    /// dropped. A pane keyed on the state then had that row claiming to send its content, showing
    /// `0` for it, and accounting for none of what it was holding: three wrong answers about one
    /// item, from asking the item instead of asking the request.
    pub fn of(kernel: &Kernel) -> Going {
        let projection = kernel.project();
        let counter = kernel.counter();
        // `included` and `messages` line up one for one under a projector that makes a message
        // per item; one that merges them has no per-item answer, and the item's own figure is a
        // better guess than a number taken from the wrong message
        let paired = projection.included.len() == projection.messages.len();

        Going {
            // what the model reads where an elided item's content was, which only a paired
            // projection can answer: it is the message that came out, not anything the item
            // holds
            marker: match paired {
                false => BTreeMap::new(),
                true => projection
                    .included
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| kernel.item(**id).is_some_and(|item| item.state.is_elided()))
                    .filter_map(|(at, id)| {
                        let said = projection.messages[at].content.as_ref()?;
                        Some((*id, said.to_text().into_owned()))
                    })
                    .collect(),
            },
            // every item, counted now, so that a subtraction against `costs` is one ruler at one
            // moment - see the note on the field
            holds: kernel
                .items()
                .iter()
                .map(|item| (item.id, counter.count_item(item)))
                .collect(),
            costs: projection
                .included
                .iter()
                .enumerate()
                .map(|(at, id)| {
                    let cost = match paired {
                        true => counter.count_message(&projection.messages[at]),
                        false => kernel.item(*id).map(|item| item.tokens).unwrap_or(0),
                    };

                    (*id, cost)
                })
                .collect(),
            // the projector's own words, rather than a second copy of them assembled out here
            // from the state and the note - which is what this was, and which had no answer at
            // all for an item the projector had repaired away
            left_out: projection
                .skipped
                .into_iter()
                .map(|skipped| (skipped.id, skipped.reason))
                .collect(),
        }
    }

    /// Whether this item's own content is going into the request.
    ///
    /// note: two conditions rather than [`nachalnik::ContextState::sends_content`], and the
    /// second is the one that bites. An item may be in a state that sends content and still not
    /// be in the request, because a projector repairs a request to keep it valid - a second
    /// result for a call that already has one is dropped, which is what putting the whole of a
    /// truncated output back beside the short copy produces. Anything asking "is this item's
    /// content going" asks here, so that the columns, the reason beside them and `/budget`
    /// cannot drift apart.
    pub fn sends_content(&self, item: &ContextItem) -> bool {
        item.state.sends_content() && self.costs.contains_key(&item.id)
    }

    /// What this item holds that the next request will not carry - which is what sending the
    /// whole of it would add.
    ///
    /// note: the difference between the two counts rather than the whole of an item that is not
    /// going, because an item is not in or out any more. Those are the same number for an
    /// excluded, archived or repaired-away item, whose message costs nothing; they are not for
    /// the two that are partly there. An elided one holds its content and sends a marker. And an
    /// assistant turn under an endpoint that will not take reasoning back - which is every
    /// OpenAI-compatible one - sends what it said and holds what it thought, which on a reasoning
    /// model is most of the session: 25,903 tokens of thinking on one turn, reported nowhere,
    /// with every row on the pane reading under 2k.
    ///
    /// note: `count_item` and `count_message` count an assistant turn's reasoning and calls the
    /// same way, so the two figures are like for like and the difference is a number rather than
    /// an artefact. Where the projection adds something of its own - a label in front of a tool
    /// result - the message is the larger of the two and this is nought, which is the right
    /// answer: nothing is being held back.
    ///
    /// note: and [`Going::holds`] rather than [`ContextItem::tokens`], because like for like is
    /// also about *when*. The field's own note has what a live run made of the difference.
    pub fn held_back(&self, item: &ContextItem) -> usize {
        self.holds(item)
            .saturating_sub(self.costs.get(&item.id).copied().unwrap_or(0))
    }

    /// What this item holds, on the same scale as everything else here.
    pub fn holds(&self, item: &ContextItem) -> usize {
        self.holds
            .get(&item.id)
            .copied()
            // an item added since this was taken; its own figure is the best there is
            .unwrap_or(item.tokens)
    }
}
