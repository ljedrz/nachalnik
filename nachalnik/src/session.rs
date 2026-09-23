//! The paper trail: an append-only log of what happened, and a snapshot of where it ended up.
//!
//! note: the two are here together so that the difference between them is read in one place. A
//! [`Record`] says what happened and survives every change to the client, the model and this
//! crate's internals; a [`Snapshot`] says what there *is* now, which is what resuming needs - the
//! log names items rather than copying them, so a log cannot rebuild a context.
//!
//! note: "names rather than copies" has one standing exception. [`Event::ContextReplaced`] carries
//! the text an item used to hold, deliberately: a replacement is the only operation that
//! overwrites an item's content, so once the change falls out of the undo window that text exists
//! in no snapshot and in no other record. Enforcing the rule on it would destroy the only account
//! of an overwrite. Two [`Config`] settings put more content in on request -
//! [`Config::record_payloads`] keeps the rendered request and [`Config::record_progress`] keeps
//! streaming fragments - and both are off by default, so what a log holds unasked is the names,
//! plus what an overwrite took.

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

/// A single entry in a session's history.
///
/// note: `#[non_exhaustive]` because the log is written here and read everywhere else. A reader
/// deserializes these rather than building them, which the attribute does not touch, and
/// something else worth recording beside an event is then a patch rather than a break.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Record {
    /// The entry's sequence number, starting at 1 and never reused.
    pub seq: u64,
    /// When it was recorded, in milliseconds since the Unix epoch.
    ///
    /// note: Milliseconds, and an integer, so that a record survives a round trip through JSON
    /// unchanged; when two records share a timestamp, [`Record::seq`] is the tie-breaker.
    pub at: u64,
    /// What happened.
    pub event: Event,
}

/// An append-only history of everything that happened in a session.
///
/// note: This is deliberately not "serialized application state": it is a list of events, so a
/// session survives changes to the client, the model, and the kernel's own internals. Exporting
/// it is a `serde_json::to_string` per [`Record`].
///
/// note: The log is unbounded, and stays that way - a capped "append-only" log is not one.
/// Streaming fragments are the only events kept out of it by default (see
/// [`Config::record_progress`]); if a session outgrows memory, subscribe with
/// [`Kernel::subscribe`], persist elsewhere, and start a new kernel.
///
/// note: A caller reaches one through [`Kernel::with_history`], the mirror of
/// [`Kernel::with_context`], which copies nothing. It is named for what it holds rather than for
/// the type, so a search for "session" on the kernel finds only [`Kernel::session_name`].
/// [`Kernel::history`] and [`Kernel::history_since`] hand back copies for when a closure is the
/// wrong shape.
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

    /// Returns whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns the highest sequence number handed out; `0` before anything has been recorded.
    ///
    /// note: the highest handed out rather than the highest still here, which is the difference
    /// [`Kernel::drain_history`](crate::Kernel::drain_history) makes: a drained log holds no
    /// records and this still answers what the last of them was numbered. That is what makes it a
    /// cursor: `last_seq`, then [`Session::since`], reads what arrived in between, whether or not
    /// anybody took the records out from under it - and numbers are never reused, so the two
    /// cannot disagree.
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Returns the records whose sequence number is greater than `seq`.
    ///
    /// note: found by halving rather than by reading every record, because the records are in
    /// the order they were numbered and a client following the log asks this after every event.
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
/// note: This is deliberately *not* the event log, and the two are not interchangeable. The log
/// says what happened; a snapshot says where things ended up. A log cannot rebuild a context,
/// because an event names an item rather than carrying its contents - which is exactly what
/// keeps the log small enough to keep forever. Persist both: the snapshot to resume from, the
/// log to answer "how did it get like this?".
///
/// note: a snapshot plus every [`Event::ContextReplaced`] since it was taken is enough to wind an
/// item back through its overwrites, which neither can do alone. See the module note.
///
/// note: What is *not* here is anything transient. A resumed kernel starts
/// [`State::Idle`](crate::State) with nothing pending, because a permission that nobody is
/// around to answer is not worth restoring; a tool call left without a result is repaired out of
/// the next request by the [`Projector`](crate::Projector), and says so.
///
/// note: `#[non_exhaustive]`, because [`Kernel::snapshot`] is what makes one and nothing outside
/// this crate has to, so a field added here is not a break. One is read back with `serde`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// The session's name.
    pub session: String,
    /// Every context item, in order, with the identifiers it had.
    pub items: Vec<ContextItem>,
    /// The parameters that were going to be sent.
    pub params: Params,
    /// The next context identifier to hand out.
    ///
    /// note: Stored rather than derived, because an identifier is never reused - not even one
    /// belonging to an item that [`Kernel::undo`] took away again.
    pub next_item: u64,
    /// The tool call identifiers the session has already used, sorted.
    ///
    /// note: Without these, a resumed session has nothing to check a provider's identifiers
    /// against, and would accept one it had used before the snapshot - the reuse
    /// [`Event::ToolCallRepaired`] exists to catch.
    pub used_calls: Vec<ToolCallId>,
    /// The sequence number of the last record in the session's log, as the items were read.
    ///
    /// note: read under the same lock as the items, so the records up to it are exactly the ones
    /// whose changes the items show. A caller writing a log beside a snapshot writes those, and
    /// the two agree.
    ///
    /// note: so that a resumed log carries on from it rather than starting again at 1. A client
    /// keeping a [`Kernel::history_since`](crate::Kernel::history_since) cursor across a resume
    /// would otherwise read nothing until the new log passed the old number, and a log kept
    /// across the resume would number two records the same.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes, and numbers
    /// from 1 as it always did.
    #[serde(default)]
    pub last_seq: u64,
    /// The next permission request identifier to hand out.
    ///
    /// note: for the reason `last_seq` is here: a question asked after a resume would otherwise
    /// carry the identifier of one asked before it, in a log that has both. `0` in a snapshot
    /// written before this existed, which resumes as `1`.
    #[serde(default)]
    pub next_permission: u64,
    /// What the [`TokenCounter`](crate::TokenCounter) had learned, if it learns at all.
    ///
    /// note: The one piece of a seam's own state a snapshot carries, and it is here for the same
    /// reason as everything else: it is easy to lose and nothing else can recover it. A session
    /// long enough to be worth resuming has already told its counter what several requests really
    /// cost; resuming at `1.0` would spend the next few relearning it, and would report a budget
    /// that quietly disagreed with the one the session was closed on.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes - it just
    /// resumes with nothing learned, which is exactly what it had.
    #[serde(default)]
    pub calibration: Option<Calibration>,
}

/// Past this, a number in a snapshot leaves no room to count on from; see [`Snapshot::problems`].
const ROOM: u64 = u64::MAX / 2;

impl Snapshot {
    /// What is wrong with this snapshot, in words; empty if nothing is.
    ///
    /// note: what [`Kernel::resume`] would have to repair or could not, for a caller that would
    /// rather refuse a snapshot than resume a repaired one. A snapshot is a record, and one read
    /// back from a file may have been edited, merged or written by something else. Resuming
    /// repairs what it can - an identifier two items share, or `0`, gets the next free one, and
    /// every call the items name is reserved whether or not `used_calls` lists it - and cannot
    /// repair a number too near the top of a `u64` to count on from.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();

        let mut held = std::collections::HashSet::new();
        for item in &self.items {
            if item.id.0 == 0 {
                problems.push("an item has no identifier (0)".to_owned());
            } else if !held.insert(item.id) {
                problems.push(format!("two items are both numbered {}", item.id));
            }
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

        let highest = self.items.iter().map(|item| item.id.0).max().unwrap_or(0);
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

/// Every tool call identifier an item names: the calls a turn made, and the one a result answers.
pub(crate) fn named_calls(item: &ContextItem) -> Vec<&ToolCallId> {
    let mut named: Vec<_> = item.calls().map(|call| &call.id).collect();
    if let crate::context::ContextKind::ToolResult { call, .. } = &item.kind {
        named.push(call);
    }
    named
}
