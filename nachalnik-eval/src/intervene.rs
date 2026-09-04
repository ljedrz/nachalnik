//! Controlled changes to a copy of a context.

use nachalnik::{Content, ContextId, ContextItem, ContextState, Snapshot};
use serde::{Deserialize, Serialize};

/// One controlled change, applied to a copy of a context and never to the session under test.
///
/// note: Every variant is a state change on a [`Snapshot`], which is to say every variant is
/// something the runtime already does and a person could already do by hand. Nothing here
/// destroys anything: an item this takes out of a copy keeps its identifier and its contents and
/// is still listed, which is what lets a copy's own account of itself name the item by the number
/// the session under test knows it by.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Intervention {
    /// Nothing at all: the control condition.
    ///
    /// note: The most important variant in this enum. A copy with nothing moved is what every
    /// other one is measured against - see [`Change`](crate::Change) - and running several of
    /// them is the only way to find out how much of a difference the same context gives twice.
    Nothing,
    /// The named items are excluded from the copy's projection.
    Without(Vec<ContextId>),
    /// Everything that was going into the request except the named items is excluded.
    Only(Vec<ContextId>),
    /// The named items keep their place in the conversation and lose their content.
    ///
    /// note: The other way to take something away, and often the better one. An excluded tool
    /// result takes the call that asked for it down as well - the projector has no choice - so
    /// the copy reads a conversation in which the call was never made. An elided one still
    /// answers its call, so only the content is gone, and what changed is what was being tested
    /// rather than the shape of the turn.
    Elided(Vec<ContextId>),
    /// One item says something else.
    Revised {
        /// The item.
        id: ContextId,
        /// What it says in the copy.
        content: String,
    },
    /// An item is added at the end.
    Planted(Box<ContextItem>),
    /// Several of the above, applied in order.
    Compound(Vec<Intervention>),
}

impl Intervention {
    /// Excludes the named items.
    pub fn without(ids: impl IntoIterator<Item = ContextId>) -> Self {
        Self::Without(ids.into_iter().collect())
    }

    /// Excludes everything else.
    pub fn only(ids: impl IntoIterator<Item = ContextId>) -> Self {
        Self::Only(ids.into_iter().collect())
    }

    /// Keeps the named items and takes their content.
    pub fn elided(ids: impl IntoIterator<Item = ContextId>) -> Self {
        Self::Elided(ids.into_iter().collect())
    }

    /// Makes one item say something else.
    pub fn revised(id: ContextId, content: impl Into<String>) -> Self {
        Self::Revised {
            id,
            content: content.into(),
        }
    }

    /// Adds an item.
    pub fn planted(item: ContextItem) -> Self {
        Self::Planted(Box::new(item))
    }

    /// Applies this to a copy of a context, and says what it actually did.
    ///
    /// note: A [`ContextState::Pinned`] item is moved like any other, and recorded in
    /// [`Applied::unpinned`]. The runtime refuses a `Compactor` that tries this, and is right to:
    /// a pin is a promise the kernel makes on the user's behalf against its own automatic
    /// machinery. This is not that machinery. An experimenter ablating an item they pinned
    /// themselves is the user, and the answer to "is that system instruction load-bearing?"
    /// cannot be reached any other way; what must not happen is its happening quietly, so it is
    /// on the record.
    pub fn apply(&self, snapshot: &mut Snapshot) -> Applied {
        let mut applied = Applied::default();
        self.apply_to(snapshot, &mut applied);

        applied
    }

    fn apply_to(&self, snapshot: &mut Snapshot, applied: &mut Applied) {
        match self {
            Self::Nothing => {}
            Self::Without(ids) => Self::state(
                snapshot,
                applied,
                ids,
                ContextState::Excluded,
                "left out of this copy",
            ),
            Self::Elided(ids) => Self::state(
                snapshot,
                applied,
                ids,
                ContextState::Elided,
                "left out of this copy",
            ),
            Self::Only(ids) => {
                let keep: Vec<ContextId> = ids.to_vec();
                let others: Vec<ContextId> = snapshot
                    .items
                    .iter()
                    .filter(|item| item.state.is_projected() && !keep.contains(&item.id))
                    .map(|item| item.id)
                    .collect();
                Self::state(
                    snapshot,
                    applied,
                    &others,
                    ContextState::Excluded,
                    "not among the items this copy was given",
                );
                // the named ones are reported on too, so that naming an item that is not there
                // is an entry in `missing` rather than a silently smaller experiment
                for id in &keep {
                    if !snapshot.items.iter().any(|item| item.id == *id) {
                        applied.missing.push(*id);
                    }
                }
            }
            Self::Revised { id, content } => {
                match snapshot.items.iter_mut().find(|i| i.id == *id) {
                    Some(item) => {
                        if item.state == ContextState::Pinned {
                            applied.unpinned.push(*id);
                        }
                        item.content = Content::text(content.clone());
                        item.note = Some("revised for this copy".to_owned());
                        // whatever it was estimated at was an estimate of what it used to say;
                        // `Kernel::resume` recounts every item with the counter it is resuming under
                        item.tokens = 0;
                        applied.touched.push(*id);
                    }
                    None => applied.missing.push(*id),
                }
            }
            Self::Planted(item) => {
                let mut item = (**item).clone();
                // never an identifier this session has handed out before, which is the one rule
                // the runtime holds to about identifiers
                item.id = ContextId(snapshot.next_item);
                snapshot.next_item += 1;
                item.note = Some("planted in this copy".to_owned());
                applied.touched.push(item.id);
                snapshot.items.push(item);
            }
            Self::Compound(each) => {
                for one in each {
                    one.apply_to(snapshot, applied);
                }
            }
        }
    }

    /// Moves the named items into a state, and records what happened to each.
    fn state(
        snapshot: &mut Snapshot,
        applied: &mut Applied,
        ids: &[ContextId],
        state: ContextState,
        note: &str,
    ) {
        for id in ids {
            match snapshot.items.iter_mut().find(|item| item.id == *id) {
                Some(item) => {
                    if item.state == ContextState::Pinned {
                        applied.unpinned.push(*id);
                    }
                    item.state = state;
                    item.note = Some(note.to_owned());
                    applied.touched.push(*id);
                }
                None => applied.missing.push(*id),
            }
        }
    }

    /// What this does, in words, for a record and for a person.
    pub fn describe(&self) -> String {
        fn ids(ids: &[ContextId]) -> String {
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }

        match self {
            Self::Nothing => "nothing moved".to_owned(),
            Self::Without(what) => format!("without {}", ids(what)),
            Self::Only(what) => format!("only {}", ids(what)),
            Self::Elided(what) => format!("with the content of {} taken", ids(what)),
            Self::Revised { id, .. } => format!("with {id} saying something else"),
            Self::Planted(item) => format!("with `{}` added", item.label),
            Self::Compound(each) => each
                .iter()
                .map(Self::describe)
                .collect::<Vec<_>>()
                .join(", and "),
        }
    }
}

/// What an [`Intervention`] actually did to a copy.
///
/// note: Three lists rather than a count, for the reason the runtime's own `StateChange` keeps
/// three: "there is no item 12" and "item 12 was pinned and I moved it anyway" are different
/// things to tell whoever reads the record, and a measurement whose intervention silently did
/// nothing is worse than one that failed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// The items that were moved.
    pub touched: Vec<ContextId>,
    /// The items that were named and are not in this context.
    pub missing: Vec<ContextId>,
    /// The items that were pinned, and were moved regardless.
    pub unpinned: Vec<ContextId>,
}

impl Applied {
    /// Whether the intervention found everything it was pointed at.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// Whether anything moved at all.
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }
}
