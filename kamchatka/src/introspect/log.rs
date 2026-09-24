//! The tool that reads the session's own record: what happened, in the order it happened, in the
//! kernel's words.
//!
//! note: the log is not the context and this is the difference the tool exists to make usable. A
//! context is what the model is carrying and is sent with every request; the log sits beside it,
//! costs nothing until something asks for it, and holds the things a context cannot - what an item
//! *used* to say, which permissions were answered and how, which tools appeared and went away.
//! `context` reads the first, this reads the second, and neither can answer the other's question.
//!
//! note: every answer opens with the true total, and that one rule is what makes the tool safe to
//! give a model. It is [`budget`](super::context) applied to the log: price it before you carry
//! it. A filtered answer states what exists as well as what matched, so a short reply is
//! self-describing and truncation cannot read as absence - which matters here more than anywhere
//! else, because the one wrong answer a log can give is *nothing happened*.
//!
//! note: raw rather than digested, deliberately. The records come back in order, named the way
//! the kernel names them and detailed the way the trace pane details them, with no sentence about
//! what any of it *means*. The histogram counts every kind rather than flagging an interesting
//! one: a count is not a flag, and a tool that hid a cheap honest fact to keep a reading
//! interesting would be the wrong trade for something people use for real.

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
        Limits, domains,
        ops::{Arg, Op, actions, inner, schema, unread},
        yes_or_no,
    },
};

use super::{Reach, if_offered, unknown};

/// How wide the event-name column is, which is the longest name plus a space.
const NAMES: usize = 20;

/// The one thing this does, and the five ways of narrowing it.
///
/// note: one operation, and asked for by name anyway, so that every tool this program offers takes
/// an `action` and none of them is the exception a model has to remember. `shell` is written the
/// same way and for the same reason.
fn ops() -> Vec<Op> {
    vec![Op::new(
        "read",
        "",
        vec![
            Arg::whole(
                "take",
                "the most recent N of whatever matched; left out, all of them. The header says \
                 how many there are either way",
            ),
            Arg::list(
                "ids",
                "integer",
                "only the records naming these context items",
            ),
            // note: `0` said out loud, because it is not guessable and the guess is costly. This
            // is exclusive - `since: N` means *after* record N - and a model reaching for
            // "everything" writes the first number it has seen, which drops exactly that record.
            // In a resumed session that is `session.resumed`, which is always the first and is
            // often the answer to the question
            Arg::whole(
                "since",
                "only the records after this sequence number, which is the first column; `0` is \
                 all of them",
            ),
            // note: two of them written out, because "spelled as the summary spells them" sends a
            // model to the summary for the vocabulary it needs before it can ask. The summary is
            // still where the list is - it is this session's kinds rather than every kind there
            // is - and a model that has not read one now knows the shape of a name
            Arg::list(
                "kinds",
                "string",
                "only these kinds, as `context.added` or `permission.decided`; the summary lists \
                 the ones this session has",
            ),
            // note: the same word `context: look` uses for the same trade, because it is the same
            // trade. Without it a replacement is shown as its first line; with it the whole of
            // what the item said arrives in your context and costs what it costs
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
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Log {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, limits: Limits) -> Self {
        let ops = ops();
        Self {
            reach,
            limits,
            schema: Arc::new(schema(&ops)),
            ops,
        }
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

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        // note: a word this tool does not have is refused by name rather than read as `read`,
        // which is what an absent `action` still is: a model that asked to `clear` the log should
        // be told there is no such thing, not handed the log
        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        let args = &*args;
        if let Some(named) = args["action"].as_str().filter(|it| *it != "read") {
            return Ok(ToolOutput::error(unknown(named, &actions(&self.ops))));
        }
        let kernel = self.reach.kernel()?;

        let query = match Query::read(args, &self.ops) {
            Ok(query) => query,
            Err(e) => return Ok(ToolOutput::error(e)),
        };
        // taken before the session lock rather than inside it: the closure below must not call
        // back into the kernel, and `counter()` is a call into the kernel
        let counter = kernel.counter();

        // note: the filtering and the rendering both happen in here, which reads like too much to
        // do under a lock and is the cheaper of the two shapes. `Kernel::history` would clone
        // every record to get them out - its own documentation says so and points here for a
        // search - and formatting is not a call back into the kernel, which is what the warning
        // on `with_history` is actually about
        let read = kernel.with_history(|session| {
            let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
            let mut every = String::new();
            let mut matched: Vec<String> = Vec::new();
            let mut added = BTreeSet::new();
            let mut hits = 0;

            for record in session.records() {
                *kinds.entry(record.event.name()).or_default() += 1;
                if let Event::ContextAdded { id, .. } = &record.event {
                    added.insert(*id);
                }
                every.push_str(&line(record, false));
                if query.wants(record) {
                    hits += 1;
                    matched.push(line(record, query.whole));
                }
            }

            Read {
                total: session.len(),
                last_seq: session.last_seq(),
                first_seq: session.records().next().map(|r| r.seq).unwrap_or_default(),
                kinds,
                every,
                matched,
                hits,
                added,
            }
        });

        Ok(ToolOutput::new(query.report(&kernel, &read, &*counter)))
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
    /// Every record, rendered, which is what "if you take them all" is priced from.
    every: String,
    /// The ones that matched, rendered, one entry per record.
    ///
    /// note: per record rather than one string, because `take` is "the most recent N of whatever
    /// matched" and a record is not a line: `whole` prints a replaced item's old text entire, so
    /// one record can be forty of them. Counted in lines, the last N would be the tail of
    /// somebody's old file with no sequence number and no event name in front of it, under a
    /// header calling that a count of records.
    matched: Vec<String>,
    /// How many those were.
    hits: usize,
    /// The sequence number of the oldest record still here; `0` when there are none.
    ///
    /// note: what says a log was *drained* rather than never written. `Session::drain_through`
    /// takes records out and leaves the counter alone, so a log whose first record is not 1 is one
    /// somebody has carried away - and an answer that reported that as "nothing happened" would be
    /// making the one mistake this tool exists not to make.
    first_seq: u64,
    /// The items this log holds a `context.added` for.
    ///
    /// note: kept so that the *absence* of one can be reported, which is the fact an `ids` filter
    /// is usually really after. An item with no creation record here was in the context before
    /// this log began, and nothing else in an answer says so.
    added: BTreeSet<ContextId>,
}

/// What a call asked for, and how to say it back.
///
/// note: the filters are read and validated before the log is touched, so a malformed one comes
/// back as something to correct rather than as an empty result. An empty result reads as *nothing
/// happened*, and for a session log that is the one answer that can be wrong in a way nobody
/// catches.
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

        if !args["take"].is_null() {
            let take = counted(&args["take"], "take")?;
            if take == 0 {
                return Err(
                    "`take: 0` asks for no records; leave it out to get the summary, \
                            which is what a bare call is"
                        .to_owned(),
                );
            }
            query.take = Some(take as usize);
        }
        if !args["since"].is_null() {
            query.since = Some(counted(&args["since"], "since")?);
        }
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
            // the same: `kinds: []` is no constraint, and saying so costs nothing
            query.kinds = kinds
                .iter()
                .filter_map(|kind| kind.as_str())
                .map(str::to_owned)
                .collect();
            if query.kinds.is_empty() && !kinds.is_empty() {
                return Err(
                    "`kinds` is a list of names and none of these is one; a bare call lists the \
                     ones this session holds"
                        .to_owned(),
                );
            }
        }

        Ok(query)
    }

    /// Whether anything was asked for beyond the summary.
    fn filtered(&self) -> bool {
        self.take.is_some() || self.narrowed()
    }

    /// Whether anything changes which records *count*, as against how many are shown.
    ///
    /// note: the two are not the same question. Answered as one, a call carrying only `take` would
    /// report a match count that is really the total, beside a filter description that is empty
    /// because there is no filter. `take` shortens an answer; these three decide what the answer
    /// is of.
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
    ) -> String {
        let all = counter.count(&Content::text(read.every.clone()));

        if read.total == 0 {
            // note: an empty log has two causes and they are opposite answers to the question
            // being asked. Nothing has happened yet, or everything that happened has been taken
            // out and written somewhere else - `Kernel::drain_history` is a supported thing to do
            // with a long session and it leaves the sequence counter alone, which is how this can
            // tell. Reporting the second as the first is the one mistake this tool must not make.
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

        // the true total, on every answer, whatever was asked for. It is what makes a short reply
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
            out.push_str(&format!(
                "\n`take`, `ids`, `since` or `kinds` asks for the records themselves; the last \
                 sequence number is {}.\n",
                read.last_seq,
            ));

            return out;
        }
        out.push_str(" in all.");

        // only where something decided which records *count*. A call carrying `take` alone has
        // matched everything, and "15 match" against a total of 15 is a sentence that says
        // nothing twice
        if self.narrowed() {
            let matched = counter.count(&Content::text(read.matched.concat()));
            out.push_str(&format!(
                " {} match {}, ~{} tokens.",
                thousands(read.hits),
                self.said(),
                thousands(matched),
            ));

            if read.hits == 0 {
                // a real zero, arriving beside a total that is not zero, which is the shape that
                // stops it reading as "nothing happened". The kinds go with it because a filter
                // that matched nothing is usually one spelled for a session other than this one
                out.push_str(" Nothing matched; these are the kinds this session holds:\n");
                out.push_str(&histogram(read));
                // and here most of all, because an `ids` filter that matched nothing is the exact
                // shape an inherited item makes, and "nothing matched" is the least useful way to
                // say so
                out.push_str(&self.inherited(kernel, read));

                return out;
            }
        }

        // `take` counts from the end, because a log is read from the end - but the records stay
        // in the order they happened, which is the order everything else here reports them in
        let shown = match self.take {
            Some(take) => take.min(read.matched.len()),
            None => read.matched.len(),
        };
        let beyond = read.matched.len() - shown;
        match beyond {
            0 => out.push_str(&format!(" Showing {}.\n", thousands(shown))),
            // said as a figure rather than implied by the count, because the thing a shortened
            // log has to say is how much of it is not here - and in the word that is true of
            // them, which is "older" when nothing narrowed what counts
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
        out.push_str(&self.inherited(kernel, read));
        out.push('\n');
        out.push_str(&read.matched[beyond..].concat());

        out
    }

    /// What an `ids` filter found no beginning for, which is usually what it was really asking.
    ///
    /// note: an *absence*, and the one answer a list of matching records cannot give. A session
    /// resumed under a second model that asks the log where an inherited item came from gets
    /// `model.requested` rows naming that item - every one of them true, because the item has been
    /// in every request since - and can read them as proof it wrote the item itself. What decides
    /// the question is the record that is not there.
    fn inherited(&self, kernel: &Kernel, read: &Read) -> String {
        let unborn: Vec<&ContextId> = self
            .ids
            .iter()
            .filter(|id| !read.added.contains(id))
            .collect();
        if unborn.is_empty() {
            return String::new();
        }

        let (has, they, them) = match unborn.len() {
            1 => ("has", "it was", "it"),
            _ => ("have", "they were", "them"),
        };

        format!(
            "\n{} {has} no `context.added` here: {they} already in the context before this log \
             begins, so nothing in it says where {them} came from or who wrote {them}. {}{}\n",
            numbered(&unborn),
            match read.first_seq > 1 {
                // the records that would have said are gone rather than never written, and which
                // of the two it is changes the answer completely
                true => format!(
                    "This log starts at record {}, so earlier ones were drained and may have said.",
                    read.first_seq
                ),
                false => "A session resumed from a snapshot starts that way.".to_owned(),
            },
            if_offered(kernel, "setup", || {
                " `setup` with `model` says whether this one did.".to_owned()
            }),
        )
    }
}

/// A count, or what was passed where one belonged.
///
/// note: a numeric string is taken as the number, which costs nothing and saves a turn. A word is
/// not, because a word here is a mistake worth reporting rather than one worth guessing at - and
/// the wrong answer to give is an empty result, which reads as an empty log.
fn counted(value: &serde_json::Value, name: &str) -> Result<u64, String> {
    if let Some(n) = value.as_u64() {
        return Ok(n);
    }
    if let Some(n) = value.as_str().and_then(|s| s.trim().parse::<u64>().ok()) {
        return Ok(n);
    }

    Err(format!(
        "`{name}` is a whole number and this one is `{value}`. Nothing was read, rather than \
         nothing being found: an empty answer here would have read as an empty log."
    ))
}

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
/// is a program in which the two of them can be shown the same session and disagree about it.
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
/// note: the events that name an item are the ones a question about an item is asked of, and they
/// are the reason `ids` is worth having at all: "what happened to 12?" is a question the context
/// cannot answer, because the context only holds what 12 says now.
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
            let named = |moved: &[nachalnik::Removed]| moved.iter().any(|one| one.id == id);
            named(&report.removed)
                || named(&report.elided)
                || named(&report.refused)
                || report.summary.as_ref().is_some_and(|one| one.id == id)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema and the permission subjects are one vocabulary.
    ///
    /// note: there is one table, so an argument the schema offers that no operation reads cannot
    /// happen - it is the same `Vec<Op>`. What can still
    /// drift is this: a tool declaring a subject it offers no way to reach, or offering an
    /// operation the policy was never told about, which is a call that cannot be refused by name.
    #[test]
    fn the_schema_and_the_subjects_are_one_vocabulary() {
        let tool = Log::new(Reach(std::sync::Weak::new()), Limits::default());
        let spec = tool.spec();

        let offered: Vec<String> = crate::tools::ops::offered(&spec.schema)
            .into_iter()
            .map(|action| domains::log(action).to_string())
            .collect();
        let declared: Vec<String> = spec.capabilities.iter().map(ToString::to_string).collect();

        assert_eq!(offered, declared, "one list of operations, in one order");
        assert_eq!(offered, ["log:read"]);
    }
}
