//! The tool that reads the context: what is being carried, what it costs, what the next request
//! holds, and what the answer to it would be.
//!
//! note: named for what it reads rather than for what it does. `introspect` was a name for the
//! whole family and this tool is one of them - anything else that reads a session from the inside
//! is introspection too, and would have had to be called something that did not say so. The id is
//! the noun now, which leaves the family its word and gives each tool the thing it is about.
//!
//! note: six actions, none of which changes anything, which is why they are one tool and why the
//! capability they declare is its own. `draft` and `fork` do ask the model, so this is not free -
//! only harmless. The tool that changes things is next door in `amend`, and the two are separately
//! grantable on purpose.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use nachalnik::{
    Block, BoxError, Capability, Config, ContextId, ContextItem, ContextKind, ContextState, Delta,
    Event, Kernel, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use serde_json::json;

use crate::{app::text::thousands, tools::Limits};

use super::{Pinned, Reach, action, ids, protected, unknown};

/// How long a fork may think before this looks up to see whether somebody has pressed escape.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// Reads the context, the budget, the request about to be sent, and the answer that would follow.
///
/// note: none of the six actions changes anything, which is why they are together and why the
/// capability they declare is its own. `draft` and `fork` do spend tokens - they ask the model -
/// so this is not free, only harmless.
pub struct Context {
    reach: Reach,
    pinned: Pinned,
    limits: Limits,
}

impl Context {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, pinned: Pinned, limits: Limits) -> Self {
        Self {
            reach,
            pinned,
            limits,
        }
    }
}

#[async_trait]
impl Tool for Context {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new(
            "context",
            "reads your own state, so you can check it before you act on it. `look` lists every \
             item in your context - what it is, what it costs, whether it is going into the next \
             request and why not if it is not - and with `ids` reads any of them back, block by \
             block, including what you were thinking when you produced them. A long one comes \
             back as its start and its end; `whole` if you need all of it anyway. `budget` is \
             what the next request costs against what there is, what the last one really cost, and \
             which items are the expensive ones - read it before deciding what to give up. \
             `request` shows the request you are about to send, message by message, what it \
             repaired, and what was left out and by which rule: a state you set, which you can \
             undo, or the projector, which you cannot. `draft` answers the conversation on a \
             throwaway copy and shows you what you would say *before* you say it, so you can \
             check your answer against your context and fix either. `fork` puts a question to a \
             copy of yourself on a copy of your context, optionally with some items left out - \
             for weighing an approach, or asking whether a piece of context is what is leading \
             you astray. A fork has no tools: it can think, not act. `search` finds text \
             anywhere in your context - archived items included, which `look` can only read by \
             copying them in - and says how many lines match and what they would cost before it \
             shows you one. Those are `action`s of this tool, not tools; `amend` is the tool that \
             changes any of this.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["look", "budget", "request", "draft", "fork", "search"],
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "look: read these items in full instead of listing all of \
                                    them; search: look only in these",
                },
                "text": {
                    "type": "string",
                    "description": "search: what to look for, case ignored",
                },
                // note: what leaving it out does is in the tool's own description - the count
                // and the price first - and saying it twice cost twelve tokens on every request
                "take": {
                    "type": "integer",
                    "description": "search: show this many of the matching lines",
                },
                // note: declared, because the tool reads it, the description tells the model to
                // use it, and `look`'s own last line and the marker in a sampled item both end by
                // telling it to ask for the `whole` of one. An argument named in three places and
                // absent from the schema is one a model following the schema cannot pass, and one
                // an endpoint validating against the schema will refuse outright
                "whole": {
                    "type": "boolean",
                    "description": "look: read the named items entire, rather than as a start and \
                                    an end. It costs what carrying them costs",
                },
                "question": {
                    "type": "string",
                    "description": "fork: what to ask the copy",
                },
                "without": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "fork: item ids the copy does not get to see",
                },
            },
            "required": ["action"],
        }))
        .with_capabilities([Capability::Custom("context".into())]);

        self.limits.apply(spec)
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        match action(&call.args)? {
            "look" => Ok(ToolOutput::new(look(
                &kernel,
                &ids(&call.args, "ids"),
                call.args["whole"].as_bool().unwrap_or(false),
            ))),
            "budget" => Ok(ToolOutput::new(budget(&kernel, &self.pinned.lock()))),
            "request" => Ok(ToolOutput::new(request(&kernel))),
            "draft" => branch(&kernel, None, &[], &output).await,
            "fork" => {
                let Some(question) = call.args["question"].as_str() else {
                    return Ok(ToolOutput::error(
                        "`fork` needs a `question` to put to the copy; `draft` is the one that \
                         just carries on the conversation",
                    ));
                };
                branch(
                    &kernel,
                    Some(question),
                    &ids(&call.args, "without"),
                    &output,
                )
                .await
            }
            "search" => {
                let Some(text) = call.args["text"].as_str().filter(|t| !t.is_empty()) else {
                    return Ok(ToolOutput::error(
                        "`search` needs the `text` to look for; `look` is the one that lists \
                         everything",
                    ));
                };
                let take = match taken(&call.args["take"]) {
                    Ok(take) => take,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                Ok(ToolOutput::new(search(
                    &kernel,
                    text,
                    &ids(&call.args, "ids"),
                    take,
                )))
            }
            other => Ok(ToolOutput::error(unknown(
                other,
                &["look", "budget", "request", "draft", "fork", "search"],
            ))),
        }
    }
}

/// How many matching lines a `search` was asked for, or what is wrong with the way it asked.
///
/// note: `log` has held its own `take` to this since it was written and `search` read it with a
/// bare `as_u64`, so the same word meant two things a tool apart. `take: 0` fell through to
/// `0.min(len)` and printed `the first 0; 1 more match and are not here:` - a heading with a
/// colon and nothing under it, which is a malformed answer rather than a wrong one. `take: -3`
/// and `take: "3"` were `None`, which is the summary, which is what leaving `take` out does: the
/// model asked for lines, was given a count, and nothing said its argument had not been read.
/// That is the failure `log` names in so many words one file over.
fn taken(value: &serde_json::Value) -> Result<Option<usize>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if let Some(take) = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
    {
        return match take {
            // not an error, because it is a coherent thing to have asked for and the tool has an
            // answer to it already: the count and the price, which is what a call with no `take`
            // gets. Refusing it would spend a turn on a call that meant something
            0 => Ok(None),
            take => Ok(Some(take as usize)),
        };
    }

    Err(format!(
        "`take` is a whole number of lines and this one is `{value}`. Nothing was read, rather \
         than nothing being found: leave it out for the count and the price, which is what \
         `take: 0` asks for too."
    ))
}

/// The context, item by item, or the whole of the named ones.
fn look(kernel: &Kernel, ids: &[ContextId], whole: bool) -> String {
    let items = kernel.items();
    if !ids.is_empty() {
        return ids
            .iter()
            .map(|id| full(&items, *id, whole))
            .collect::<Vec<_>>()
            .join("\n");
    }

    let budget = kernel.budget();
    // note: the undo depth is reported and named as somebody else's on purpose. It is the stack
    // behind the `u` key in the terminal, it holds everything that has ever happened to this
    // context, and `amend undo` does not touch it - a figure that big, sitting unlabelled next to
    // a tool called `undo`, would be an invitation to try to walk back the person's work
    let (withheld, theirs) =
        kernel.with_context(|context| (context.tokens_withheld(), context.undo_len()));

    let mut out = inherited(kernel, &items);
    out.push_str(&format!(
        "{} items · {} of them go into the next request\n\
         ~{} tokens going{}, ~{} withheld\n\
         {} change(s) in the person's own undo stack, which is theirs; `amend undo` walks back \
         what you did\n\n\
         {:>4}  {:<10}  {:<18}  {:>8}  what it is\n",
        items.len(),
        items.iter().filter(|item| item.is_projected()).count(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(
                " of {} ({}%)",
                thousands(limit),
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize
            ))
            .unwrap_or_default(),
        thousands(withheld),
        theirs,
        "id",
        "state",
        "kind",
        "tokens",
    ));

    for item in &items {
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(item.tokens),
            row(item),
        ));
    }

    out.push_str(
        "\n`look` with `ids` reads any of these back, including the reasoning recorded on an \
         assistant turn; a long one arrives as its start and its end unless you ask for the \
         `whole` of it.\n",
    );

    out
}

/// Which of these items this session did not produce, said before the listing rather than after.
///
/// note: three live runs bought this line. A session resumed under a *second* model was asked
/// whether it had written an inherited turn. Every time, it reached for `look` - and `look` had
/// nothing to say, because an item restored from a snapshot is a perfectly ordinary item with no
/// field that marks it. Every time, the model read the turn, recognised its own voice in it, and
/// confabulated a first-person account of writing a sentence another model wrote.
///
/// note: `setup` with `model` has said this since it existed and none of those runs called it. A
/// fact that is only reachable by a tool nobody reaches for is a fact the program does not really
/// have, so it goes where the question is actually asked. It is a count of items in a listing that
/// already counts items - not a warning, and it says nothing about what the turns are worth.
///
/// note: conditioned on `session.resumed` being in the log rather than on the absence of a
/// `context.added` alone, because `Kernel::drain_history` also takes those away. Absent that
/// record, an item with no beginning here means the log was shortened, which is a different fact
/// and would be a false one to report as this.
fn inherited(kernel: &Kernel, items: &[Arc<ContextItem>]) -> String {
    let (resumed, added) = kernel.with_history(|session| {
        let mut resumed = false;
        let mut added = BTreeSet::new();
        for record in session.records() {
            match &record.event {
                Event::SessionResumed { .. } => resumed = true,
                Event::ContextAdded { id, .. } => {
                    added.insert(*id);
                }
                _ => {}
            }
        }

        (resumed, added)
    });
    if !resumed {
        return String::new();
    }

    let carried: Vec<String> = items
        .iter()
        .filter(|item| !added.contains(&item.id))
        .map(|item| item.id.0.to_string())
        .collect();
    if carried.is_empty() {
        return String::new();
    }

    format!(
        "this session was resumed from a snapshot, and {} of the items below were already in it: \
         {}. They were not produced here. A turn among them reads in the first person and was \
         written by whatever model that session ran, which nothing in the turn records - `setup` \
         with `model` says what this one is.\n\n",
        carried.len(),
        carried.join(", "),
    )
}

/// One item's row: its label, then whatever else is worth knowing on one line.
fn row(item: &ContextItem) -> String {
    let glimpsed = glimpse(&item.content.to_text());
    let mut said = match glimpsed.is_empty() {
        true => item.label.clone(),
        false => format!("{}: {glimpsed}", item.label),
    };
    if matches!(item.kind, ContextKind::AssistantMessage { .. }) {
        // `calls()` and `thinking()` rather than the kind's own slots: a turn a provider recorded
        // in the order it was produced keeps both inside its content, and a row that read the
        // slots would report a reasoning model as having thought nothing and asked for nothing
        let calls = item.calls().count();
        if calls != 0 {
            said.push_str(&format!(" [{calls} call(s)]"));
        }
        if item.thinking().next().is_some() {
            said.push_str(" [+reasoning]");
        }
        if let Some(blocks) = item.content.as_blocks() {
            said.push_str(&format!(" [{} ordered block(s)]", blocks.len()));
        }
    }
    if let Some(note) = &item.note {
        said.push_str(&format!(" · {note}"));
    }

    said
}

/// The whole of one item, or the fact that there is no such item.
/// How much of an item's content `look` shows either side of the gap before it is asked for the
/// whole thing.
///
/// note: reading an item copies that item into the context. So asking to see a 9,000-token tool
/// result in order to decide whether to keep it costs very nearly what keeping it costs - a live
/// session did exactly that, twice, and finished an honest clean-up 7,688 tokens heavier than it
/// started. A head and a tail is enough to tell build noise from something worth keeping, and the
/// whole thing is still one argument away for the times it is really wanted.
const SAMPLE: usize = 1_500;

/// An item's content: whole if it is small or if it was asked for, a head and a tail otherwise.
fn sampled(text: &str, whole: bool) -> String {
    if whole || text.len() <= SAMPLE * 2 {
        return text.to_owned();
    }

    // on a character boundary, so that a cut through a multi-byte character does not panic
    let mut head = SAMPLE;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - SAMPLE;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }

    format!(
        "{}\n[... {} bytes not shown. Asking for an item copies it into your context, so reading \
         all of this costs about what carrying it costs; `whole: true` if you need it anyway ...]\n{}",
        &text[..head],
        thousands(tail - head),
        &text[tail..],
    )
}

fn full(items: &[Arc<ContextItem>], id: ContextId, whole: bool) -> String {
    let Some(item) = items.iter().find(|item| item.id == id) else {
        return format!("[{id}] there is no such item\n");
    };

    let mut out = format!(
        "[{}] {} · {} · from {} · {} · {} tokens\n",
        item.id,
        item.label,
        item.kind.name(),
        item.source,
        item.state,
        thousands(item.tokens),
    );
    if let Some(because) = &item.included_because {
        out.push_str(&format!("  it is here because: {because}\n"));
    }
    if let Some(note) = &item.note {
        out.push_str(&format!("  it is {} because: {note}\n", item.state));
    }
    if !item.meta.is_null() {
        out.push_str(&format!("  attached: {}\n", item.meta));
    }
    // a turn that was recorded as an order is read back as one, block by block. This is the
    // thing `context` exists for and the one view of it that is not available anywhere else: the
    // request the model will be sent has the same parts in the same order, but by then the
    // thinking looks like a field rather than something that happened between two calls
    if let Some(blocks) = item.content.as_blocks() {
        out.push_str(&format!("  --- {} block(s), in order ---\n", blocks.len()));
        for (at, block) in blocks.iter().enumerate() {
            let said = match block {
                Block::Call(call) => format!("{}({})", call.tool, call.args),
                _ => block
                    .part()
                    .map(|part| part.content.to_text().into_owned())
                    .unwrap_or_default(),
            };
            let signed = match block.extra().is_null() {
                true => "",
                false => " (signed)",
            };
            out.push_str(&format!("  [{at}] {}{signed}: {said}\n", block.name()));
        }

        return out;
    }

    // the reasoning first, because on the turn that carries it it is the part that explains the
    // rest, and because it is the one thing here the model cannot see in the request itself
    if let Some(reasoning) = item.reasoning() {
        out.push_str(&format!("  --- reasoning ---\n{}\n", reasoning.to_text()));
    }
    out.push_str(&format!(
        "  --- content ---\n{}\n",
        sampled(&item.content.to_text(), whole)
    ));

    out
}

/// Where a piece of text is in the context, and what reading it would cost - the count first.
///
/// note: the thing `look` cannot do. An archived item is kept in full and never sent, and the only
/// way to see inside one was to read it back, which copies it into the context - so a session
/// carrying eleven megabytes it had put away could not look at any of it without undoing the
/// saving. That made the archive write-only from the model's side, which is not what "nothing is
/// destroyed" is supposed to mean.
///
/// note: so the rule is `log`'s rule, for the same reason: the count and its price first, the
/// lines on request, and never the item. A search that answered with what it found would be a
/// second way to pay for an item without meaning to, which is the thing this action exists to
/// undo.
///
/// note: case is ignored. A model that searched for `landlock` and was told there are no matches
/// in a context full of `Landlock` has been told something false about itself, and the failure is
/// silent - which is the one shape of wrong answer a search must not have.
fn search(kernel: &Kernel, text: &str, only: &[ContextId], take: Option<usize>) -> String {
    let needle = text.to_lowercase();
    let items = kernel.items();

    // (item, matching lines), in context order
    let mut found: Vec<(&Arc<ContextItem>, Vec<String>)> = Vec::new();
    // counted rather than assumed from `only`, because the two differ exactly where it matters:
    // an id naming nothing is looked at zero times, and reporting it as one looked at is this
    // tool saying an item exists and does not contain the text. `look` answers `[99] there is no
    // such item`; a search that quietly counted it would be the less honest of two siblings
    let mut read = 0;
    for item in &items {
        if !only.is_empty() && !only.contains(&item.id) {
            continue;
        }
        read += 1;
        let hay = item.content.to_text();
        let lines: Vec<String> = hay
            .lines()
            .filter(|line| line.to_lowercase().contains(&needle))
            .map(around(&needle))
            .collect();
        if !lines.is_empty() {
            found.push((item, lines));
        }
    }

    let matches: usize = found.iter().map(|(_, lines)| lines.len()).sum();
    let where_ = match only.is_empty() {
        true => String::new(),
        false => format!(
            " (looking only in {})",
            only.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    // the ids that name nothing, said out loud rather than passed over. A search narrowed to an
    // item that is not there has not searched anything, and "nothing matched" is the one reading
    // of that which leaves the model believing the item exists
    let missing: Vec<String> = only
        .iter()
        .filter(|id| !items.iter().any(|item| item.id == **id))
        .map(|id| id.to_string())
        .collect();
    let unknown = match missing.as_slice() {
        [] => String::new(),
        [one] => format!(" There is no item {one}, so it was not among them."),
        _ => format!(
            " There are no items {}, so they were not among them.",
            missing.join(", ")
        ),
    };
    if matches == 0 {
        return format!(
            "no line of your context says `{text}`{where_}. Case was ignored, archived and \
             excluded items were searched, and {read} item(s) were looked at.{unknown}\n",
        );
    }

    // rendered before it is priced, because the price is of this and not of an estimate of it
    let rendered: Vec<String> = found
        .iter()
        .flat_map(|(item, lines)| {
            lines
                .iter()
                .map(move |line| format!("  [{}] {}: {line}\n", item.id, item.label))
        })
        .collect();
    let counter = kernel.counter();
    let cost = counter.count(&nachalnik::Content::text(rendered.concat()));

    // and on an answer that did find something, for the same reason: an id that named nothing is
    // a fact about the call, and a result that only reports what it found lets it pass unnoticed
    let mut out = format!(
        "{} line(s) say `{text}`{where_}, ~{} tokens if you take them all, in {} item(s):{}\n",
        thousands(matches),
        thousands(cost),
        found.len(),
        match unknown.is_empty() {
            true => String::new(),
            false => format!("{unknown}\n"),
        },
    );
    for (item, lines) in &found {
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>4} line(s)  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            lines.len(),
            glimpse(&item.label),
        ));
    }

    let Some(take) = take else {
        out.push_str(
            "\n`take` shows that many of the lines. None of this puts an item into your request: \
             an archived one is still archived, and searching it changed nothing.\n",
        );

        return out;
    };

    let shown = take.min(rendered.len());
    match rendered.len() - shown {
        0 => out.push_str(&format!("\nall {} of them:\n", thousands(shown))),
        more => out.push_str(&format!(
            "\nthe first {}; {} more match and are not here:\n",
            thousands(shown),
            thousands(more)
        )),
    }
    out.push_str(&rendered[..shown].concat());

    out
}

/// A matching line, trimmed to the part with the match in it.
///
/// note: around the match rather than from the start of the line, because the line a search finds
/// something in is as likely as not a minified one or a log line, and the first eighty characters
/// of those is the part nobody asked about.
fn around(needle: &str) -> impl Fn(&str) -> String + '_ {
    /// How much of a matching line comes back.
    const WINDOW: usize = 100;
    /// How much of it sits before the match, when there is room.
    const LEAD: usize = 30;

    move |line: &str| {
        let line = line.trim();
        if line.chars().count() <= WINDOW {
            return line.to_owned();
        }
        let at = line
            .to_lowercase()
            .find(needle)
            .map(|byte| line[..byte].chars().count())
            .unwrap_or_default();
        let from = at.saturating_sub(LEAD);
        let said: String = line.chars().skip(from).take(WINDOW).collect();

        format!(
            "{}{said}{}",
            if from > 0 { "…" } else { "" },
            if from + WINDOW < line.chars().count() {
                "…"
            } else {
                ""
            },
        )
    }
}

/// What the next request costs, what there is, and what giving something up would buy.
///
/// note: the action a compaction decision is actually made from, which `look` was being asked to
/// be and is not: a table of every item in insertion order answers "what am I carrying?" and not
/// "what is it costing me and what should go?". The expensive items are the ones that decide that,
/// so they are sorted and totalled here rather than left to be found by reading.
///
/// note: it reports the estimate beside what the provider charged for the last request, and the
/// correction the counter has worked out from the difference, because the estimate is made without
/// the model's tokenizer and is usually low. An agent budgeting against a number nobody has
/// checked is the thing this crate exists not to do quietly.
fn budget(kernel: &Kernel, mine: &BTreeSet<ContextId>) -> String {
    let budget = kernel.budget();
    let withheld = kernel.with_context(|context| context.tokens_withheld());

    let room = match budget.limit {
        Some(limit) => format!(
            " of {} ({}% full, ~{} left)",
            thousands(limit),
            (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
            thousands(limit.saturating_sub(budget.used())),
        ),
        None => ", against a limit this provider does not report".to_owned(),
    };

    let mut out = format!(
        "the next request is ~{} tokens{room}\n  {} in the context, {} in the tool definitions\n\
         ~{} tokens are being held back - excluded, archived, or elided to a marker\n",
        thousands(budget.used()),
        thousands(budget.context_tokens),
        thousands(budget.tool_tokens),
        thousands(withheld),
    );

    match budget.reported {
        Some(usage) => out.push_str(&format!(
            "the last request really cost {} in / {}, as the provider counted it\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::app::text::charged(&usage),
        )),
        None => out.push_str(
            "nothing has been charged for yet, so the figures above are only an estimate\n",
        ),
    }
    if let Some(learned) = kernel.counter().calibration() {
        out.push_str(&match learned.observations {
            0 => "the estimate has not been checked against a real request yet; treat it as a floor\n"
                .to_owned(),
            seen => format!(
                "the estimate is corrected by x{:.2}, learned from {seen} request(s)\n",
                learned.scale
            ),
        });
    }

    // the expensive ones, biggest first: what a decision about compaction is made from.
    //
    // note: what the *projection* included, not what the states say. They are not the same list -
    // a tool result whose call is not in the request is repaired out of it by the projector, and
    // is costing nothing however active it looks. Offering it as something to save tokens by
    // eliding would be advice that buys nothing
    let going: BTreeSet<ContextId> = kernel.project().included.into_iter().collect();
    let mut costly: Vec<_> = kernel
        .items()
        .into_iter()
        .filter(|item| going.contains(&item.id) && item.state.sends_content())
        .collect();
    costly.sort_by_key(|item| std::cmp::Reverse(item.tokens));
    costly.truncate(10);

    if costly.is_empty() {
        return out;
    }

    out.push_str(&format!(
        "\nthe {} most expensive item(s) actually going into it:\n{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  what it is\n",
        costly.len(),
        "id",
        "state",
        "kind",
        "tokens",
        "if all go",
    ));
    let mut running = 0;
    for item in &costly {
        running += item.tokens;
        // saying so here saves a call that would only be refused, and the reason is the same one
        // `amend` would give: it is not the model's to move
        let whose = match protected(item, mine, None) {
            Some(_) => " · not yours",
            None => "",
        };
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}{whose}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(item.tokens),
            thousands(running),
            glimpse(&format!("{}: {}", item.label, item.content.to_text())),
        ));
    }
    out.push_str(
        "\nthe fifth column is what eliding everything down to that row would save, give or take \
         what the markers cost. Eliding leaves a marker in place, so a tool result still answers \
         the call that asked for it; excluding one takes that call down with it, and the model \
         then reads a conversation in which it never asked. `amend` does either.\n",
    );

    out
}

/// The request that would go next, as a shape rather than as its bytes.
///
/// note: a summary and not the request itself, which is the one thing this could print and must
/// not: the request *is* the context, so answering with it would double every token the agent was
/// asking about. Roles, sizes and first lines are what the question "what am I about to send?"
/// actually wants, and `/request` in the terminal has the verbatim JSON for whoever wants that.
fn request(kernel: &Kernel) -> String {
    let request = match kernel.preview_request() {
        Ok(request) => request,
        Err(e) => return format!("there is no request to preview: {e}\n"),
    };
    let projection = kernel.project();
    let budget = kernel.budget();

    let mut out = format!(
        "{} message(s), {} tool(s), ~{} tokens{}\n\n{:>4}  {:<10}  {:>8}  first line\n",
        request.messages.len(),
        request.tools.len(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        "#",
        "role",
        "bytes",
    );

    for (index, message) in request.messages.iter().enumerate() {
        let said = message
            .content
            .as_ref()
            .map(|c| c.to_text())
            .unwrap_or_default();
        out.push_str(&format!(
            "{:>4}  {:<10}  {:>8}  {}\n",
            index + 1,
            message.role.as_str(),
            thousands(said.len()),
            glimpse(&said),
        ));
    }

    // note: split by *what* dropped it, because the two halves are answered differently and one
    // list could not say which was which. An item left out by its own state is one `amend` with
    // `restore` puts straight back. An item the projector dropped is a consequence of something
    // else in the context, and restoring it does nothing whatever - the thing to move is the
    // cause. A model reading one undifferentiated list has to guess which it is looking at, and
    // the cheap guess is `restore`, which is the one that changes nothing and costs a call.
    let (by_state, by_projector): (Vec<_>, Vec<_>) =
        projection
            .skipped
            .iter()
            .partition(|left_out| match kernel.item(left_out.id) {
                Some(item) => !item.is_projected(),
                // an id the context no longer has is nothing a state change can reach either
                None => false,
            });

    if !by_state.is_empty() {
        out.push_str("\nleft out by its own state, which `amend` with `restore` puts back:\n");
        for left_out in &by_state {
            out.push_str(&format!("  [{}] {}\n", left_out.id, left_out.reason));
        }
    }
    if !by_projector.is_empty() {
        out.push_str(&format!(
            "\nleft out by the projector (`{}`), the rule that turns your context into messages. \
             Restoring these changes nothing - each is a consequence of something else, and the \
             cause is what there is to move:\n",
            kernel.projector().name(),
        ));
        for left_out in &by_projector {
            out.push_str(&format!("  [{}] {}\n", left_out.id, left_out.reason));
        }
    }
    if !projection.repairs.is_empty() {
        out.push_str("\nand what that same projector rewrote, to keep the request valid:\n");
        for repair in &projection.repairs {
            out.push_str(&format!("  {repair}\n"));
        }
    }

    out
}

/// Answers on a copy of the context, and hands back only what was said.
///
/// note: the copy is a whole second [`Kernel`] resumed from a [`Snapshot`](nachalnik::Snapshot) of
/// this one, which is why this needed nothing added to the runtime: forking a session is what
/// `snapshot` and `resume` already are, and the documentation for `resume` says as much. It gets
/// this session's provider and projector so that it is answering the same model in the same
/// dialect, and it gets **no tools**, no compactor and a limit of one request. A fork can think;
/// it cannot act, and it cannot go on thinking after it has answered once.
///
/// note: nothing the fork does reaches this session's context, and nothing it does reaches this
/// session's event log either - it has a log of its own that goes when it does. What it *is*
/// visible as is the text it streams, relayed into this tool's own [`OutputSink`], so a person
/// watching the terminal sees a fork thinking rather than a tool that has gone quiet.
async fn branch(
    kernel: &Kernel,
    question: Option<&str>,
    without: &[ContextId],
    output: &OutputSink,
) -> Result<ToolOutput, BoxError> {
    let Some(provider) = kernel.provider() else {
        return Ok(ToolOutput::error("there is no provider to ask"));
    };

    let mut snapshot = kernel.snapshot();
    let mut left_out = Vec::new();
    for item in &mut snapshot.items {
        if without.contains(&item.id) {
            // excluded rather than deleted, so the fork's own account of itself can still name
            // the item by the number this session knows it by
            item.state = ContextState::Excluded;
            item.note = Some("left out of this fork".into());
            left_out.push(item.id);
        }
    }
    let fork = Kernel::resume(
        Config {
            session_name: Some(format!("{}#fork", kernel.session_name())),
            // it answers once and is thrown away: there is nothing for an undo stack to be for,
            // and nothing after the first request for a second one to build on
            context_undo_depth: 0,
            max_requests_per_turn: Some(1),
            ..Config::default()
        },
        snapshot,
    );
    fork.set_provider(provider);
    fork.set_projector(kernel.projector());
    // note: said out loud, because the copy cannot work it out. It inherits a conversation full of
    // tool calls and their results and no tool definitions at all, and a model reading that asks
    // for a tool - which nothing here can run, so the answer comes back as a call and no words.
    // Measured against a real model that is not a corner case, it is what happens every time
    fork.push(
        ContextItem::system(
            "You are a copy of this session, made to think and not to act. You have no tools \
             here, and nothing you ask for can be run: answer in words, from what is already in \
             front of you.",
        )
        .pinned(),
    );
    if let Some(question) = question {
        fork.push(ContextItem::user(question).because("put to a fork of this context"));
    }
    // what the fork will actually read, rather than what it was handed: the projector still has
    // to repair the call this very tool is answering out of the copy, and a count taken before it
    // did would be one the fork never saw
    let items = fork.project().included.len();

    let mut events = fork.subscribe();
    let sink = output.clone();
    let relay = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let Event::ModelDelta {
                delta: Delta::Text(text),
            } = event
            {
                sink.push(text);
            }
        }
    });

    // the same heartbeat the `shell` tool runs on, and for the same reason: the fork is a whole
    // request that could take a minute, and escape has to reach it
    let outcome = {
        let turn = fork.turn();
        tokio::pin!(turn);
        loop {
            tokio::select! {
                outcome = &mut turn => break outcome,
                _ = tokio::time::sleep(HEARTBEAT) => {
                    if output.is_interrupted() {
                        fork.interrupt();
                    }
                }
            }
        }
    };
    relay.abort();

    if let Err(e) = outcome {
        return Ok(ToolOutput::error(format!("the fork got no answer: {e}")));
    }
    let Some(response) = fork.last_response() else {
        return Ok(ToolOutput::error("the fork got no answer at all"));
    };

    let mut out = match question {
        Some(question) => format!("a copy of you, asked `{question}`, on {items} of your items"),
        None => format!("what you would say if you answered now, drafted on {items} of your items"),
    };
    // note: said either way, because "on 9 of your items" cannot be read as "on all of them" and a
    // fork's whole worth is which items the copy did not get. A live session asked a copy what it
    // would conclude "without knowing my earlier statement", passed no `without` at all, and
    // reported the matching answer as an ablation - it had asked the copy to pretend rather than
    // taken the item away, and nothing in the reply distinguished the two. The copy really did see
    // everything, so the reply says so.
    match left_out.is_empty() {
        true => out.push_str(
            ". The copy saw all of them: nothing was left out, so this is the same context \
             answering again rather than a test of what any of it was doing. `without` takes items \
             away from the copy, and a question that asks it to disregard something is not the \
             same thing - it is still reading it.",
        ),
        false => {
            let numbers: Vec<String> = left_out.iter().map(|id| id.to_string()).collect();
            out.push_str(&format!(
                ", without {}, which the copy could not read at all.",
                numbers.join(", ")
            ));
        }
    }
    out.push_str(
        " None of this is in your context and nobody has read it; it is yours to use or drop.\n",
    );
    if let Some(usage) = response.usage {
        out.push_str(&format!(
            "it cost {} in / {}.\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::app::text::charged(&usage),
        ));
    }
    let said = response
        .content
        .as_ref()
        .map(|content| content.to_text())
        .unwrap_or_default();
    match said.trim().is_empty() {
        // it asked for a tool instead of answering, and there is nothing in a fork to run one.
        // Saying so beats handing back a blank: a caller reading an empty draft has no way to
        // tell a copy that had nothing to say from one that tried to do something
        true => out.push_str(&format!(
            "\n--- it said nothing ({:?}) ---\nit asked for {} instead of answering, and a fork \
             has no tools; ask it something it can answer from what it already has.\n",
            response.stop,
            match response.calls().next() {
                Some(call) => format!("`{}`", call.tool),
                None => "nothing at all".to_owned(),
            },
        )),
        false => out.push_str(&format!(
            "\n--- what it said ({:?}) ---\n{said}\n",
            response.stop
        )),
    }

    // note: the answer before the thinking, which is the opposite of the order it was produced
    // in and the right way round for the one thing that happens to this output: an output limit
    // cuts from the end. On a reasoning model the thinking is the bulk of a fork - measured on one
    // real fork, 68% of 34,287 bytes against the answer's 30% - so with the thinking first the
    // limit ate the answer and left the deliberation about how to answer. That session lost
    // exactly the three paragraphs it had asked for. Nothing about this order is a claim about
    // what the copy did; the two sections are labelled and the reasoning says it is reasoning
    if let Some(reasoning) = &response.reasoning {
        out.push_str(&format!(
            "\n--- its reasoning, which it produced before the answer above ---\n{}\n",
            reasoning.to_text()
        ));
    }

    Ok(ToolOutput::new(out))
}

/// The first line of something, shortened to fit a column.
fn glimpse(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    match first.chars().count() > GLIMPSE {
        true => format!("{}…", first.chars().take(GLIMPSE - 1).collect::<String>()),
        false => first.to_owned(),
    }
}
