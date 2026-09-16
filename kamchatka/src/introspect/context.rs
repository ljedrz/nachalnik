//! `context`: what this session is carrying, what it costs, and what is carried into the next
//! request.
//!
//! note: named for what it is about rather than for what it does to it. `introspect` was a name
//! for the whole family and this tool is one of them - anything else that reads a session from
//! the inside is introspection too, and would have had to be called something that did not say
//! so. The id is the noun now, which leaves the family its word and gives each tool the thing it
//! is about.
//!
//! note: one tool for reading the context and one for changing it was one tool too many, and the
//! reason it was two is worth recording as a thing that turned out to be wrong. The argument was
//! that a tool declares its capabilities once, so `context` and `amend` being one would mean
//! answering *always* to "may it look at its own items?" also answered "may it rewrite a tool
//! result?" That is a real hazard and it stopped being this tool's problem the day a subject
//! became `<domain>:<operation>` and [`Tool::needs`] let a call declare which one it is:
//! `context:look` and `context:revise` are separate questions with separate rows whether they
//! arrive under one tool's name or two. What two tools cost was the part nothing was measuring -
//! two descriptions in every request, most of each spent saying which of the two the other one
//! was.
//!
//! note: the changing half is still in `amend.rs`, which is the file it was written in and the
//! file its journal and its refusals belong to. What moved is the schema and the dispatch.

use std::{collections::BTreeSet, sync::Arc};

use nachalnik::{
    Block, BoxError, Capability, ContextId, ContextItem, ContextKind, Event, Kernel, OutputSink,
    Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use serde_json::json;

use crate::{
    app::{Going, text::thousands},
    tools::{Limits, domains, unread},
};

use super::{Amend, Pinned, Reach, action, ids, protected, unknown};

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// The four that read, in the order the schema lists them.
const READS: [&str; 4] = ["look", "budget", "request", "search"];

/// The nine that change, each named for what it leaves behind.
///
/// note: the word for the move is the word the result is reported in - an item you `archive` reads
/// back as `archived` everywhere it is listed. One level, and the same vocabulary at both ends of
/// it. They were a `prune` action with a `state` argument once, which put the word for one of them
/// over all nine.
const CHANGES: [&str; 9] = [
    "elide", "exclude", "archive", "pin", "restore", "revise", "note", "undo", "redo",
];

/// Every operation this tool has, in one list, because three things have to agree about it: the
/// `action` a model chooses from, the subject each call declares, and what a refusal names.
fn operations() -> impl Iterator<Item = &'static str> {
    READS.into_iter().chain(CHANGES)
}

/// What each of them reads, beside `action`, which they all take.
///
/// note: the list [`unread`] holds a call to. Thirteen operations share ten arguments and most of
/// them read three, so most of what this table says is what an operation does *not* take - which
/// is the half worth saying, and the half no reading of the dispatch below makes obvious. `note`
/// is the sharp case: it is one of the nine that change, the `ids` argument says it is for the
/// nine that change, and it writes a new item and has no use for an id.
const TAKES: [(&str, &[&str]); 13] = [
    ("look", &["ids", "whole"]),
    ("budget", &[]),
    ("request", &[]),
    ("search", &["ids", "text", "take"]),
    ("elide", MOVES),
    ("exclude", MOVES),
    ("archive", MOVES),
    ("pin", MOVES),
    ("restore", MOVES),
    ("revise", &["ids", "content", "reason"]),
    ("note", &["content", "label", "pin", "reason"]),
    ("undo", &["steps", "reason"]),
    ("redo", &["steps", "reason"]),
];

/// What the five that move an item take, which is one list because they are one function.
///
/// note: named rather than written out five times, and the difference is not brevity. Five
/// identical rows are five chances to disagree about one fact: `label` off `restore` alone would
/// change a real answer - a `restore` naming a label instead of `ids` would stop being told the
/// `select: "label:…"` it meant - and the test for that answer asks `elide`. One list cannot drift.
///
/// note: `label` is in here because [`Amend::moved`] reads it. Not to move anything by: to answer
/// a call that gave one instead of `ids` with the spelling it wanted. An argument a tool answers
/// about is not one it ignored, which is the only thing this table is for.
const MOVES: &[&str] = &["ids", "select", "label", "reason"];

/// Reads the context and changes it: what is in it, what it costs, and what goes into the next
/// request.
pub struct Context {
    reach: Reach,
    pinned: Pinned,
    limits: Limits,
    /// The half that changes things, which keeps the journal `undo` walks and the set of items
    /// this tool pinned itself.
    amend: Amend,
}

impl Context {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, pinned: Pinned, limits: Limits) -> Self {
        Self {
            amend: Amend::new(pinned.clone()),
            reach,
            pinned,
            limits,
        }
    }
}

#[async_trait]
impl Tool for Context {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "context",
            "your own context: what is in it, what it costs, and what you carry into the next \
             request. Four actions read it and nine change it. \
             `look` lists every item - what it is, what it costs, whether it is going into the \
             next request and why not if it is not - and with `ids` reads any of them back, block \
             by block, including what you were thinking when you produced them. `budget` is what \
             the next request costs against what there is, what the last one really cost, and \
             which items are the expensive ones: read it before deciding what to give up. \
             `request` shows the request you are about to send, message by message, what it \
             repaired, and what was left out and by which rule - a state you set, which you can \
             undo, or the projector, which you cannot. `search` finds text anywhere in your \
             context, archived items included, which `look` can only read by copying them in; it \
             says how many lines match and what they would cost before showing you one. \
             The five that move an item are named for the state they leave, which is the word \
             you will read back on it. `elide` replaces what an item says with a short marker: \
             the call it answers stays answered and stops costing what it holds, which is what to \
             reach for once a tool result has served its purpose. `exclude` takes it out of the \
             request altogether, and takes down the call that asked for it. `archive` says the \
             same and means you are done with it. `pin` protects it from being compacted away. \
             `restore` is the way \
             back from any of them. `revise` rewrites what one item says, for when you wrote \
             something down wrong. `note` writes something into your context - a plan, a \
             conclusion, a thing not to try again. Saying it in a turn is not the same: thinking \
             is not carried into later requests, a note is an item of its own that goes into \
             every one and can be pinned. `undo` and `redo` walk back through the changes *you* \
             made here. Nothing destroys anything: every item keeps its number and can be \
             restored. Everything that changes something needs a `reason`. A pinned item, a \
             system instruction and the turn you are speaking in are refused - they are not \
             yours.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": operations().collect::<Vec<_>>(),
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "for `look`: read these items in full instead of listing \
                                    all of them. For `search`: look only in these. For the nine \
                                    that change: the items to move, and exactly one for \
                                    `revise`",
                },
                // note: the forms, with the variable part written as a placeholder. It listed
                // examples - `tool:shell`, `kind:assistant_message` - and a model reading them as
                // literals rather than as instances asked to prune `tool:shell` in a session with
                // no shell. The closed sets are not spelled out here because `look` prints them
                // in its own columns, which is a shorter way to learn them than a schema is
                "select": {
                    "type": "string",
                    "description": "a class of items instead of `ids`. One of: an item \
                                    number; `all`; `all:tool_results` (or files, diagnostics, \
                                    selections, memories, instructions, system, user, model, \
                                    compaction); `kind:<kind>` or `state:<state>`, taking the \
                                    words `look` prints in those columns; `tool:<name>`, \
                                    optionally `:first` or `:latest`; `source:<name>`; \
                                    `file:<path>`; `label:<text>`. Anything else is read as a \
                                    label.",
                },
                "text": {
                    "type": "string",
                    "description": "for `search`: what to look for, case ignored",
                },
                // note: what leaving it out does is in the tool's own description - the count
                // and the price first - and saying it twice cost twelve tokens on every request
                "take": {
                    "type": "integer",
                    "description": "for `search`: show this many of the matching lines",
                },
                // note: declared, because the tool reads it, the description tells the model to
                // use it, and `look`'s own last line and the marker in a sampled item both end by
                // telling it to ask for the `whole` of one. An argument named in three places and
                // absent from the schema is one a model following the schema cannot pass, and one
                // an endpoint validating against the schema will refuse outright
                "whole": {
                    "type": "boolean",
                    "description": "for `look`: read the named items entire rather than as a \
                                    start and an end. It costs what carrying them costs",
                },
                "content": {
                    "type": "string",
                    "description": "for `revise`: what the item should say instead. For `note`: \
                                    what to write down",
                },
                // note: what it is *not* is half of this line, and it is the half a live run
                // needed. `label` reads as a key, five notes went in under one name meaning to
                // replace each other, and the tool appended every time - which the result now
                // also says when it happens. This is the same sentence one step earlier, where
                // the name is being chosen rather than regretted.
                "label": {
                    "type": "string",
                    "description": "for `note`: a short name for it, so you can find it again. \
                                    Not a key: a second note under a name is a second item, and \
                                    `revise` is what changes one you already wrote",
                },
                "pin": {
                    "type": "boolean",
                    "description": "for `note`: protect it from compaction, for a finding that \
                                    has to outlast the context it was found in",
                },
                "reason": {
                    "type": "string",
                    "description": "required by the nine that change: why, in your own words; the \
                                    person you work with reads this",
                },
                "steps": {
                    "type": "integer",
                    "description": "for `undo` and `redo`: how many of your own changes to walk; \
                                    1 by default",
                },
            },
            "required": ["action"],
        }))
        .with_capabilities(operations().map(domains::context))
    }

    /// note: `action` and nothing else, so a rule about `context:look` is about looking whichever
    /// way a call asked for it - and so that the reading half and the changing half are still
    /// separately grantable now that they are one tool. A call naming no operation this tool has
    /// declares all thirteen, which is the strictest reading of a call nobody can place, and
    /// `invoke` then refuses it by name.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match action(&call.args) {
            Ok(op) if operations().any(|it| it == op) => vec![domains::context(op)],
            // a word this tool does not have is refused by `invoke` with a list of the ones it
            // does; what it must not be is a call that needed nothing and was therefore allowed
            _ => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        // an operation this tool does not have falls through to the arm that names the ones it
        // does, so there is nothing to hold its arguments to yet; a word it knows is held to them
        // before anything is done with it
        if let Some(refusal) = unread(action(&call.args)?, &call.args, &TAKES) {
            return Ok(ToolOutput::error(refusal));
        }

        match action(&call.args)? {
            "look" => Ok(ToolOutput::new(look(
                &kernel,
                &ids(&call.args, "ids"),
                call.args["whole"].as_bool().unwrap_or(false),
            ))),
            "budget" => Ok(ToolOutput::new(budget(&kernel, &self.pinned.lock()))),
            "request" => Ok(ToolOutput::new(request(&kernel))),
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
            // note: the `reason` is asked for here rather than in the schema, because it is
            // required by nine of the thirteen and `required` in a schema is all or nothing.
            // `look` and `budget` change nothing and have nothing to justify
            op if CHANGES.contains(&op) => {
                let Some(reason) = call.args["reason"]
                    .as_str()
                    .filter(|it| !it.trim().is_empty())
                else {
                    return Ok(ToolOutput::error(
                        "`reason` is required by everything that changes something: it becomes \
                         the item's note, and it is what the person at the terminal reads when \
                         they ask why something is not in the request",
                    ));
                };

                Ok(self.amend.change(&kernel, call, op, reason))
            }
            other => Ok(ToolOutput::error(unknown(
                other,
                &operations().collect::<Vec<_>>(),
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
///
/// note: two columns of figures rather than one headed `tokens`, and they are the two the person's
/// own pane has shown all along: what an item puts into the next request, and what it is keeping
/// out of one. One column could only be one of those, and whichever it was would be wrong about
/// the rows that matter - an elided item, which sends a marker and holds its content, and any turn
/// whose thinking the endpoint will not take back, which is every turn under an OpenAI-compatible
/// one. Read as a budget, the held figure invites giving up what the request was not carrying;
/// read as an inventory, the sending figure hides tens of thousands of tokens the agent really is
/// carrying. Both, named, is the only honest answer, and it is what `held` in the next line and
/// the expensive list under `budget` are counted from.
fn look(kernel: &Kernel, ids: &[ContextId], whole: bool) -> String {
    let items = kernel.items();
    let going = Going::of(kernel);
    if !ids.is_empty() {
        return ids
            .iter()
            .map(|id| full(&items, *id, &going, whole))
            .collect::<Vec<_>>()
            .join("\n");
    }

    let budget = kernel.budget();
    // note: the undo depth is reported and named as somebody else's on purpose. It is the stack
    // behind the `u` key in the terminal, it holds everything that has ever happened to this
    // context, and `amend undo` does not touch it - a figure that big, sitting unlabelled next to
    // a tool called `undo`, would be an invitation to try to walk back the person's work
    let theirs = kernel.with_context(|context| context.undo_len());
    let withheld: usize = items.iter().map(|item| going.held_back(item)).sum();

    let mut out = inherited(kernel, &items);
    out.push_str(&format!(
        "{} items · {} of them go into the next request\n\
         ~{} tokens going{}, ~{} held back - what the request does not carry, whether because \
         you set a state or because the endpoint will not take it\n\
         {} change(s) in the person's own undo stack, which is theirs; `undo` walks back what \
         you did\n\n\
         {:>4}  {:<10}  {:<18}  {:>8}  {:>8}  what it is\n",
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
        "sending",
        "held",
    ));

    for item in &items {
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(going.costs.get(&item.id).copied().unwrap_or(0)),
            match going.held_back(item) {
                0 => String::new(),
                held => thousands(held),
            },
            row(item, &going),
        ));
    }

    // note: the second sentence is what a live run bought. The columns were right and a model
    // read them wrong: asked which item was holding the most and whether it could change that, it
    // named the turn holding 1,398 tokens of its own thinking and answered "yes, I can elide it" -
    // which would free the 68 that turn is *sending* and not one token of the 1,398, because the
    // endpoint was never being sent them. `budget` has said this under its own table all along;
    // `look` is where an agent actually reads the figure, and it said nothing.
    out.push_str(
        "\n`look` with `ids` reads any of these back, including the reasoning recorded on an \
         assistant turn; a long one arrives as its start and its end unless you ask for the \
         `whole` of it. What a row shows under `held` is already out of the next request: giving \
         that item up frees what it is `sending` and none of what it is holding.\n",
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
fn row(item: &ContextItem, going: &Going) -> String {
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
    // and why it is not in the request, where the state column cannot say. An item the projector
    // dropped is `active` and not going, so the row reads `0` under `sending` with nothing to
    // account for it - which a live run put in front of a model about its own latest turn: the
    // turn carrying the call being answered has no result yet, so it is out of the projection for
    // as long as the tool runs. The pane has said this on the row all along, in the projector's
    // own words, and this is the same sentence out of the same place
    if let Some(why) = going.left_out.get(&item.id) {
        said.push_str(&format!(" · not going: {why}"));
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

fn full(items: &[Arc<ContextItem>], id: ContextId, going: &Going, whole: bool) -> String {
    let Some(item) = items.iter().find(|item| item.id == id) else {
        return format!("[{id}] there is no such item\n");
    };

    // note: what it holds, and then how much of that the request does not carry - because this is
    // the view that reads a turn's *thinking* back, and under most endpoints the thinking is the
    // part that is not carried. A line reporting one figure would be telling the agent that the
    // page it is reading costs what it weighs, in the one place it is most likely to be wrong.
    //
    // note: `Going::holds` rather than `ContextItem::tokens`, so that the two figures in this one
    // sentence are counted on one scale - `Going::holds` has why that is not the same figure
    let mut out = format!(
        "[{}] {} · {} · from {} · {} · {} tokens{}\n",
        item.id,
        item.label,
        item.kind.name(),
        item.source,
        item.state,
        thousands(going.holds(item)),
        match going.held_back(item) {
            0 => String::new(),
            held => format!(
                ", {} of them held back from the next request",
                thousands(held)
            ),
        },
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
///
/// note: the four ways of being held back are named and then divided in place, rather than counted
/// off by an ordinal. It used to end `Only the first three are yours to change`, which meant the
/// first three of those four causes and was read by a live session as *items 1, 2 and 3* - an
/// understandable reading, because every other number on this screen is an item id and the table
/// under it opens with a column of them. The model spent the next four calls hunting for what
/// items 1-3 were hiding, reached outside the sandbox for `/proc/self/fd/0`, and then dumped the
/// whole log with `since: 0` - a 2,938-token item that was the most expensive thing it carried for
/// the next forty turns. An ordinal in a tool whose output is a numbered table has two readings and
/// costs whatever the wrong one costs.
fn budget(kernel: &Kernel, mine: &BTreeSet<ContextId>) -> String {
    let budget = kernel.budget();
    let going = Going::of(kernel);
    let withheld: usize = kernel
        .items()
        .iter()
        .map(|item| going.held_back(item))
        .sum();

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
         ~{} tokens are being held back: excluded, archived or elided to a marker - three states \
         you set, and `restore` takes any of them off again - or thinking this endpoint will not \
         take back, which is not yours to change\n",
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
    //
    // note: and sorted by what each one *costs the request*, not by what it holds. Those are the
    // same figure for most rows and not for a turn that thought at length: one held 25,903 tokens
    // and put 1,035 into the request, so ranked by what it held it stood at the top of a list
    // headed "the most expensive items actually going into it" - offering the agent 25,903 tokens
    // for an elision that would free a thousand. The column that decides is the column to rank on.
    let mut costly: Vec<_> = kernel
        .items()
        .into_iter()
        .filter(|item| going.sends_content(item))
        .collect();
    costly.sort_by_key(|item| std::cmp::Reverse(going.costs.get(&item.id).copied().unwrap_or(0)));
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
        "sending",
        "if all go",
    ));
    let mut running = 0;
    for item in &costly {
        let cost = going.costs.get(&item.id).copied().unwrap_or(0);
        running += cost;
        // saying so here saves a call that would only be refused, and the reason is the same one
        // `amend` would give: it is not the model's to move
        let whose = match protected(item, mine, None) {
            Some(_) => " · not yours",
            None => "",
        };
        // what it is holding on top of that, where it holds anything, because this row is where
        // an agent decides what to give up and the difference is the thing giving it up will not
        // free. A turn's thinking is gone from the request already
        let holding = match going.held_back(item) {
            0 => String::new(),
            held => format!(" · holding {} the request does not carry", thousands(held)),
        };
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}{whose}{holding}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(cost),
            thousands(running),
            glimpse(&format!("{}: {}", item.label, item.content.to_text())),
        ));
    }
    out.push_str(
        "\nthe fifth column is what eliding everything down to that row would save, give or take \
         what the markers cost. Eliding leaves a marker in place, so a tool result still answers \
         the call that asked for it; excluding one takes that call down with it, and the model \
         then reads a conversation in which it never asked. `elide` and `exclude` are the two. A \
         row that says \
         it is holding something is holding it out of the request already - giving that row up \
         frees the fourth column and not the rest.\n",
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
    // list could not say which was which. An item left out by its own state is one `restore`
    // puts straight back. An item the projector dropped is a consequence of something
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
        out.push_str("\nleft out by its own state, which `restore` puts back:\n");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema and [`TAKES`] say the same thing about every argument.
    ///
    /// note: the same check `fs` keeps, and it matters more here: thirteen operations share ten
    /// arguments, so the table is mostly a statement about which of them each one does *not* take,
    /// and there is no reading the code that makes that obvious. An argument the schema offers and
    /// no row takes would be refused the moment a model did as it was told.
    #[test]
    fn every_argument_the_schema_offers_is_one_some_action_takes() {
        let tool = Context::new(
            Reach(std::sync::Weak::new()),
            Pinned::default(),
            Limits::default(),
        );

        let spec = tool.spec();
        let declared: Vec<&str> = spec.schema["properties"]
            .as_object()
            .expect("the schema is an object with properties")
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "action")
            .collect();
        let taken: Vec<&str> = TAKES
            .iter()
            .flat_map(|(_, args)| args.iter().copied())
            .collect();

        for argument in &declared {
            assert!(
                taken.contains(argument),
                "the schema offers `{argument}` and no action takes it, so passing it is refused"
            );
        }
        for argument in &taken {
            assert!(
                declared.contains(argument),
                "`{argument}` is taken by an action and the schema never mentions it"
            );
        }

        // one list of operations, in one order: the `action` enum, the capabilities and this
        assert_eq!(
            TAKES.map(|(action, _)| action).to_vec(),
            operations().collect::<Vec<_>>(),
        );
    }

    /// Everything that changes something takes a `reason`, and nothing that only reads does.
    ///
    /// note: `reason` is required by nine of the thirteen and asked for in `invoke` rather than in
    /// the schema, because `required` in a schema is all or nothing. So the fact that the nine and
    /// only the nine take one lives in two places, and this is what holds them together: a change
    /// whose row forgot `reason` would refuse every call anybody made to it.
    #[test]
    fn the_nine_that_change_take_a_reason_and_the_four_that_read_do_not() {
        for (action, takes) in TAKES {
            assert_eq!(
                takes.contains(&"reason"),
                CHANGES.contains(&action),
                "`{action}` and `reason` disagree"
            );
        }
    }
}
