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
    BoxError, Capability, Content, ContextId, Event, OutputSink, Record, Tool, ToolCall,
    ToolOutput, ToolSpec, async_trait,
};
use serde_json::json;

use crate::{
    app::text::{thousands, trace_line},
    tools::Limits,
};

use super::Reach;

/// How wide the event-name column is, which is the longest name plus a space.
const NAMES: usize = 20;

/// Reads the session log: what happened, of what kinds, and what taking it would cost.
pub struct Log {
    reach: Reach,
    limits: Limits,
}

impl Log {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, limits: Limits) -> Self {
        Self { reach, limits }
    }
}

#[async_trait]
impl Tool for Log {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new(
            "log",
            "reads the session's own record: an append-only log of what happened, kept beside \
             your context and not part of it. Every item added, replaced, elided, undone or \
             compacted; every permission asked for and answered; every tool that appeared or went \
             away. Called bare it says only how many records there are, of what kinds, and what \
             they would cost you - ask again with `take`, `ids`, `since` or `kinds` to get them. \
             Every answer opens with the true total, so what you are not being shown is never a \
             surprise. You cannot write to it. A replacement is the one entry that keeps what the \
             item said before, because once it falls out of the undo window that text exists \
             nowhere else.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "take": {
                    "type": "integer",
                    "description": "the most recent N of whatever matched; the header still says \
                                    how many there are",
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "only the records naming these context items",
                },
                // note: `0` said out loud, because it is not guessable and the guess is costly.
                // This is exclusive - `since: 1` means *after* record 1 - and a live session
                // reaching for "everything" wrote `since: 1`, which in a resumed session drops
                // exactly one record: `session.resumed`, which is always the first. It then
                // answered the question that record was the answer to, wrongly.
                "since": {
                    "type": "integer",
                    "description": "only the records after this sequence number, which is the \
                                    first column; `0` is all of them",
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "only these kinds, spelled as the summary spells them",
                },
                // note: the same word `context: look` uses for the same trade, because it is the
                // same trade. Without it a replacement is shown as its first line; with it the
                // whole of what the item said arrives in your context and costs what it costs
                "whole": {
                    "type": "boolean",
                    "description": "print a replaced item's old text entire rather than its first \
                                    line. It costs what that text costs",
                },
            },
        }))
        .with_capabilities([Capability::Custom("log".into())]);

        self.limits.apply(spec)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        let query = match Query::read(&call.args) {
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
            let mut matched = String::new();
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
                    matched.push_str(&line(record, query.whole));
                }
            }

            Read {
                total: session.len(),
                last_seq: session.last_seq(),
                kinds,
                every,
                matched,
                hits,
                added,
            }
        });

        Ok(ToolOutput::new(query.report(&read, &*counter)))
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
    /// The ones that matched, rendered.
    matched: String,
    /// How many those were.
    hits: usize,
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
/// back as something to correct rather than as an empty result. That distinction is the whole of
/// the care this tool needs: an empty result reads as *nothing happened*, and for a session log
/// that is the one answer that can be wrong in a way nobody catches.
#[derive(Default)]
struct Query {
    take: Option<usize>,
    ids: Vec<ContextId>,
    since: Option<u64>,
    kinds: Vec<String>,
    whole: bool,
}

/// The arguments this tool takes; anything else is a mistake worth reporting.
const TAKES: [&str; 5] = ["take", "ids", "since", "kinds", "whole"];

impl Query {
    /// Reads one, or says what is wrong with the arguments.
    fn read(args: &serde_json::Value) -> Result<Self, String> {
        // note: an argument nobody reads is the same failure as a filter nobody can parse, one
        // step earlier: the call comes back looking like a bare call, which is a real answer, so
        // nothing says it did not do what was asked. A live session called `log {action: "look"}`,
        // got the summary, and read it as the answer to a question it had not asked - then cited
        // it. The sibling tools all take an `action`, so reaching for one here is the obvious
        // mistake to make and gets a sentence of its own.
        if let Some(given) = args.as_object() {
            let unknown: Vec<&str> = given
                .keys()
                .map(String::as_str)
                .filter(|key| !TAKES.contains(key))
                .collect();
            if !unknown.is_empty() {
                let mut why = format!(
                    "`log` does not take {}. It takes {}, and nothing was read - a call that \
                     ignored an argument would have come back looking like a bare call.",
                    unknown
                        .iter()
                        .map(|key| format!("`{key}`"))
                        .collect::<Vec<_>>()
                        .join(" or "),
                    TAKES
                        .iter()
                        .map(|key| format!("`{key}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                if unknown.contains(&"action") {
                    why.push_str(
                        " There are no actions here: `context`, `setup` and `amend` have them and \
                         this does not. A bare call is the summary.",
                    );
                }

                return Err(why);
            }
        }

        let mut query = Self {
            whole: args["whole"].as_bool().unwrap_or(false),
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
        if !args["ids"].is_null() {
            let Some(ids) = args["ids"].as_array() else {
                return Err(
                    "`ids` is a list of context item numbers, as `ids: [12, 13]`".to_owned(),
                );
            };
            // note: an *empty* array constrains nothing and is how a model spells "no id filter"
            // while passing every argument the schema lists - which a live one did, and was
            // refused, and spent a turn on it. An array with entries in it that are not numbers
            // is a different thing and is still a mistake worth reporting.
            query.ids = ids
                .iter()
                .filter_map(|id| id.as_u64())
                .map(ContextId)
                .collect();
            if query.ids.is_empty() && !ids.is_empty() {
                return Err(format!(
                    "`ids` is a list of item numbers and none of {} is one; `context` with `look` \
                     lists what there is",
                    serde_json::Value::Array(ids.clone()),
                ));
            }
        }
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
    /// note: the two are not the same question and the header used to answer them as though they
    /// were, so a call carrying only `take` reported "15 match , ~205 tokens" - a match count that
    /// is really the total, and a filter description that was empty because there was no filter.
    /// `take` shortens an answer; these three decide what the answer is of.
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
    fn report(&self, read: &Read, counter: &dyn nachalnik::TokenCounter) -> String {
        let all = counter.count(&Content::text(read.every.clone()));

        if read.total == 0 {
            return "nothing has been recorded yet; this session's log is empty, which is not the \
                    same as a log you have not been shown.\n"
                .to_owned();
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
            let matched = counter.count(&Content::text(read.matched.clone()));
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
                out.push_str(&self.inherited(read));

                return out;
            }
        }

        // `take` counts from the end, because a log is read from the end - but the lines stay in
        // the order they happened, which is the order everything else here reports them in
        let lines: Vec<&str> = read.matched.lines().collect();
        let shown = match self.take {
            Some(take) => take.min(lines.len()),
            None => lines.len(),
        };
        let beyond = lines.len() - shown;
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
        out.push_str(&self.inherited(read));
        out.push('\n');
        out.push_str(&lines[beyond..].join("\n"));
        out.push('\n');

        out
    }

    /// What an `ids` filter found no beginning for, which is usually what it was really asking.
    ///
    /// note: an *absence*, and the one answer a list of matching records cannot give. A live
    /// session, resumed under a second model, asked the log where an inherited item had come from.
    /// It got five `model.requested` rows naming that item - every one of them true, because the
    /// item had been in every request since - and read them as proof it had written the item
    /// itself. What decided the question was the record that was not there.
    fn inherited(&self, read: &Read) -> String {
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
             begins, so nothing in it says where {them} came from or who wrote {them}. A session \
             resumed from a snapshot starts that way, and `setup` with `model` says whether this \
             one did.\n",
            numbered(&unborn),
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
