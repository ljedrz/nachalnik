//! The tool that reads the session's own record: what happened, in the order it happened, in the
//! kernel's words.
//!
//! note: the log is not the context, and this tool makes the difference usable. A context is what
//! the model is carrying and is sent with every request; the log sits beside it, costs nothing
//! until something asks for it, and holds the things a context cannot - what an item *used* to say,
//! which permissions were answered and how, which tools appeared and went away. `context` reads the
//! first, this reads the second, and neither can answer the other's question.
//!
//! note: every answer opens with the true total, which is what makes the tool safe to give a model.
//! It is [`budget`](super::context) applied to the log: price it before you carry it. A filtered
//! answer states what exists as well as what matched, so a short reply is self-describing and
//! truncation cannot read as absence, which matters most here, because the one wrong answer a log
//! can give is *nothing happened*.
//!
//! note: raw rather than digested. The records come back in order, named the way the kernel names
//! them and detailed the way the trace pane details them, with no sentence about what any of it
//! *means*. The histogram counts every kind rather than flagging an interesting one: a count is not
//! a flag, and hiding a cheap true fact to keep a reading interesting would be the wrong trade.

use std::collections::{BTreeMap, BTreeSet};

use nachalnik::{
    BoxError, Content, ContextId, Event, Kernel, OutputSink, Record, Tool, ToolCall, ToolOutput,
    ToolSpec, async_trait,
};
use serde_json::Value;
use std::sync::Arc;

use crate::{
    app::text::{thousands, trace_line},
    tools::{
        Limits, domains, number,
        ops::{Arg, Layout, Op, actions, inner, schema, unread},
        yes_or_no,
    },
    wiring::Earlier,
};

use super::{Reach, if_reachable, unknown};
use crate::tools::Careful;

/// How wide the event-name column is, which is the longest name plus a space.
const NAMES: usize = 20;

/// The one operation, reading the log, and the five ways of narrowing it.
///
/// note: one operation, asked for by name like every other tool; each takes an `action`, so none is
/// an exception a model must remember. `shell` is written the same way for the same reason.
fn ops() -> Vec<Op> {
    vec![Op::new(
        "read",
        "",
        vec![
            Arg::whole(
                "take",
                "the most recent N of whatever matched, up to 64; left out, all of them. The \
                 header says how many there are either way",
            ),
            Arg::list(
                "ids",
                "integer",
                "only the records naming these context items",
            ),
            // note: `0` is said out loud, because it is not guessable and guessing is costly.
            // `since: N` is exclusive: it means after record N. A model asking for "everything"
            // writes the first number it has seen, which drops exactly that record. In a resumed
            // session that is `session.resumed`, which is always the first and is often the answer
            // to the question
            Arg::whole(
                "since",
                "only the records after this sequence number, which is the first column; `0` is \
                 all of them",
            ),
            // note: two of them written out, because "spelled as the summary spells them" alone
            // sends a model to the summary for the vocabulary it needs before it can ask. The
            // summary still has the list, of this session's kinds rather than every kind there is,
            // and a model that has not read one now knows the shape of a name
            Arg::list(
                "kinds",
                "string",
                "only these kinds, as `context.added` or `permission.decided`; the summary lists \
                 the ones this session has",
            ),
            // note: the same word `context`'s `look` uses for the same trade. Without it a
            // replacement is shown as its first line; with it the whole item text arrives in your
            // context and costs what it costs
            Arg::truth(
                "whole",
                "print a replaced item's old text entire rather than its first line. It costs \
                 what that text costs",
            ),
        ],
    )]
}

/// Reads the session log: what happened, of what kinds, and what taking it would cost.
pub struct Log {
    reach: Reach,
    limits: Limits,
    /// The records of the session this one was resumed from, which come before the kernel's.
    earlier: Earlier,
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Log {
    /// Builds one; see [`super::build`], which is the only caller.
    pub(super) fn new(reach: Reach, limits: Limits, earlier: Earlier, layout: Layout) -> Self {
        let ops = ops();
        Self {
            reach,
            limits,
            earlier,
            schema: Arc::new(schema(&ops, layout)),
            ops,
        }
    }

    /// What a call asks of the log, or why it cannot be answered.
    fn query(&self, call: &ToolCall) -> Result<Query, String> {
        // note: a word this tool does not have is refused by name rather than read as `read`. An
        // absent `action` is still `read`; a model that asked to `clear` the log is told there is
        // no such thing, not handed the log
        let args = inner(&call.args).map_err(|misread| misread.said(Layout::of(&self.schema)))?;
        let named = &args["action"];
        if !named.is_null() && named.as_str() != Some("read") {
            let named = named
                .as_str()
                .map_or_else(|| named.to_string(), str::to_owned);
            return Err(unknown(&named, &actions(&self.ops)));
        }

        Query::read(&args, &self.ops)
    }
}

#[async_trait]
impl Tool for Log {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "log",
            "reads the session's own record: an append-only log of what happened, kept beside \
             your context and not part of it. Every item added, replaced, elided, undone or \
             compacted; every permission asked for and answered; every tool that appeared or went \
             away. On its own it says how many records there are, of what kinds, and what they \
             would cost you, and the arguments narrow that down and hand them over. Every \
             answer opens with the true total, so a short one is never absence. You cannot write \
             to it. A replacement keeps what the item said before, which once it leaves the undo \
             window is nowhere else at all.",
        )
        .with_schema(self.schema.clone())
        .with_capabilities([domains::log("read")])
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    fn unparsed(&self, written: &str) -> Option<String> {
        crate::tools::ops::advice(written, Layout::of(&self.schema))
    }

    /// note: what `invoke` refuses before it reads the log, so a call that could never be answered
    /// is not put to somebody to allow first.
    fn malformed(&self, call: &ToolCall) -> Option<String> {
        self.query(call).err()
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let query = match self.query(call) {
            Ok(query) => query,
            Err(e) => return Ok(ToolOutput::error(e)),
        };
        let kernel = self.reach.kernel()?;

        // taken before the session lock rather than inside it: the closure below must not call back
        // into the kernel, and `counter()` is a call into the kernel
        let counter = kernel.counter();
        // note: the records of the session this one was resumed from, then the kernel's, read as
        // one log. A resumed kernel numbers on from the last record its snapshot reflects, so the
        // two meet at `session.resumed` with nothing missing between them. The total, the kinds,
        // `since`, and the items with a beginning cover the whole log
        let earlier = self.earlier.records();

        // note: filtering and rendering both happen under the lock because it is cheaper than
        // `Kernel::history` cloning every record out. Formatting does not call back into the
        // kernel; the warning on `with_history` is about that
        let read = kernel.with_history(|session| {
            let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
            let mut every = String::new();
            let mut matched: Vec<String> = Vec::new();
            let mut added = BTreeSet::new();
            let mut hits = 0;
            let mut lost = Vec::new();
            let mut before: Option<u64> = None;

            for record in earlier.iter().chain(session.records()) {
                if let Some(before) = before.filter(|before| record.seq > before + 1) {
                    lost.push((before + 1, record.seq - 1));
                }
                before = Some(record.seq);
                *kinds.entry(record.event.name()).or_default() += 1;
                if let Event::ContextAdded { id, .. } = &record.event {
                    added.insert(*id);
                }
                // as the matches are, or the total is priced from a shorter log than the part of
                // it that matched: `whole` prints a replaced item's old text entire
                every.push_str(&line(record, query.whole));
                if query.wants(record) {
                    hits += 1;
                    matched.push(line(record, query.whole));
                }
            }

            let first = earlier.first().or_else(|| session.records().next());
            Read {
                total: earlier.len() + session.len(),
                last_seq: session.last_seq(),
                first_seq: first.map(|r| r.seq).unwrap_or_default(),
                resumed: matches!(first.map(|r| &r.event), Some(Event::SessionResumed { .. })),
                kinds,
                every,
                matched,
                hits,
                added,
                lost,
            }
        });

        Ok(ToolOutput::new(query.report(
            &kernel,
            &read,
            &*counter,
            self.reach.policy(),
        )))
    }
}

/// Everything one pass over the log produced, so the pass happens once.
struct Read {
    /// How many records there are, whatever matched.
    total: usize,
    /// The highest sequence number, which is what `since` is measured against.
    last_seq: u64,
    /// How many records of each kind, in the kernel's own names.
    kinds: BTreeMap<&'static str, usize>,
    /// Every record, rendered as the matches are, which is what "if you take them all" is priced
    /// from.
    every: String,
    /// The records that matched, rendered, one string per record.
    ///
    /// note: one string per record because `take` returns the most recent N of whatever matched,
    /// and a record is not a line: `whole` prints a replaced item's old text entire, so one record
    /// can be forty lines. Counting in lines would give the tail of somebody's old file, with no
    /// sequence number or event name, under a header calling it a count of records.
    matched: Vec<String>,
    /// How many those were.
    hits: usize,
    /// The sequence number of the oldest record still here; `0` when there are none.
    ///
    /// note: with [`Read::resumed`], this says which of the two records before it are missing.
    /// `Session::drain_through` removes records but leaves the counter alone, so a log whose first
    /// record is not 1 does not hold everything that happened. Reporting that as "nothing happened"
    /// would be wrong.
    first_seq: u64,
    /// Whether this log begins at a session that was picked back up.
    ///
    /// note: `first_seq > 1` has two opposite causes. A resumed session numbers on from the record
    /// the snapshot left off after, so it always starts above 1. Reading the number alone would
    /// falsely tell a model its earlier records were drained, and says nothing on paths where the
    /// number is never printed. The event at the head of the log tells them apart.
    resumed: bool,
    /// The items this log holds a `context.added` for.
    ///
    /// note: kept so the absence of one can be reported, which is what an `ids` filter usually
    /// wants. An item with no creation record here was in the context before this log began, and
    /// nothing else in an answer says so.
    added: BTreeSet<ContextId>,
    /// The runs of records missing between two that are here, oldest first. A run killed after its
    /// snapshot was taken numbered them, and the session carried on from the snapshot without what
    /// they did.
    ///
    /// note: anywhere in the log, not only where the records read before the resume end and this
    /// session's own begin. A resumed session's record copies the one before it forward, so a gap
    /// left at one resume is in the middle of the log by the next.
    lost: Vec<(u64, u64)>,
}

impl Read {
    /// Whether a `since` below the oldest record held asked for records this log has not got.
    fn skipped(&self, since: u64) -> bool {
        self.first_seq > 0 && since + 1 < self.first_seq
    }
}

/// What a call asked for, and how to say it back.
///
/// note: the filters are read and validated before the log is touched, so a malformed one comes
/// back as something to correct rather than as an empty result. An empty result reads as *nothing
/// happened*, and for a session log that is the one wrong answer nobody would catch.
#[derive(Default)]
struct Query {
    take: Option<usize>,
    ids: Vec<ContextId>,
    since: Option<u64>,
    kinds: Vec<String>,
    whole: bool,
}

impl Query {
    /// Reads one, or says what is wrong with the arguments.
    fn read(args: &Value, ops: &[Op]) -> Result<Self, String> {
        if let Some(refusal) = unread("read", args, ops) {
            return Err(refusal);
        }

        let mut query = Self {
            whole: yes_or_no(args, "whole")?,
            ..Self::default()
        };

        let not_read = |refusal| format!("{refusal} {UNREAD}");
        if let Some(take) = number(args, "take").map_err(not_read)? {
            if take == 0 {
                return Err(
                    "`take: 0` asks for no records; leave it out to get the summary, \
                            which is what a bare call is"
                        .to_owned(),
                );
            }
            query.take = Some(take as usize);
        }
        query.since = number(args, "since").map_err(not_read)?;
        // note: the reader every tool here shares, so an entry that is not an item number is
        // refused rather than dropped. An *empty* list is no filter, which is how a model passing
        // every argument the schema lists spells one
        query.ids = super::ids(args, "ids")?;
        if !args["kinds"].is_null() {
            let Some(kinds) = args["kinds"].as_array() else {
                return Err(
                    "`kinds` is a list of names, as `kinds: [\"context.replaced\"]`; a \
                            bare call lists the ones this session holds"
                        .to_owned(),
                );
            };
            // the same: `kinds: []` is no constraint, and saying so costs nothing. An entry that
            // is not a name is refused with the rest, since dropping it answers a narrower
            // question than the one asked
            query.kinds = kinds
                .iter()
                .map(|kind| kind.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    "`kinds` is a list of names and not every one of these is one; a bare call \
                     lists the ones this session holds"
                        .to_owned()
                })?;
        }

        Ok(query)
    }

    /// Whether anything was asked for beyond the summary.
    fn filtered(&self) -> bool {
        self.take.is_some() || self.narrowed()
    }

    /// Whether anything changes which records *count*, as against how many are shown.
    ///
    /// note: two different questions. Answered as one, a call carrying only `take` would report a
    /// match count that is really the total, beside a filter description that is empty because
    /// there is no filter. `take` shortens an answer; these three decide what the answer is of.
    fn narrowed(&self) -> bool {
        !self.ids.is_empty() || self.since.is_some() || !self.kinds.is_empty()
    }

    /// Whether this record is one of the ones asked for.
    fn wants(&self, record: &Record) -> bool {
        if let Some(since) = self.since
            && record.seq <= since
        {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.iter().any(|kind| kind == record.event.name()) {
            return false;
        }
        if !self.ids.is_empty() && !self.ids.iter().any(|id| names(&record.event, *id)) {
            return false;
        }

        true
    }

    /// The filters, spelled the way they were asked for, so the header says what it answered.
    fn said(&self) -> String {
        let mut parts = Vec::new();
        if let Some(since) = self.since {
            parts.push(format!("since:{since}"));
        }
        if !self.kinds.is_empty() {
            parts.push(format!("kinds:{:?}", self.kinds));
        }
        if !self.ids.is_empty() {
            let numbers: Vec<String> = self.ids.iter().map(|id| id.0.to_string()).collect();
            parts.push(format!("ids:[{}]", numbers.join(", ")));
        }

        parts.join(" ")
    }

    /// The answer: the true total first, then whatever was asked for.
    fn report(
        &self,
        kernel: &Kernel,
        read: &Read,
        counter: &dyn nachalnik::TokenCounter,
        policy: &Careful,
    ) -> String {
        let all = counter.count(&Content::text(read.every.clone()));

        if read.total == 0 {
            // note: an empty log has two causes, opposite answers to the question being asked:
            // nothing has happened yet, or everything has been drained out and written somewhere
            // else. `Kernel::drain_history` is supported for a long session and leaves the sequence
            // counter alone, which is how this can tell. Reporting drained as nothing-happened is
            // the one mistake this tool must not make.
            return match read.last_seq {
                0 => "nothing has been recorded yet: this session's log is empty because nothing \
                      has happened, not because you are being kept from it.\n"
                    .to_owned(),
                seq => format!(
                    "this log is empty and {seq} record(s) have been through it. They were drained \
                     - taken out and kept somewhere this session cannot read - which is how a long \
                     session is stopped from growing forever. Nothing that happens from here on is \
                     affected, and the next record will be number {}.\n",
                    seq + 1,
                ),
            };
        }

        // the true total, on every answer, whatever was asked for. It makes a short reply
        // self-describing rather than indistinguishable from an empty session
        let mut out = format!(
            "{} records, ~{} tokens",
            thousands(read.total),
            thousands(all)
        );

        if !self.filtered() {
            out.push_str(
                " if you take them all. Nothing here is in your context until you ask for it.\n",
            );
            out.push_str(&histogram(read));
            // and where the count above is not the whole session, which a total otherwise claims to
            // be: the first thing a resumed session asks its log about is what came before
            out.push_str(&apart(earlier(read)));
            out.push_str(&format!(
                "\n`take`, `ids`, `since` or `kinds` asks for the records themselves; the last \
                 sequence number is {}.\n",
                read.last_seq,
            ));

            return out;
        }
        out.push_str(" in all.");

        // only where something decided which records *count*. A call carrying `take` alone has
        // matched everything, so "15 match" against a total of 15 adds nothing
        if self.narrowed() {
            let matched = counter.count(&Content::text(read.matched.concat()));
            out.push_str(&format!(
                " {} match {}, ~{} tokens.",
                thousands(read.hits),
                self.said(),
                thousands(matched),
            ));

            if read.hits == 0 {
                // a zero beside a non-zero total stops it looking like nothing happened. The kinds
                // go with it because a filter that matched nothing is usually for another session
                out.push_str(" Nothing matched; these are the kinds this session holds:\n");
                out.push_str(&histogram(read));
                // and the records this log does not hold, because a filter for a kind this session
                // never emitted is the shape a resumed one is in: the kind is in the log written
                // beside the snapshot, not here
                out.push_str(&earlier(read));
                // an `ids` filter that matched nothing is the shape an inherited item makes, so
                // "nothing matched" is the least useful way to say so
                out.push_str(&self.inherited(kernel, read, policy));

                return out;
            }
        }

        // `take` counts from the end, because a log is read from the end - but the records stay in
        // the order they happened, which is the order everything else here reports them in
        //
        // note: clamped, and said where the figure is. A log is the one thing here that grows
        // without a call asking it to - a session's own record is append-only and grows for as long
        // as the run does - so `take: 999999` on a real log was once the whole of it in one answer,
        // which the compactor then elided on the way in: a session that read its own log in bulk
        // could not read it again to find out why, because the record of the read was in what the
        // read had replaced with a marker
        let capped = self.take.map(|take| take.min(super::TAKE));
        let shown = match capped {
            Some(take) => take.min(read.matched.len()),
            None => read.matched.len(),
        };
        let beyond = read.matched.len() - shown;
        match beyond {
            0 => out.push_str(&format!(" Showing {}.\n", thousands(shown))),
            // The number not here is stated as a figure, not implied by the count, because a
            // shortened log must say how much is not here. The word is "older" when nothing
            // narrowed what counts
            more => out.push_str(&format!(
                " Showing the {} most recent; {} {} not here.\n",
                thousands(shown),
                thousands(more),
                match self.narrowed() {
                    true => "more match and are",
                    false => "older are",
                },
            )),
        }
        // note: said only where it stopped the answer short, because a `take` of a hundred on a
        // log of nine records is a number nobody needed bounding. And it says what narrows rather
        // than how to page, because this one can page: `since` is the cursor, and the figure it
        // wants is the one on the record it stops at
        if let Some(take) = self.take
            && take > super::TAKE
            && beyond != 0
        {
            out.push_str(&format!(
                "`take` is at most {}, so the {beyond} above are not here; `kinds`, `ids` or a \
                 `since` past the last record shown reach them.\n",
                thousands(super::TAKE)
            ));
        }
        // `since` below the oldest record held is a resumed session: the filter asks for records
        // this log has never had. Without this message it says so as silence between the count and
        // the records, which reads as there being none
        match self.since {
            Some(since) if read.skipped(since) => {
                out.push_str(&format!(
                    "\nNothing numbered {} to {} is here, so `since: {since}` skipped them.\n",
                    since + 1,
                    read.first_seq - 1,
                ));
                out.push_str(&earlier(read));
            }
            // Every other answer that matched, because `kinds` over a resumed log counts only what
            // happened since the resume
            _ => out.push_str(&apart(earlier(read))),
        }
        out.push_str(&self.inherited(kernel, read, policy));
        out.push('\n');
        out.push_str(&read.matched[beyond..].concat());

        out
    }

    /// What an `ids` filter found no beginning for, which is usually what it was really asking.
    ///
    /// note: an absence, and the one answer a list of matching records cannot give. A session
    /// resumed under a second model that asks the log where an inherited item came from gets
    /// `model.requested` rows naming that item - every one true, because the item has been in every
    /// request since - and can read them as proof it wrote the item itself. What decides the
    /// question is the record that is not there.
    fn inherited(&self, kernel: &Kernel, read: &Read, policy: &Careful) -> String {
        // an item the kernel has never held has no beginning; calling it inherited would tell the
        // model a history that did not happen
        let (unborn, never): (Vec<&ContextId>, Vec<&ContextId>) = self
            .ids
            .iter()
            .filter(|id| !read.added.contains(id))
            .partition(|id| kernel.item(**id).is_some());
        let never = match never.is_empty() {
            true => String::new(),
            false => format!("\n{} is no item this session has held.\n", numbered(&never)),
        };
        if unborn.is_empty() {
            return never;
        }

        let (has, they, them) = match unborn.len() {
            1 => ("has", "it was", "it"),
            _ => ("have", "they were", "them"),
        };

        format!(
            "{never}\n{} {has} no `context.added` here: {they} already in the context before this \
             log begins, so nothing in it says where {them} came from or who wrote {them}. {}{}\n",
            numbered(&unborn),
            earlier(read),
            if_reachable(kernel, policy, "setup:read", || {
                " `setup` with `model` says whether this one did.".to_owned()
            }),
        )
    }
}

/// Where this log's records stop being the whole of what happened, when it does not hold them all.
///
/// note: the same sentence for every answer that turns on records this log cannot show, because the
/// two causes are opposite and a model told the wrong one is worse off than one told neither. A log
/// that starts above 1 either had records drained out of it - carried away and kept where this
/// session cannot read - or began at a resumed session, in which case the records before it belong
/// to the session it was resumed from and were never taken away. `since` below the first record and
/// an `ids` filter with no match are the same question, so both are answered with this. An empty
/// string is the answer for a log that starts at 1, where nothing is missing.
///
/// note: records missing from the middle, where a resume carried on from a snapshot the log beside
/// it had run past. They are left out because what they did is not in this session - an item they
/// added is not in its context - and they are said because a log read as one whole would otherwise
/// pass over them as if nothing had happened there.
fn earlier(read: &Read) -> String {
    let lost = (!read.lost.is_empty()).then(|| {
        let runs: Vec<String> = read
            .lost
            .iter()
            .map(|(from, to)| match from == to {
                true => from.to_string(),
                false => format!("{from} to {to}"),
            })
            .collect();
        let runs = match runs.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        };
        let (which, them) = match read.lost[..] {
            [(from, to)] if from == to => (format!("Record {runs} is"), "it"),
            _ => (format!("Records {runs} are"), "them"),
        };
        format!(
            "{which} not here: a run that stopped after its snapshot had been taken wrote {them}, \
             the session carried on from that snapshot, and what {them} did is not in it."
        )
    });
    let begins = (read.first_seq > 1).then(|| match read.resumed {
        true => format!(
            "This log begins at record {}, because the session was resumed; records before it \
             belong to the session it was resumed from and this one cannot read them.",
            read.first_seq
        ),
        false => format!(
            "This log starts at record {}, so earlier ones were drained and may have said.",
            read.first_seq
        ),
    });

    [begins, lost]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A sentence on a line of its own, or nothing where there is no sentence.
fn apart(sentence: String) -> String {
    match sentence.is_empty() {
        true => sentence,
        false => format!("\n{sentence}\n"),
    }
}

/// What a count that could not be read says was not done, the part a refusal from [`number`] leaves
/// to its caller.
const UNREAD: &str = "Nothing was read, rather than nothing being found: an empty answer here \
                      would have read as an empty log.";

/// Item numbers, as somebody would read them out.
fn numbered(ids: &[&ContextId]) -> String {
    let numbers: Vec<String> = ids.iter().map(|id| format!("[{}]", id.0)).collect();
    match numbers.len() {
        1 => numbers[0].clone(),
        _ => numbers.join(", "),
    }
}

/// How many of each kind, most first.
fn histogram(read: &Read) -> String {
    let mut counted: Vec<(&&str, &usize)> = read.kinds.iter().collect();
    // by count, and by name within a count, so two runs of the same session read the same way
    counted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));

    counted
        .iter()
        .map(|(kind, count)| format!("  {kind:<NAMES$} {:>6}\n", thousands(**count)))
        .collect()
}

/// One record, as a line: its sequence number, its kind, and what the kind has to say.
///
/// note: [`trace_line`] rather than a second renderer, so the words a model reads off the log are
/// the words a person reads off the trace pane. A program whose two accounts of one event differ
/// can be shown the same session and disagree about it.
fn line(record: &Record, whole: bool) -> String {
    let (name, detail) = trace_line(&record.event);
    let mut out = format!("{:>5}  {name:<NAMES$}  {detail}", record.seq);

    // the one event carrying content, and the only place this needs a word of its own. Without
    // `whole` the line says how much is not on it, because a first line that does not admit to
    // being one is the shape of thing this tool exists not to produce
    if let Event::ContextReplaced { was, .. } = &record.event {
        let text = was.to_text();
        let first = detail.len();
        match whole {
            true => out.push_str(&format!("\n       --- what it said, entire ---\n{text}")),
            false if text.len() > first => out.push_str(&format!(
                " [{} bytes in all; `whole` for them]",
                thousands(text.len())
            )),
            false => {}
        }
    }
    out.push('\n');

    out
}

/// Whether an event is about a given context item.
///
/// note: these are the events a question about an item is asked of, and they are why `ids` is worth
/// having: "what happened to 12?" is a question the context cannot answer, because the context only
/// holds what 12 says now.
fn names(event: &Event, id: ContextId) -> bool {
    match event {
        Event::ContextAdded { id: at, .. }
        | Event::ContextChanged { id: at, .. }
        | Event::ContextReplaced { id: at, .. }
        | Event::ContextAnnotated { id: at, .. } => *at == id,
        Event::ContextUndone {
            removed, changed, ..
        } => removed.contains(&id) || changed.contains(&id),
        Event::ContextRedone {
            restored, changed, ..
        } => restored.contains(&id) || changed.contains(&id),
        Event::ToolFinished { item, whole, .. } => *item == id || *whole == Some(id),
        Event::ModelRequested { items, skipped, .. } => {
            items.contains(&id) || skipped.iter().any(|left_out| left_out.id == id)
        }
        Event::Compacted { report } => {
            let named = |moved: &[nachalnik::ReportItem]| moved.iter().any(|one| one.id == id);
            named(&report.removed)
                || named(&report.elided)
                || named(&report.refused)
                || report.summary.as_ref().is_some_and(|one| one.id == id)
        }
        _ => false,
    }
}
