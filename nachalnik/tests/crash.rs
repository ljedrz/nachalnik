//! Losing the process in the middle of a turn, and what is still true afterwards.
//!
//! note: two questions, and they are not the same one. *What did the kernel promise* - which
//! items, which records, which call identifiers survive a snapshot - is answered here against the
//! kernel. *Did the thing actually happen* - did the payment go through, was the file written -
//! is not the kernel's to answer and never can be: it does not perform the side effect and has no
//! way to ask the system that did. What it provides is a durable name for the attempt, and these
//! tests are about how far that gets an application and where its own work begins.
//!
//! note: the crash is a dropped `Kernel` and a resume from whatever had been written down. That
//! is the same shape as `SIGKILL` for everything being checked here, and it is deterministic,
//! which a real signal in a test is not.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

use nachalnik::{
    BoxError, Capability, Config, ContextItem, ContextKind, Kernel, ModelResponse, OutputSink,
    Record, Snapshot, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec, async_trait,
    test::{AllowAll, ScriptedProvider, call},
};
use parking_lot::Mutex;
use serde_json::json;

// ------------------------------------------------------------------- the world outside the kernel

/// A service that charges money, and remembers what it has already charged for.
///
/// note: the idempotency key is the whole of what makes this recoverable, and it is deliberately
/// *not* something the kernel invented. It is the `ToolCallId` the model's own call already
/// carries - which survives a snapshot, is refused for reuse afterwards, and is therefore a name
/// for this attempt that outlives the process. An application does not need a new execution id
/// from the runtime; it needs to pass the one it already has to whoever is doing the work.
#[derive(Default)]
struct Ledger {
    charged: Mutex<BTreeMap<String, u64>>,
    attempts: AtomicUsize,
}

impl Ledger {
    /// Charges once per key, however many times it is asked.
    fn charge(&self, key: &str, amount: u64) -> u64 {
        self.attempts.fetch_add(1, SeqCst);
        let mut charged = self.charged.lock();
        *charged.entry(key.to_owned()).or_insert(amount)
    }

    /// What the application asks on resume: did this one go through?
    fn committed(&self, key: &str) -> Option<u64> {
        self.charged.lock().get(key).copied()
    }

    fn distinct(&self) -> usize {
        self.charged.lock().len()
    }

    fn attempts(&self) -> usize {
        self.attempts.load(SeqCst)
    }
}

/// Charges the ledger, and can be told to lose the process on the way.
struct Charge {
    ledger: Arc<Ledger>,
    /// Set to lose the process *after* the money moves and before the kernel is told it did.
    ///
    /// note: it blocks forever rather than returning an error, because those are not the same
    /// event and the difference is the whole test. A tool that returns `Err` has *answered*: the
    /// kernel records a failure, the model reads it, and nothing is outstanding. A process that
    /// dies mid-call answers nothing, and the way to produce that here is a call the kernel never
    /// gets back from, whose future is then dropped - which is what the kernel documents as
    /// leaving the claimed calls unrecorded.
    crash_after_effect: bool,
}

#[async_trait]
impl Tool for Charge {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            id: "charge".to_owned(),
            description: "charge the account".to_owned(),
            schema: Arc::new(json!({ "type": "object" })),
            capabilities: vec![Capability::Network],
            output_limit: None,
        }
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        // the call's own id, passed through as the idempotency key. Nothing else here is durable:
        // a key minted inside `invoke` would be gone with the process that minted it
        let charged = self.ledger.charge(&call.id.0, 100);
        if self.crash_after_effect {
            // the process dies here. The money has moved and nothing has recorded that it did
            std::future::pending::<()>().await;
        }

        Ok(ToolOutput::new(format!("charged {charged}")))
    }
}

// ------------------------------------------------------------------------- the application's disk

/// What an application writes down, and all it has after a crash.
///
/// note: the snapshot and the records are two separate writes, which is the whole of the ordering
/// problem: whichever goes second is the one a crash can lose.
#[derive(Default)]
struct Disk {
    snapshot: Option<Snapshot>,
    log: Vec<Record>,
}

impl Disk {
    /// The sequence number of the last record safely on disk.
    fn logged_through(&self) -> u64 {
        self.log.last().map(|record| record.seq).unwrap_or(0)
    }
}

/// The checkpoint protocol these tests are about.
///
/// note: **copy, write, then drop.** `history_since` hands back clones and leaves the kernel
/// holding them; `drain_history` hands back the only copy there is. Draining first and writing
/// second opens a window in which the records exist nowhere - not in the kernel, which has let
/// them go, and not on the disk, which has not got them yet - and a crash inside that window
/// loses them silently. Writing first and draining second cannot lose anything: the worst a crash
/// there can do is leave the kernel still holding records the disk already has.
///
/// note: the snapshot goes last for the same reason turned round. A snapshot written ahead of the
/// records is a state nothing in the log accounts for; a snapshot written behind them is a state
/// the log can explain, which is a gap somebody can close by reading. Of the two orders only one
/// leaves every crash recoverable.
fn checkpoint(kernel: &Kernel, disk: &mut Disk) {
    let through = kernel.last_seq();

    let records = kernel.history_since(disk.logged_through());
    disk.log.extend(records);

    kernel.drain_history(through);
    disk.snapshot = Some(kernel.snapshot());
}

// ---------------------------------------------------------------------------------- what survives

#[tokio::test]
async fn a_call_that_moved_money_and_never_answered_is_findable_afterwards() {
    let ledger = Arc::new(Ledger::default());
    let mut disk = Disk::default();

    {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(vec![call("pay-1", "charge", json!({}))]),
            ModelResponse::text("done"),
        ])));
        kernel.set_policy(Arc::new(AllowAll));
        kernel.add_tool(Arc::new(Charge {
            ledger: ledger.clone(),
            crash_after_effect: true,
        }));
        kernel.push(ContextItem::user("pay the invoice"));

        // the request, which records the assistant turn and the call it asked for
        kernel.step().await.unwrap();

        // and the execution, which charges and never comes back. Dropping the future is the
        // crash: the kernel documents that the calls it had claimed go with it and their results
        // are simply never recorded, which is exactly the state a killed process leaves behind
        let died = tokio::time::timeout(std::time::Duration::from_millis(50), kernel.step()).await;
        assert!(died.is_err(), "the call should not have come back");

        checkpoint(&kernel, &mut disk);
    }

    // the money moved
    assert_eq!(ledger.committed("pay-1"), Some(100));

    // and the kernel, resumed from the disk, can name the call that did it
    let snapshot = disk.snapshot.take().expect("checkpointed");
    let resumed = Kernel::resume(Config::default(), snapshot);
    let outstanding = unanswered(&resumed);
    assert_eq!(
        outstanding,
        vec![ToolCallId("pay-1".to_owned())],
        "the call the model made and never got an answer to"
    );

    // what the kernel cannot say is whether the charge went through - it did not make it. The
    // application asks the system that did, with the identifier the kernel kept for it
    assert_eq!(
        ledger.committed(&outstanding[0].0),
        Some(100),
        "reconciliation is the application's, and the key is the call's own id"
    );
}

#[tokio::test]
async fn the_same_call_replayed_against_the_ledger_does_not_charge_twice() {
    let ledger = Arc::new(Ledger::default());
    let tool = Charge {
        ledger: ledger.clone(),
        crash_after_effect: false,
    };

    // the crash happened after the money moved, so a resumed run that decides to retry sends the
    // same key again. The guarantee that this is safe is the ledger's, not the kernel's - the
    // kernel's half is that the key is still the same after a snapshot
    for _ in 0..3 {
        let _ = tool
            .invoke(
                &call("pay-1", "charge", json!({})),
                OutputSink::disconnected(),
            )
            .await;
    }

    assert_eq!(ledger.attempts(), 3, "it was asked three times");
    assert_eq!(ledger.distinct(), 1, "and charged once");
    assert_eq!(ledger.committed("pay-1"), Some(100));
}

#[tokio::test]
async fn a_resumed_session_refuses_to_reuse_the_identifier() {
    let ledger = Arc::new(Ledger::default());
    let mut disk = Disk::default();

    {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(vec![call("pay-1", "charge", json!({}))]),
            ModelResponse::text("done"),
        ])));
        kernel.set_policy(Arc::new(AllowAll));
        kernel.add_tool(Arc::new(Charge {
            ledger: ledger.clone(),
            crash_after_effect: false,
        }));
        kernel.push(ContextItem::user("pay the invoice"));
        kernel.turn().await.unwrap();
        checkpoint(&kernel, &mut disk);
    }

    // this is what stops a reconciliation from becoming a second charge by accident: the
    // identifier the application is keying on cannot come round again in the resumed session
    let snapshot = disk.snapshot.take().expect("checkpointed");
    assert!(
        snapshot
            .used_calls
            .contains(&ToolCallId("pay-1".to_owned())),
        "the snapshot carries what has already been called: {:?}",
        snapshot.used_calls
    );
}

// -------------------------------------------------------------------------- and what the log says

#[tokio::test]
async fn writing_before_draining_is_what_makes_a_crash_survivable() {
    let mut disk = Disk::default();
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("one"));
    kernel.push(ContextItem::user("two"));

    let before = kernel.last_seq();
    assert!(before > 0, "something was recorded");

    checkpoint(&kernel, &mut disk);
    assert_eq!(
        disk.logged_through(),
        before,
        "every record up to the checkpoint is on the disk"
    );
    assert!(
        kernel.history_since(0).is_empty(),
        "and the kernel is no longer holding them"
    );

    // the same again, and the log does not repeat itself: what is written is what is new
    kernel.push(ContextItem::user("three"));
    let logged = disk.log.len();
    checkpoint(&kernel, &mut disk);
    assert!(disk.log.len() > logged, "the third one was written");

    let mut seqs: Vec<u64> = disk.log.iter().map(|record| record.seq).collect();
    let sorted = seqs.clone();
    seqs.dedup();
    assert_eq!(seqs.len(), sorted.len(), "no record is written twice");
    assert!(sorted.windows(2).all(|at| at[0] < at[1]), "and in order");
}

#[tokio::test]
async fn a_crash_between_the_log_and_the_snapshot_replays_rather_than_loses() {
    let mut disk = Disk::default();
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("one"));

    // the first checkpoint lands whole
    checkpoint(&kernel, &mut disk);
    let older = disk.snapshot.clone().expect("checkpointed");

    // and the second one gets as far as the log before the process dies
    kernel.push(ContextItem::user("two"));
    let records = kernel.history_since(disk.logged_through());
    disk.log.extend(records);
    // ... and no snapshot is written. `disk.snapshot` is still the older one
    drop(kernel);

    // what comes back is the older state, which is behind the log rather than ahead of it
    let resumed = Kernel::resume(Config::default(), older);
    assert_eq!(
        resumed.items().len(),
        1,
        "the snapshot is the one before `two`"
    );
    // and the log is what says so: it holds records the resumed state has not produced, which is
    // a gap an application can see and close. The other order - a snapshot ahead of the log -
    // leaves a state nothing accounts for, and nothing to read to find out what happened
    assert!(
        disk.logged_through() > 0,
        "the log ran ahead, which is the recoverable direction"
    );
}

/// Every call in the context that has no result answering it.
///
/// note: written here rather than asked of the kernel, because it is a question about a *context*
/// and the answer is four lines of reading one. The kernel's part is that both halves survive a
/// snapshot with their identifiers intact, which is what makes the four lines possible at all.
fn unanswered(kernel: &Kernel) -> Vec<ToolCallId> {
    let items = kernel.items();
    let answered: Vec<&ToolCallId> = items
        .iter()
        .filter_map(|item| match &item.kind {
            ContextKind::ToolResult { call, .. } => Some(call),
            _ => None,
        })
        .collect();

    items
        .iter()
        .flat_map(|item| item.calls().map(|call| call.id.clone()).collect::<Vec<_>>())
        .filter(|id| !answered.contains(&id))
        .collect()
}
