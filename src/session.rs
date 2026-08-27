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
};

/// A single entry in a session's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Returns the sequence number of the most recent record; `0` if there is none.
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Returns the records whose sequence number is greater than `seq`.
    pub fn since(&self, seq: u64) -> impl Iterator<Item = &Record> {
        self.records.iter().filter(move |r| r.seq > seq)
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

    /// Appends an event, returning the record it became.
    pub(crate) fn append(&mut self, event: Event) -> Record {
        self.seq += 1;
        let record = Record {
            seq: self.seq,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default(),
            event,
        };

        self.records.push_back(record.clone());

        record
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
/// note: What is *not* here is anything transient. A resumed kernel starts
/// [`State::Idle`](crate::State) with nothing pending, because a permission that nobody is
/// around to answer is not worth restoring; a tool call left without a result is repaired out of
/// the next request by the [`Projector`](crate::Projector), and says so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// note: Without these, a resumed session could hand out an identifier it has used before,
    /// which is the whole thing [`Event::ToolCallRepaired`] exists to prevent.
    pub used_calls: Vec<ToolCallId>,
}
