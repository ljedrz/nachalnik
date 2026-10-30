//! An append-only log of what happened, and a snapshot of where it ended up.
//!
//! note: the two are together so their difference is read in one place. A [`Record`] says what
//! happened, and survives changes to the client, the model and this crate's internals; a
//! [`Snapshot`] says what there is now, which is what resuming needs. The log names items rather
//! than copying them, so it cannot rebuild a context.
//!
//! note: one standing exception: [`Event::ContextReplaced`] carries the text an item used to hold,
//! because a replacement is the only operation that overwrites content, and once out of the undo
//! window that text is in no snapshot or other record. [`Config::record_payloads`] (the rendered
//! request) and [`Config::record_progress`] (streaming fragments) add more on request; both are off
//! by default, so unasked a log holds names plus what overwrites took.

use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::{Config, Kernel};
use crate::{
    context::ContextItem,
    event::Event,
    model::{Params, ToolCallId},
    tokens::Calibration,
};

/// The format the log and the snapshot are written in, which [`Snapshot::format`] and
/// [`Record::format`] carry.
///
/// note: the number moves when something already written would be read differently, not when
/// something is added. A new event, or a new field with `serde(default)`, leaves it alone, since a
/// reader of this format already reads them: an unknown event as [`Event::Unknown`], an unknown
/// field not at all. A new [`ContextState`](crate::ContextState) does move it: there is no
/// catch-all, so a reader that does not know it cannot read an item in it. `0` is what a record or
/// snapshot from before formats were numbered reads as.
///
/// note: `2` writes the items of [`Event::ModelRequested`] as runs of identifiers, `[[1, 40], 42]`,
/// where `1` wrote every one; see [`ContextIds`](crate::ContextIds). A reader of `2` reads `1`
/// unchanged. A snapshot holds no request, so a snapshot is the same in both but for its number.
pub const FORMAT: u32 = 2;

/// A single entry in a session's history.
///
/// note: `#[non_exhaustive]` because the log is written here and read everywhere else, by
/// deserializing, which the attribute does not affect; something new recorded beside an event is
/// then a patch rather than a break.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Record {
    /// The [`FORMAT`] the log is written in, on the record that begins a session - its
    /// [`Event::SessionStarted`] or [`Event::SessionResumed`] - and on no other.
    ///
    /// note: on those two because a log begins with one of them, and a log carried on across a
    /// resume has the second where it continues, so a reader meets the format before anything
    /// written in it. Not a line of its own, which a reader taking every line for a record could
    /// not read, and not on every record, as a million copies of one number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    /// The entry's sequence number, starting at 1 and never reused.
    pub seq: u64,
    /// When it was recorded, in milliseconds since the Unix epoch.
    ///
    /// note: milliseconds, and an integer, so a record survives a round trip through JSON
    /// unchanged. When two records share a timestamp, [`Record::seq`] is the tie-breaker.
    pub at: u64,
    /// What happened.
    pub event: Event,
}

/// An append-only history of everything that happened in a session.
///
/// note: not serialized application state but a list of events, so a session survives changes to
/// the client, the model and the kernel's internals. Exporting it is a `serde_json::to_string` per
/// [`Record`].
///
/// note: unbounded, because a capped append-only log is not one. Only streaming fragments are kept
/// out by default (see [`Config::record_progress`]). A session that outgrows memory can subscribe
/// with [`Kernel::subscribe`], persist elsewhere and start a new kernel.
///
/// note: reached through [`Kernel::with_history`], the mirror of [`Kernel::with_context`], which
/// copies nothing; it is named for what it holds so that a search for "session" on the kernel finds
/// only [`Kernel::session_name`]. [`Kernel::history`] and [`Kernel::history_since`] return copies
/// where a closure is the wrong shape.
#[derive(Debug, Clone)]
pub struct Session {
    name: String,
    records: VecDeque<Record>,
    seq: u64,
}

impl Session {
    /// Creates an empty session.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            records: VecDeque::new(),
            seq: 0,
        }
    }

    /// Returns the session's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the records, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.iter()
    }

    /// Returns the number of records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns whether the log holds no records, which it also does once they have been drained.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns the highest sequence number handed out; `0` before anything has been recorded.
    ///
    /// note: the highest handed out, not the highest still here: after
    /// [`Kernel::drain_history`](crate::Kernel::drain_history) a log holds no records but this
    /// still answers the last number. `last_seq`, then [`Session::since`], reads what arrived in
    /// between, drained or not, and numbers are never reused, so the two cannot disagree.
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Returns the records whose sequence number is greater than `seq`.
    ///
    /// note: found by halving rather than by reading every record, because the records are in the
    /// order they were numbered and a client following the log asks this after every event.
    pub fn since(&self, seq: u64) -> impl Iterator<Item = &Record> {
        let from = self.records.partition_point(|record| record.seq <= seq);

        self.records.range(from..)
    }

    /// Removes and returns the records up to and including `seq`, oldest first.
    pub(crate) fn drain_through(&mut self, seq: u64) -> Vec<Record> {
        let keep = self
            .records
            .iter()
            .position(|record| record.seq > seq)
            .unwrap_or(self.records.len());

        self.records.drain(..keep).collect()
    }

    /// Numbers the next record after `seq`, for a session carried on from a [`Snapshot`].
    pub(crate) fn carry_on_from(&mut self, seq: u64) {
        self.seq = self.seq.max(seq);
    }

    /// Appends an event.
    pub(crate) fn append(&mut self, event: Event) {
        self.seq = self.seq.saturating_add(1);
        let record = Record {
            format: matches!(
                event,
                Event::SessionStarted { .. } | Event::SessionResumed { .. }
            )
            .then_some(FORMAT),
            seq: self.seq,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default(),
            event,
        };

        self.records.push_back(record);
    }
}

/// Everything a new [`Kernel`] needs in order to carry on where another left off.
///
/// note: not the event log, and not interchangeable with it. The log says what happened, a snapshot
/// where things ended up; the log cannot rebuild a context because its events name items rather
/// than carry them, which keeps it small enough to keep forever. Persist both: the snapshot to
/// resume from, the log to answer how it got like this.
///
/// note: a snapshot plus every [`Event::ContextReplaced`] since it was taken can wind an item back
/// through its overwrites, which neither can alone; see the module note.
///
/// note: nothing transient is here. A resumed kernel starts [`State::Idle`](crate::State) with
/// nothing pending, since a permission nobody is there to answer is not worth restoring; a tool
/// call left without a result is repaired out of the next request by the
/// [`Projector`](crate::Projector), which says so.
///
/// note: `#[non_exhaustive]` because only [`Kernel::snapshot`] makes one, so a new field is not a
/// break; one is read back with `serde`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// The [`FORMAT`] it is written in.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed reads as `0` and resumes
    /// as it always did. One from a later format is named by [`Snapshot::problems`] rather than
    /// read as though it were this one.
    #[serde(default)]
    pub format: u32,
    /// The session's name.
    pub session: String,
    /// Every context item, in order, with the identifiers it had.
    pub items: Vec<ContextItem>,
    /// The parameters that were going to be sent.
    pub params: Params,
    /// The next context identifier to hand out.
    ///
    /// note: stored rather than derived, because an identifier is never reused, not even one
    /// belonging to an item that [`Kernel::undo`] took away again.
    pub next_item: u64,
    /// Tool call identifiers the session has already used, sorted.
    ///
    /// note: a resumed session uses these to check a provider's identifiers against; without them
    /// it would accept one it had used before the snapshot, which [`Event::ToolCallRepaired`]
    /// exists to catch.
    pub used_calls: Vec<ToolCallId>,
    /// The sequence number of the last record in the session's log, as the items were read.
    ///
    /// note: read under the same lock as the items, so the records up to it are exactly the ones
    /// whose changes the items show. A caller writing a log beside a snapshot writes those, and the
    /// two agree.
    ///
    /// note: so a resumed log carries on from it rather than starting again at 1. A client keeping
    /// a [`Kernel::history_since`](crate::Kernel::history_since) cursor across a resume would
    /// otherwise read nothing until the new log passed the old number, and a log kept across the
    /// resume would number two records the same.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes, and numbers
    /// from 1 as it always did.
    #[serde(default)]
    pub last_seq: u64,
    /// The next permission request identifier to hand out.
    ///
    /// note: for the same reason as `last_seq`: a question asked after a resume would otherwise
    /// carry the identifier of one asked before it, in a log that has both. `0` in a snapshot
    /// written before this existed resumes as `1`.
    #[serde(default)]
    pub next_permission: u64,
    /// What the [`TokenCounter`](crate::TokenCounter) learned, if it learns at all.
    ///
    /// note: the one piece of a seam's own state a snapshot carries, because it is easy to lose and
    /// nothing else can recover it. A session long enough to resume has told its counter what
    /// several requests cost; resuming at `1.0` would spend the next requests relearning it and
    /// report a budget different from the one the session closed on.
    ///
    /// note: `serde(default)`, so an older snapshot resumes with nothing learned.
    #[serde(default)]
    pub calibration: Option<Calibration>,
}

/// Past this, a number in a snapshot leaves no room to count on from; see [`Snapshot::problems`].
const ROOM: u64 = u64::MAX / 2;

impl Snapshot {
    /// What is wrong with this snapshot, in words; empty if nothing is.
    ///
    /// note: what [`Kernel::resume`] would have to repair or could not, for a caller that would
    /// rather refuse a snapshot than resume a repaired one. A snapshot read back from a file may
    /// have been edited, merged or written by something else. Resuming repairs what it can: an
    /// identifier two items share, or `0`, gets the next free one; items out of order are sorted by
    /// identifier; a `next_item` that is not past every item is moved past them; every call the
    /// items name is reserved whether or not `used_calls` lists it. It cannot repair a number too
    /// near the top of a `u64` to count on from.
    ///
    /// note: it also names a call and its result that do not pair: a result answering a call no
    /// item makes, or made only after it, and two results answering one call. The kernel records
    /// neither, so either means something else changed the snapshot - except a whole output
    /// shortened by an output limit, which
    /// [`Config::keep_truncated_output`](crate::Config::keep_truncated_output) keeps as a second
    /// result for the call and does not count, once. A call with no result is not among them: a
    /// snapshot taken while the call ran, or before a crash, holds one, and the projector leaves it
    /// out of the next request and says so.
    ///
    /// note: it also names a snapshot in a later [`FORMAT`] than this one, which is read as far as
    /// it fits this version's types and may mean something this version cannot tell.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if self.format > FORMAT {
            problems.push(format!(
                "it is written in format {}, and this reads format {FORMAT} and earlier",
                self.format
            ));
        }

        let mut held = std::collections::HashSet::new();
        let mut highest_so_far = 0;
        for item in &self.items {
            if item.id.0 == 0 {
                problems.push("an item has no identifier (0)".to_owned());
            } else if !held.insert(item.id) {
                problems.push(format!("two items are both numbered {}", item.id));
            } else if item.id.0 < highest_so_far {
                problems.push(format!(
                    "item {} comes after item {highest_so_far}, and would be moved before it",
                    item.id
                ));
            }
            highest_so_far = highest_so_far.max(item.id.0);
        }

        let used: std::collections::HashSet<_> = self.used_calls.iter().collect();
        for call in self.items.iter().flat_map(named_calls) {
            if !used.contains(call) {
                problems.push(format!(
                    "the call `{}` is in the items and not in `used_calls`",
                    call.0
                ));
            }
        }

        let mut made = std::collections::HashSet::new();
        let mut answered = std::collections::HashSet::new();
        let mut whole = std::collections::HashSet::new();
        for item in &self.items {
            made.extend(item.calls().map(|call| &call.id));
            if let crate::context::ContextKind::ToolResult { call, .. } = &item.kind {
                if !made.contains(call) {
                    problems.push(format!(
                        "item {} answers the call `{}`, which no item before it makes",
                        item.id, call.0
                    ));
                } else if item.included_because.as_deref() == Some(crate::context::WHOLE_OUTPUT) {
                    // the kernel's own second answer, kept beside the copy the model was shown,
                    // and kept once
                    if !whole.insert(call) {
                        problems.push(format!(
                            "item {} holds the whole output of the call `{}` a second time",
                            item.id, call.0
                        ));
                    }
                } else if !answered.insert(call) {
                    problems.push(format!(
                        "item {} answers the call `{}` a second time",
                        item.id, call.0
                    ));
                }
            }
        }

        let highest = self.items.iter().map(|item| item.id.0).max().unwrap_or(0);
        if self.next_item == 0 {
            problems.push("`next_item` is 0, which is no identifier".to_owned());
        } else if self.next_item <= highest {
            problems.push(format!(
                "`next_item` is {}, which is not past item {highest}",
                self.next_item
            ));
        }
        for (what, number) in [
            ("an item identifier", highest),
            ("`next_item`", self.next_item),
            ("`last_seq`", self.last_seq),
            ("`next_permission`", self.next_permission),
        ] {
            if number > ROOM {
                problems.push(format!(
                    "{what} is {number}, which leaves no room to number anything after it"
                ));
            }
        }

        problems
    }
}

/// Every tool call identifier an item names: the calls an assistant turn asked for, or the one a
/// tool result answers.
pub(crate) fn named_calls(item: &ContextItem) -> impl Iterator<Item = &ToolCallId> {
    let answers = match &item.kind {
        crate::context::ContextKind::ToolResult { call, .. } => Some(call),
        _ => None,
    };

    item.calls().map(|call| &call.id).chain(answers)
}
