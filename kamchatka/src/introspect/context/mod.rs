//! `context`: what this session is carrying, what it costs, and what is carried into the next
//! request.
//!
//! note: named for what it is about rather than for what it does to it. `introspect` is the name
//! of the whole family, and anything else that reads a session from the inside is introspection
//! too. The id is the noun, which leaves the family its word and gives each tool the thing it is
//! about.
//!
//! note: one tool for reading the context and changing it, because separate permissions do not
//! need separate tools: [`Tool::needs`] lets a call declare its subject, so `context:look` and
//! `context:revise` are separate questions with separate rows whether they arrive under one tool's
//! name or two. Two tools would cost two descriptions in every request, most of each spent saying
//! which of the two the other one was.
//!
//! note: a directory, with the changing half in `changes.rs`: the journal `undo` walks, the
//! refusals, and the accounting that says what a change cost. What is here is what a model sees -
//! the schema, the dispatch, and everything that only reads.

use std::{collections::BTreeSet, sync::Arc};

use nachalnik::{
    Block, BoxError, Capability, Content, ContextId, ContextItem, ContextKind, Event, Kernel,
    OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use parking_lot::Mutex;
use serde_json::Value;

use crate::{
    app::{Going, text::thousands},
    tools::{
        Limits, domains,
        ops::{Arg, Op, action_of, actions, inner, schema, unread},
        yes_or_no,
    },
};

use super::{Reach, action, ids, named, protected, unknown};

mod changes;

use changes::{Changes, own_turn};

/// The items the agent pinned itself, shared between the half that sets them and the half that
/// reports them.
type Pinned = Arc<Mutex<super::Mine>>;

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// The eight that change, each named for what it leaves behind.
///
/// note: the word for the move is the word the result is reported in - an item you `elide` reads
/// back as `elided` everywhere it is listed. One level, and the same vocabulary at both ends of
/// it, rather than a `prune` action with a `state` argument, which would put the word for one of
/// the moves over all of them.
const CHANGES: [&str; 8] = [
    "elide", "exclude", "pin", "restore", "revise", "note", "undo", "redo",
];

/// Why a call that changes something has to say why, which is the same sentence eight times.
const WHY: &str = "why, in your own words; the person you work with reads this, and it becomes \
                   the item's note";

/// What a `select` is, said where the four that take one can read it.
///
/// note: "instead of" is said in both directions - here and on each `ids` beside it - because a
/// call giving both is refused and the schema cannot say so. `oneOf` would say it, and neither
/// dialect this crate speaks has that keyword.
const SELECT: &str = "a class of items instead of `ids`, never both in one call, in the selector \
                      grammar this tool's description sets out: `all:tool_results`, \
                      `state:elided`, `tool:shell:latest`";

/// The twelve operations, what each is for, and what each reads.
///
/// note: `reason` is `needed()` wherever a call changes something, which the schema can say
/// because it is a branch per operation: in one flat property bag, `required` is all or nothing.
/// Eight operations require it and four do not offer it at all.
///
/// note: twelve operations, eight shapes. The four that move an item read one argument list,
/// because they are one function, and so do `undo` and `redo`; each group is therefore one branch
/// under an `action` of several words. Written out per operation they would repeat `reason` and
/// `select` in branch after branch, on every request, to say a thing that is true once.
fn ops() -> Vec<Op> {
    let mut ops = vec![
        Op::new(
            "look",
            "lists every item - what it is, what it costs, whether it is going into the next \
             request and why not if it is not",
            vec![
                Arg::list(
                    "ids",
                    "integer",
                    "read these items in full - block by block, including what you were \
                     thinking when you produced them - instead of listing all of them",
                ),
                Arg::text(
                    "select",
                    "list only the items this class comes to, which is the set a change naming \
                     the same `select` would take: the grammar is the one in this tool's \
                     description. Not with `ids`, which names items to read rather than a class \
                     to resolve",
                ),
                Arg::truth(
                    "whole",
                    "read the named items entire rather than as a start and an end. It costs what \
                     carrying them costs",
                ),
            ],
        ),
        Op::new(
            "budget",
            "what the next request costs against what there is, what the last one really cost, \
             and which items are the expensive ones: read it before deciding what to give up",
            vec![],
        ),
        Op::new(
            "request",
            "the request you are about to send, message by message; what was left out and by \
             which rule - a state you set, which you can undo, or the projector, which you cannot \
             - and what the projector had to take out or put in a different order to make it a \
             request an endpoint will read",
            vec![],
        ),
        Op::new(
            "search",
            "finds text anywhere in your context, archived items included, which `look` can only \
             read by copying them in; it says how many lines match and what they would cost \
             before showing you one",
            vec![
                // note: it says what it is *not*, because `fs`'s `grep` is offered in the same
                // request and says outright that its `pattern` is a regular expression, so a model
                // reaching for one here is being consistent. A pattern read as text matches
                // nothing and the answer is a plain "no matches", which is the one shape of wrong
                // answer the note on `search` says this must not have
                Arg::text(
                    "text",
                    "what to look for: the words themselves, not a pattern - `fs`'s `grep` is the \
                     one that takes a regular expression. Case is ignored",
                )
                .needed(),
                Arg::list("ids", "integer", "look only in these items, by number"),
                Arg::whole(
                    "take",
                    "show this many of the matching lines; left out, you get the count and the \
                     price and no lines",
                ),
            ],
        ),
    ];

    ops.push(Op::these(
        &["elide", "exclude", "pin", "restore"],
        "moves items, and each of the four is named for the state it leaves - which is the word \
         you will read back on the item afterwards. Name them with `ids`, or a class of them with \
         `select`, and never with both in one call; `look` with the same `select` lists what it \
         comes to, if you want to see them before they move. `elide` replaces what an item says \
         with a short marker: the call it answers stays answered and stops costing what it holds, \
         which is what to reach for once a tool result has served its purpose. `exclude` takes it \
         out of the request altogether, and takes down the call that asked for it - reach for it \
         when you are done with something rather than merely finished reading it. `pin` protects \
         it from being compacted away. `restore` is the way back from any of the other three, a \
         `pin` of your own included - it puts an item back to plain active, so restoring something \
         you pinned unpins it.",
        vec![
            Arg::list(
                "ids",
                "integer",
                "the items to move, by the numbers `look` prints; not with `select`, which is the \
                 other way of saying which",
            ),
            Arg::text("select", SELECT),
            // note: read but not offered, because it is neither a way to move anything nor an
            // argument to refuse. A model reaches for `label` to say *which item*, and
            // `Changes::moved` answers that with the spelling it meant - which it cannot do if
            // `unread` has already refused the call, and should not have to do if the schema had
            // advertised `label` as the way to name one
            Arg::tolerated("label"),
            Arg::text("reason", WHY).needed(),
        ],
    ));

    ops.extend([
        Op::new(
            "revise",
            "rewrites what one item says, for when you wrote something down wrong",
            vec![
                Arg::one_of("ids", "integer", "the one item to rewrite").needed(),
                Arg::text("content", "what the item should say instead").needed(),
                Arg::text("reason", WHY).needed(),
            ],
        ),
        Op::new(
            "note",
            "writes something into your context - a plan, a conclusion, a thing not to try again. \
             Saying it in a turn is not the same: thinking is not carried into later requests, a \
             note is an item of its own that goes into every one and can be pinned",
            vec![
                Arg::text("content", "what to write down").needed(),
                // note: what it is *not* is half of this line. `label` reads as a key, so a model
                // writes notes under one name meaning each to replace the last, and the tool
                // appends every time - which the result also says when it happens. This is the
                // same sentence one step earlier, where the name is being chosen rather than
                // regretted
                Arg::text(
                    "label",
                    "a short name for it, so you can find it again. Not a key: a second note \
                     under a name is a second item, and `revise` is what changes one you already \
                     wrote",
                ),
                Arg::truth(
                    "pin",
                    "protect it from compaction, for a finding that has to outlast the context it \
                     was found in",
                ),
                Arg::text("reason", WHY).needed(),
            ],
        ),
        Op::these(
            &["undo", "redo"],
            "`undo` walks back through the changes *you* made here, and `redo` walks forward \
             again through what `undo` took back. Neither touches anything you did not do, and \
             an item somebody else has changed since you did is left as it is now.",
            vec![
                Arg::whole(
                    "steps",
                    "how many of your own changes to walk, from 1 to 64; 1 by default",
                ),
                Arg::text("reason", WHY).needed(),
            ],
        ),
    ]);

    ops
}

/// Reads the context and changes it: what is in it, what it costs, and what goes into the next
/// request.
pub struct Context {
    reach: Reach,
    pinned: Pinned,
    limits: Limits,
    /// The half that changes things, which keeps the journal `undo` walks and the set of items
    /// this tool pinned itself.
    changes: Changes,
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Context {
    /// Builds one; see [`super::install`], which is the only caller.
    ///
    /// note: the pinned set is made here and shared with the changing half rather than handed in.
    /// It is one tool's memory of which pins are its own, so a second holder of it could only ever
    /// be something that would disagree about a promise.
    pub(super) fn new(reach: Reach, limits: Limits) -> Self {
        let ops = ops();
        let pinned = Pinned::default();
        Self {
            changes: Changes::new(pinned.clone()),
            reach,
            pinned,
            limits,
            schema: Arc::new(schema(&ops)),
            ops,
        }
    }
}

#[async_trait]
impl Tool for Context {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "context",
            "your own context: what is in it, what it costs, and what you carry into the next \
             request. Four operations read it and eight change it, and each says what it does. \
             Nothing destroys anything: every item keeps its number and can be restored. What \
             is refused is changing a system instruction, the turn you are speaking in, or an \
             item the person you work with pinned - those are not yours. A pin of your own is, \
             and `restore` undoes it.\n\
             Where an operation takes a `select`, it is a class of items instead of `ids`: an \
             item number; `all`; `all:tool_results` (or files, diagnostics, selections, memories, \
             instructions, system, user, model, compaction); `kind:<kind>` or `state:<state>`, \
             taking the words `look` prints in those columns; `tool:<name>`, optionally `:first` \
             or `:latest`; `source:<name>`; `file:<path>`, a file attached rather than read; \
             `label:<text>`. Anything else is read as a label.",
        )
        .with_schema(self.schema.clone())
        .with_capabilities(
            actions(&self.ops)
                .into_iter()
                .map(domains::context)
                .collect::<Vec<_>>(),
        )
    }

    /// note: `action` and nothing else, so a rule about `context:look` is about looking whichever
    /// way a call asked for it - and so that the reading half and the changing half are still
    /// separately grantable now that they are one tool. A call naming no operation this tool has
    /// declares all twelve, which is the strictest reading of a call nobody can place, and
    /// `invoke` then refuses it by name.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match action_of(call, &self.ops) {
            Some(op) => vec![domains::context(op)],
            // a word this tool does not have is refused by `invoke` with a list of the ones it
            // does; what it must not be is a call that needed nothing and was therefore allowed
            None => self.spec().capabilities,
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
        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        let args = &*args;
        if let Some(refusal) = unread(action(args)?, args, &self.ops) {
            return Ok(ToolOutput::error(refusal));
        }

        match action(args)? {
            "look" => {
                let named = match named(&kernel.items(), args) {
                    Ok(named) => named,
                    Err(refusal) => return Ok(ToolOutput::error(refusal)),
                };
                let whole = match yes_or_no(args, "whole") {
                    Ok(whole) => whole,
                    Err(refusal) => return Ok(ToolOutput::error(refusal)),
                };
                Ok(ToolOutput::new(match named.select {
                    Some(select) => matched(
                        &kernel,
                        select,
                        &named.ids,
                        &self.pinned.lock(),
                        own_turn(&kernel, &call.id),
                    ),
                    None => look(&kernel, &named.ids, whole, own_turn(&kernel, &call.id)),
                }))
            }
            "budget" => Ok(ToolOutput::new(budget(&kernel, &self.pinned.lock()))),
            "request" => Ok(ToolOutput::new(request(&kernel))),
            "search" => {
                let Some(text) = args["text"].as_str().filter(|t| !t.is_empty()) else {
                    return Ok(ToolOutput::error(
                        "`search` needs the `text` to look for; `look` is the one that lists \
                         everything",
                    ));
                };
                let take = match taken(&args["take"]) {
                    Ok(take) => take,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                let only = match ids(args, "ids") {
                    Ok(ids) => ids,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                let own = own_turn(&kernel, &call.id);
                Ok(ToolOutput::new(search(&kernel, text, &only, take, own)))
            }
            // note: asked for here *as well as* in the schema. A branch per operation lets eight
            // of the twelve require it and four not offer it at all, but nothing is sent
            // `strict`, so the schema is advice and this is what holds
            op if CHANGES.contains(&op) => {
                let Some(reason) = args["reason"].as_str().filter(|it| !it.trim().is_empty())
                else {
                    return Ok(ToolOutput::error(
                        "`reason` is required by everything that changes something: it becomes \
                         the item's note, and it is what the person at the terminal reads when \
                         they ask why something is not in the request",
                    ));
                };

                Ok(self.changes.make(&kernel, call, args, op, reason))
            }
            other => Ok(ToolOutput::error(unknown(other, &actions(&self.ops)))),
        }
    }
}

/// How many matching lines a `search` was asked for, or what is wrong with the way it asked.
///
/// note: a `take` that cannot be read is refused, as `log` refuses its own, so the same word does
/// not mean two things a tool apart. Read with a bare `as_u64`, `take: 0` would print a heading
/// with a colon and nothing under it, which is a malformed answer rather than a wrong one; and
/// `take: -3` and `take: "3"` would be `None`, which is the summary that leaving `take` out gives:
/// the model asked for lines, was given a count, and nothing said its argument had not been read.
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
/// own pane shows: what an item puts into the next request, and what it is keeping out of one.
/// One column could only be one of those, and whichever it was would be wrong about the rows that
/// matter - an elided item, which sends a marker and holds its content, and any turn whose
/// thinking the endpoint will not take back, which is every turn under an OpenAI-compatible one.
/// Read as a budget, the held figure invites giving up what the request was not carrying; read as
/// an inventory, the sending figure hides tens of thousands of tokens the agent really is
/// carrying. Both, named, is the only honest answer, and it is what `held` in the next line and
/// the expensive list under `budget` are counted from.
fn look(kernel: &Kernel, ids: &[ContextId], whole: bool, own: Option<ContextId>) -> String {
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
    // context, and this tool's own `undo` does not touch it - a figure that big, sitting
    // unlabelled beside an operation called `undo`, would be an invitation to try to walk back
    // the person's work
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
        // what the projection carries, and the turn this call is in, which it will carry once
        // this call has an answer
        items
            .iter()
            .filter(|item| going.costs.contains_key(&item.id) || Some(item.id) == own)
            .count(),
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
            figure(item, &going),
            match going.held_back(item) {
                0 => String::new(),
                held => thousands(held),
            },
            row(item, &going),
        ));
    }
    out.push_str(floor(&items));

    // note: the second sentence is there because the columns are easy to read wrong. Asked what
    // it could free, a model names the turn holding the most of its own thinking and offers to
    // elide it - which frees what that turn is *sending* and not one token of what it holds,
    // because the endpoint is never sent those. `budget` says this under its own table, and
    // `look` is where an agent actually reads the figure
    out.push_str(
        "\n`look` with `ids` reads any of these back, including the reasoning recorded on an \
         assistant turn; a long one arrives as its start and its end unless you ask for the \
         `whole` of it. What a row shows under `held` is already out of the next request: giving \
         that item up frees what it is `sending` and none of what it is holding.\n",
    );

    out
}

/// What one row says an item is sending: its figure, with a `+` where part of it is unpriced.
///
/// note: marked as the context tab marks it, so that the two things `0` can mean are not one
/// figure. One function for both tables, because a `look` narrowed by a selector is the same
/// listing and a picture in it is no cheaper there.
fn figure(item: &ContextItem, going: &Going) -> String {
    let figure = thousands(going.costs.get(&item.id).copied().unwrap_or(0));
    match item.uncounted {
        0 => figure,
        _ => format!("{figure}+"),
    }
}

/// The line under a table that says what its `+` means, where any row carries one.
fn floor(items: &[Arc<ContextItem>]) -> &'static str {
    match items.iter().any(|item| item.uncounted != 0) {
        true => {
            "\na `+` is a floor: part of that item is content nothing here can put a number on, \
             so it and the total going cost more than they say\n"
        }
        false => "",
    }
}

/// The rows of the items a class comes to, which is the set a change naming the same class takes.
///
/// note: a selector is resolved against the context at the moment it is used, and without this the
/// only way to use one is to move something: `elide` with `select: "tool:shell"` says what it has
/// taken *after* taking it. Undoing that is one call, and knowing first is none.
///
/// note: the figures are the matched items' own rather than the session's, because that is the
/// number the decision turns on - what giving this class up would free - and the request's total
/// is on the line beside it to read them against. The rest of the accounting is `budget`'s.
///
/// note: the items a change would refuse are marked here rather than left to be discovered by the
/// change. A preview that named four items where a move takes three is exactly the confident wrong
/// answer the rest of this tool is written to avoid, and [`protected`] is the same function the
/// move itself consults, so the two cannot come apart.
fn matched(
    kernel: &Kernel,
    select: &str,
    ids: &[ContextId],
    mine: &super::Mine,
    own: Option<ContextId>,
) -> String {
    let items = kernel.items();
    let going = Going::of(kernel);
    let wanted: BTreeSet<ContextId> = ids.iter().copied().collect();
    let picked: Vec<&Arc<ContextItem>> =
        items.iter().filter(|it| wanted.contains(&it.id)).collect();
    if picked.is_empty() {
        return format!(
            "`{select}` is a selector, and nothing in your context matches it. `look` with no \
             `select` lists what there is.{}\n",
            crate::introspect::unmatched_file(select)
        );
    }

    let sending: usize = (picked.iter())
        .map(|item| going.costs.get(&item.id).copied().unwrap_or(0))
        .sum();
    let withheld: usize = picked.iter().map(|item| going.held_back(item)).sum();
    let carried: Vec<Arc<ContextItem>> = picked.iter().map(|item| Arc::clone(item)).collect();

    let mut out = inherited(kernel, &carried);
    out.push_str(&format!(
        "`{select}` matches {} of {} items · {} of them go into the next request\n\
         ~{} tokens going and ~{} held back, out of ~{} the whole request carries\n\n\
         {:>4}  {:<10}  {:<18}  {:>8}  {:>8}  what it is\n",
        picked.len(),
        items.len(),
        picked
            .iter()
            .filter(|item| going.costs.contains_key(&item.id) || Some(item.id) == own)
            .count(),
        thousands(sending),
        thousands(withheld),
        thousands(kernel.budget().used()),
        "id",
        "state",
        "kind",
        "sending",
        "held",
    ));

    let mut refused = 0;
    for item in &picked {
        let mut said = row(item, &going);
        if let Some(why) = protected(item, mine, own) {
            refused += 1;
            said.push_str(&format!(" · not yours to move: {why}"));
        }
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            figure(item, &going),
            match going.held_back(item) {
                0 => String::new(),
                held => thousands(held),
            },
            said,
        ));
    }

    out.push_str(floor(&carried));
    out.push_str(&format!(
        "\na change naming the same `select` takes these{}. Giving them up frees what they are \
         `sending` and none of what they are holding; `look` with `ids` reads any of them back in \
         full.\n",
        match refused {
            0 => String::new(),
            n => format!(", less the {n} marked as not yours"),
        }
    ));

    out
}

/// Which of these items this session did not produce, said before the listing rather than after.
///
/// note: a session resumed under a *second* model, asked whether it wrote an inherited turn,
/// reaches for `look` - and without this line `look` has nothing to say, because an item restored
/// from a snapshot is a perfectly ordinary item with no field that marks it. The model reads the
/// turn, recognises its own voice in it, and confabulates a first-person account of writing a
/// sentence another model wrote.
///
/// note: `setup` with `model` says this too, and a model asking the question does not call it. A
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

/// Whether this session was resumed from a snapshot, which `session.resumed` in its log says.
pub(super) fn resumed(kernel: &Kernel) -> bool {
    kernel.with_history(|session| {
        session
            .records()
            .any(|record| matches!(record.event, Event::SessionResumed { .. }))
    })
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
    // account for it - which is how a model sees its own latest turn: the turn carrying the call
    // being answered has no result yet, so it is out of the projection for as long as the tool
    // runs. The pane says this on the row, in the projector's own words, and this is the same
    // sentence out of the same place
    if let Some(why) = going.left_out.get(&item.id) {
        said.push_str(&format!(" · not going: {why}"));
    }

    said
}

/// How much of an item's content `look` shows either side of the gap before it is asked for the
/// whole thing.
///
/// note: reading an item copies that item into the context. So asking to see a 9,000-token tool
/// result in order to decide whether to keep it costs very nearly what keeping it costs, and a
/// clean-up done that way can finish heavier than it started. A head and a tail is enough to tell
/// build noise from something worth keeping, and the whole thing is still one argument away for
/// the times it is really wanted.
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

/// The whole of one item, or the fact that there is no such item.
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
        "[{}] {} · {} · from {} · {} · {} tokens{}{}\n",
        item.id,
        item.label,
        item.kind.name(),
        item.source,
        item.state,
        thousands(going.holds(item)),
        match item.uncounted {
            0 => String::new(),
            n => format!(" and {n} piece(s) nothing here can price"),
        },
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
/// other way to see inside one is to read it back, which copies it into the context - so a session
/// could not look at anything it had put away without undoing the saving. That would make the
/// archive write-only from the model's side, which is not what "nothing is destroyed" is supposed
/// to mean.
///
/// note: so the rule is `log`'s rule, for the same reason: the count and its price first, the
/// lines on request, and never the item. A search that answered with what it found would be a
/// second way to pay for an item without meaning to, which is the thing this action exists to
/// undo.
///
/// note: case is ignored. A model that searched for `landlock` and was told there are no matches
/// in a context full of `Landlock` has been told something false about itself, and the failure is
/// silent - which is the one shape of wrong answer a search must not have.
fn search(
    kernel: &Kernel,
    text: &str,
    only: &[ContextId],
    take: Option<usize>,
    own: Option<ContextId>,
) -> String {
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
        // what a turn thought and what it called a tool with are in the context too, and a search
        // that skipped them would answer "no line says it" about an argument the model passed
        let mut hay = item.content.to_text().into_owned();
        if let Some(reasoning) = item.reasoning() {
            hay.push('\n');
            hay.push_str(&reasoning.to_text());
        }
        // but not the turn asking: its calls carry the text being searched for
        for asked in item.calls().filter(|_| Some(item.id) != own) {
            hay.push_str(&format!("\n{} {}", asked.tool, asked.args));
        }
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
        // note: counted in the lowercased line, which is where the offset came from. Lowercasing
        // can change a character's length - the Kelvin sign is three bytes and its `k` is one - so
        // the same offset into the line as written can land inside a character, which is a panic
        let lower = line.to_lowercase();
        let at = lower
            .find(needle)
            .map(|byte| lower[..byte].chars().count())
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
/// off by an ordinal. Every other number on this screen is an item id and the table under it opens
/// with a column of them, so "the first three" of four causes reads as *items 1, 2 and 3* - and a
/// model hunting for what those items are hiding spends calls, and carries what they fetch. An
/// ordinal in a tool whose output is a numbered table has two readings and costs whatever the
/// wrong one costs.
fn budget(kernel: &Kernel, mine: &super::Mine) -> String {
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
         ~{} tokens are being held back: excluded or elided to a marker, which you set; archived, \
         which is where a note you undid and the whole of a shortened answer go; or thinking this \
         endpoint will not take back, which is not yours to change. `restore` puts an excluded, \
         elided or archived item back\n",
        thousands(budget.used()),
        thousands(budget.context_tokens),
        thousands(budget.tool_tokens),
        thousands(withheld),
    );
    // straight after the figures it qualifies, as `/budget` puts it for the person: a picture reads
    // as nothing, so a context carrying one looks small when it is not
    if !budget.fully_counted() {
        out.push_str(&format!(
            "{} piece(s) of content nothing here can put a number on, so every figure above is a \
             floor and the real request is larger\n",
            budget.uncounted
        ));
    }

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
    // same figure for most rows and not for a turn that thought at length, which can hold tens of
    // thousands of tokens and put a thousand into the request. Ranked by what it held, it would
    // top a list headed "the most expensive items actually going into it", offering the agent
    // tens of thousands of tokens for an elision that frees a thousand. The column that decides is
    // the column to rank on.
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
        // a move would give: it is not the model's to move
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
        // note: what goes out rather than what was said, which is what `to_text` answers: a turn
        // that only calls a tool says nothing and sends its arguments, and a picture's text is
        // its name. A turn carried as blocks holds its calls and its thinking in its content
        let bytes = message.content.as_ref().map_or(0, Content::byte_len)
            + message
                .tool_calls
                .iter()
                .map(ToolCall::byte_len)
                .sum::<usize>()
            + message.reasoning.as_ref().map_or(0, Content::byte_len);
        out.push_str(&format!(
            "{:>4}  {:<10}  {:>8}  {}\n",
            index + 1,
            message.role.as_str(),
            thousands(bytes),
            glimpse(&said),
        ));
    }

    // note: split by *what* dropped it, because the two halves are answered differently and one
    // list could not say which was which. An item left out by its own state is one `restore`
    // puts straight back. An item the projector dropped is a consequence of something else in
    // the context, and restoring it does nothing whatever - the thing to move is the cause. A
    // model reading one undifferentiated list has to guess which it is looking at, and the cheap
    // guess is `restore`, which is the one that changes nothing and costs a call.
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
    // note: under a heading of its own, and one that says nothing was lost. This tool is read by a
    // model deciding what to do next, and "rewrote, to keep the request valid" over a line about a
    // tool result changing places is an invitation to go and fix something that is not broken -
    // which is what a `note` produces, every single time it is written
    if !projection.reordered.is_empty() {
        out.push_str(
            "\nand what it put in a different order than your context holds it, which costs \
             nothing and is nothing to act on:\n",
        );
        for moved in &projection.reordered {
            out.push_str(&format!("  {moved}\n"));
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

    /// The schema and the permission subjects are one vocabulary.
    ///
    /// note: an argument the schema offers that no operation reads cannot happen - they are the
    /// same `Vec<Op>`, and the branch an argument appears in is the operation that reads it. What
    /// can still drift is a subject with no branch to reach it by, or a branch the policy was
    /// never told about, which is a call that cannot be refused by name.
    #[test]
    fn the_schema_and_the_subjects_are_one_vocabulary() {
        let spec = tool().spec();

        let offered: Vec<String> = crate::tools::ops::offered(&spec.schema)
            .into_iter()
            .map(|action| domains::context(action).to_string())
            .collect();
        let declared: Vec<String> = spec.capabilities.iter().map(ToString::to_string).collect();

        assert_eq!(offered, declared, "one list of operations, in one order");
        assert_eq!(offered.len(), 12, "four that read and eight that change");
    }

    /// Everything that changes something requires a `reason`, and nothing that only reads offers
    /// one.
    ///
    /// note: the assertion is against the schema a model is actually shown - the branches that
    /// demand a `reason`, and the four that do not mention one - rather than a table held to
    /// `CHANGES`, which could only check that the *tool* would ask.
    #[test]
    fn the_eight_that_change_require_a_reason_and_the_four_that_read_do_not() {
        let spec = tool().spec();
        let branches = spec.schema["properties"]["call"]["anyOf"]
            .as_array()
            .expect("a branch per operation")
            .clone();

        for branch in branches {
            let action = branch["properties"]["action"]["enum"][0]
                .as_str()
                .expect("a branch names its own action");
            let required = branch["required"]
                .as_array()
                .expect("a branch says what it requires")
                .iter()
                .any(|it| it == "reason");
            let offered = branch["properties"]["reason"].is_object();

            assert_eq!(
                required,
                CHANGES.contains(&action),
                "`{action}` and `reason` disagree about being required"
            );
            assert_eq!(
                offered, required,
                "`{action}` offers a `reason` it does not require, or the other way about"
            );
        }
    }

    fn tool() -> Context {
        Context::new(Reach(std::sync::Weak::new()), Limits::default())
    }
}
