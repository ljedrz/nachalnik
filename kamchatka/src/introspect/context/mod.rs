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
//! refusals, and the accounting that says what a change cost. Everything that only reads is in
//! `reads.rs`, and what is here is what a model sees of both - the schema and the dispatch.

use std::sync::Arc;

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use parking_lot::Mutex;
use serde_json::Value;

use crate::tools::{
    Limits, domains,
    ops::{Arg, Op, action_of, actions, inner, schema, unread},
    yes_or_no,
};

use super::{Reach, action, ids, named, unknown};

mod changes;
mod reads;

use changes::{Changes, asking_turn, own_turn};
use reads::{budget, look, matched, request, search, taken};

/// The items the agent pinned itself, shared between the half that sets them and the half that
/// reports them.
type Pinned = Arc<Mutex<super::Mine>>;

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
            "finds text anywhere in your context, excluded items included, which `look` can only \
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
                    "show this many of the matching lines, up to 64; left out, you get the count \
                     and the price and no lines",
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
             request. Four operations read it and eight change it. Nothing destroys anything: \
             every item keeps its number and can be restored. What is refused is changing a \
             system instruction, the turn you are speaking in, or an item the person you work \
             with pinned - those are not yours. A pin of your own is, and `restore` undoes it.\n\
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
                    None => look(
                        &kernel,
                        &named.ids,
                        whole,
                        own_turn(&kernel, &call.id),
                        &self.pinned.lock(),
                    ),
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
                let take = match taken(args) {
                    Ok(take) => take,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                let only = match ids(args, "ids") {
                    Ok(ids) => ids,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                let own = asking_turn(&kernel, &call.id);
                Ok(ToolOutput::new(search(&kernel, text, &only, take, &own)))
            }
            // note: asked for here *as well as* in the schema. A branch per operation lets eight
            // of the twelve require it and four not offer it at all, but nothing is sent
            // `strict`, so the schema is advice and this is what holds
            op if CHANGES.contains(&op) => {
                let Some(reason) = args["reason"].as_str().filter(|it| !it.trim().is_empty())
                else {
                    // note: it says nothing was done and what to call again. A refusal that gave
                    // only why a reason is asked for was taken for a remark about a note the
                    // model had written, and it went on to put away the results the note was
                    // written from. The why is `WHY`, which the schema carries on every request
                    return Ok(ToolOutput::error(format!(
                        "`reason` is required by everything that changes something, and nothing \
                         was done: call `{op}` again with one"
                    )));
                };

                Ok(self.changes.make(&kernel, call, args, op, reason))
            }
            other => Ok(ToolOutput::error(unknown(other, &actions(&self.ops)))),
        }
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
