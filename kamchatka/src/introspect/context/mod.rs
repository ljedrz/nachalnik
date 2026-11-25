//! `context`: what this session carries, what it costs, and what goes into the next request.
//!
//! note: named for what it is about, not what it does to it. `introspect` names the family;
//! anything that reads a session from the inside is introspection. The id is the noun, leaving the
//! family its word and giving each tool the thing it is about.
//!
//! note: one tool reads and changes the context because separate permissions do not need separate
//! tools: [`Tool::needs`] lets a call declare its subject, so `context:read` and `context:revise`
//! are separate questions with separate rows whether they arrive under one tool's name or two. Two
//! tools would cost two descriptions in every request.
//!
//! note: four subjects for the eleven operations, named for what a call does to the context; see
//! [`subjects`]. One subject per operation let a refused `context:look` leave `search` reading the
//! items somebody took out, as a refused `fs:read` once left `grep` printing files.
//!
//! note: a directory. `changes.rs` holds the journal `undo` walks, the refusals, and the accounting
//! of what a change cost. `reads.rs` holds what only reads. This file holds the schema and the
//! dispatch.

use std::sync::Arc;

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use parking_lot::Mutex;
use serde_json::Value;

use crate::tools::{
    Limits, domains,
    ops::{Arg, Layout, Op, action_of, actions, inner, schema, shape},
    yes_or_no,
};

use super::{Reach, action, ids, named, unknown};

/// What a call to `op` needs, by what it does to the context: `read` for the four that only read
/// it; `state` for `elide`, `exclude` and `reset_state`, which change what of an item goes into the
/// request and never what it says; `pin` for keeping an item from the compactor; `note` for adding
/// an item of the model's own; and `revise` for rewriting what one says. `undo` walks back
/// through any of the changes, so it needs all four that change it, as does a word this tool does
/// not have.
///
/// note: `pin` on its own, though it is one of the four state changes in the schema, because it
/// decides something else: what the person's compactor may take, rather than what the model sends.
///
/// note: `state` with `read` is what a run that should decide what goes into its requests, and
/// change nothing else, is granted.
pub(crate) fn subjects(op: &str) -> Vec<Capability> {
    let one = |subject| vec![domains::context(subject)];
    match op {
        "look" | "budget" | "request" | "search" => one("read"),
        "elide" | "exclude" | "reset_state" => one("state"),
        "pin" => one("pin"),
        "note" => one("note"),
        "revise" => one("revise"),
        _ => ["state", "pin", "note", "revise"]
            .map(domains::context)
            .to_vec(),
    }
}

mod changes;
mod reads;

pub(crate) use changes::Journal;
use changes::{Changes, asking_turn, own_turn};
use reads::{budget, look, matched, request, search, taken};

/// The items the agent pinned itself, shared between the half that sets them and the half that
/// reports them.
type Pinned = Arc<Mutex<super::Mine>>;

/// The seven operations that change something.
///
/// note: three of the state changes are named for the state they leave - an item you `elide` reads
/// back as `elided` everywhere - and `reset_state` for what it does to the other three. One
/// vocabulary at both ends, rather than a `prune` action with a `state` argument, which would put
/// one change's word over all of them.
const CHANGES: [&str; 7] = [
    "elide",
    "exclude",
    "pin",
    "reset_state",
    "revise",
    "note",
    "undo",
];

/// Why a call that changes something has to say why, which is the same sentence seven times.
const WHY: &str = "why, in your own words; the person reads it";

/// What a `select` is, said where the four that take one can read it.
///
/// note: "instead of" is said in both directions - here and on each `ids` beside it - because a
/// call giving both is refused and the schema cannot say so. `oneOf` would say it, and neither
/// dialect this crate speaks has that keyword.
const SELECT: &str = "a selector, written as this tool's description lists them, instead of \
                      `ids`; never both";

/// The eleven operations, what each is for, and what each reads.
///
/// note: `reason` is `needed()` wherever a call changes something. The schema can say so because it
/// branches per operation: in one flat property bag, `required` is all or nothing. Seven operations
/// require it; four do not offer it.
///
/// note: eleven operations, eight shapes. The four state changes read one argument list because
/// they are one function; they are one branch under an `action` of several words. Written per
/// operation they would repeat `reason` and `select` on every request to say something true once.
fn ops() -> Vec<Op> {
    let mut ops = vec![
        Op::new(
            "look",
            format!(
                "lists the items - what each is, what it costs, whether it is going into the next \
                 request and why not if it is not. One call lists at most the {} most recent, with \
                 or without a `select`. When there are more, a line above the table says how many \
                 earlier ones were left out and gives a `select` range, such as `1-50`, that lists \
                 the ones just before; for older ones still, call it again with an earlier range. \
                 With `ids` it shows those items' content instead, in any state: an elided or \
                 excluded item shows what it holds, not its marker",
                reads::ROWS
            ),
            vec![
                Arg::list(
                    "ids",
                    "integer",
                    format!(
                        "show the content of these items instead of listing them, including the \
                         reasoning in your own replies, which is kept here even though it is not \
                         sent back in later requests. An item longer than {} \
                         bytes shows its first and last {} bytes and says how much is between \
                         them, unless `whole` asks for all of it",
                        2 * reads::SAMPLE,
                        reads::SAMPLE
                    ),
                ),
                Arg::text(
                    "select",
                    "list only the items this selector matches: the items a state change with \
                     the same `select` would apply to. Use either `ids` or `select`, not both",
                ),
                Arg::truth(
                    "whole",
                    "`true`, together with `ids`, to show each item in full instead of its first \
                     and last part. The full text goes into your context and costs its tokens \
                     there",
                ),
            ],
        ),
        Op::new(
            "budget",
            "shows what the next request will cost against the model's context limit, what the \
             last request really cost, and which items are the expensive ones: call it before \
             deciding what to elide or exclude",
            vec![],
        ),
        Op::new(
            "request",
            "shows the next request, message by message, and why each left-out item was left out: \
             either its state, which you can change, or the projector, which you cannot. The \
             projector is the fixed rule that turns your context into messages. The answer also \
             lists what the projector had to drop or reorder so that the endpoint accepts the \
             request",
            vec![],
        ),
        Op::new(
            "search",
            "finds text anywhere in your context, excluded items included, except the current \
             turn - the person's latest message, your replies since then and their results - \
             which you already have. Without `take` it shows no lines, only how many match and \
             what they would cost in tokens, so you can decide before reading them",
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
                Arg::list("ids", "integer", "search only these items, by number"),
                Arg::whole(
                    "take",
                    "show this many of the matching lines, up to 64; left out, you get only the \
                     number of matches and their token cost",
                ),
            ],
        ),
    ];

    ops.push(Op::these(
        &["elide", "exclude", "pin", "reset_state"],
        "changes the state of items, named with `ids` or `select`. To see which items a `select` \
         matches before changing them, call `look` with it.\n\
         - `elide`: the item stays in the request, but its content there is replaced by a short \
         marker. Use it on a tool result you no longer need; the tool call it answers stays in \
         the request.\n\
         - `exclude`: the item is left out of the request entirely; for a tool result, the \
         call that asked for it is left out too.\n\
         - `pin`: compaction, which elides or excludes older items on its own when the context \
         fills up, will not touch the item.\n\
         - `reset_state`: makes the item active again, undoing `elide`, `exclude` or your own \
         `pin`. It does not undo a `revise`.\n\
         On one of your own replies, `elide` replaces its text and reasoning but keeps its tool \
         calls, so their results stay; `exclude` leaves out the reply and its results together.",
        vec![
            Arg::list(
                "ids",
                "integer",
                "the item numbers, as `look` shows them. Use either `ids` or `select`, not both",
            ),
            Arg::text("select", SELECT),
            // note: `label` is read but not offered because it is neither a way to name items to
            // change nor an argument to refuse. A model reaches for `label` to say which item, and
            // `Changes::moved` answers with the spelling it meant - which it cannot do if `unread`
            // has already refused the call, and should not have to do if the schema advertised
            // `label` as the way to name one
            Arg::tolerated("label"),
            Arg::text("reason", WHY).needed(),
        ],
    ));

    ops.extend([
        Op::new(
            "revise",
            "rewrites the text of one item, for when something written down is wrong. Reasoning \
             and tool calls stay as they were. It works on anything except the person's \
             messages, tool results, pictures, PDFs and other files that are not text, and \
             replies that hold only tool calls; if an item still cannot be revised, the refusal \
             says why",
            vec![
                Arg::one_of(
                    "ids",
                    "integer",
                    "a list holding the number of the one item to rewrite, such as `[12]`",
                )
                .needed(),
                Arg::text("content", "what the item should say instead").needed(),
                Arg::text("reason", WHY).needed(),
            ],
        ),
        Op::new(
            "note",
            "adds a note to your context: a plan, a conclusion, a thing not to try again. \
             Your reasoning is not sent back in later requests, so a conclusion reached only \
             there is lost; a note is an item of its own that goes into every request and can be \
             pinned",
            vec![
                Arg::text("content", "what to write down").needed(),
                // note: what it is *not* is half of this line. `label` reads as a key, so a model
                // writes notes under one name meaning each to replace the last, and the tool
                // appends every time - which the result also says when it happens. This is the
                // same sentence one step earlier, where the name is being chosen rather than
                // regretted
                Arg::text(
                    "label",
                    "a short name for it, so you can find it again. A second note with the same \
                     label is a second item and does not replace the first; to change a note, \
                     use `revise`",
                ),
                Arg::truth(
                    "pin",
                    "`true` to pin the note at once, so compaction will not elide or exclude it",
                ),
                Arg::text("reason", WHY).needed(),
            ],
        ),
        Op::new(
            "undo",
            "takes back your most recent change that has not been undone yet. One change is one \
             earlier `context` call, however many items it changed; call `undo` again to take \
             back the one before. Undoing a `note` \
             excludes it; undoing a `revise` puts back the item's contents as they were before \
             that revision. Items somebody else changed since are left as they are. Only your 16 \
             most recent changes can be undone. An older state change can still be reversed with \
             `reset_state`.",
            vec![Arg::text("reason", WHY).needed()],
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
    /// Builds one; see [`super::build`], which is the only caller.
    ///
    /// note: the pinned set is made here and shared with the changing half rather than handed in.
    /// It is one tool's memory of which pins are its own, so a second holder of it could only ever
    /// be something that would disagree about a promise.
    pub(super) fn new(
        reach: Reach,
        limits: Limits,
        layout: Layout,
        journal: Arc<Mutex<Journal>>,
    ) -> Self {
        let ops = ops();
        let pinned = Pinned::default();
        Self {
            changes: Changes::new(pinned.clone(), reach.1.clone(), journal),
            reach,
            pinned,
            limits,
            schema: Arc::new(schema(&ops, layout)),
            ops,
        }
    }
}

#[async_trait]
impl Tool for Context {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "context",
            format!(
                "your own context: what is in it, what it costs, and what goes into the next \
                 request. The `action` is one of `look`, `budget`, `request` or `search`, which \
                 read it, or `elide`, `exclude`, `pin`, `reset_state`, `revise`, `note` or \
                 `undo`, which change it.\n\
                 Your context is a numbered list of items: the person's messages, your replies, \
                 tool results, notes, attached files and summaries. Each item has a state: \
                 `active`, `elided`, `excluded` or `pinned`. Nothing is deleted: an item keeps \
                 its number and its content whatever its state.\n\
                 No change can touch a system instruction, an item the person pinned, or the \
                 reply you are writing now (its text, reasoning and tool calls).\n\
                 A `select` picks items by a rule instead of listing their numbers in `ids`:\n\
                 - `17`: item 17; `10-15`: items 10 to 15, both included; `all`: every item\n\
                 - `tool:<name>`: every result of that tool; `tool:<name>:latest`: only its most \
                 recent result\n\
                 - `kind:<kind>`, what the item is: `system`, `user_message`, `assistant_message` \
                 (your replies), `tool_result`, `reference` (notes, attached files and summaries)\n\
                 - `state:<state>`: `active`, `elided`, `excluded`, `pinned`\n\
                 - `source:<name>`, where the item came from: `user` (the person's messages), \
                 `model` (your replies), `tool_result`, `file` (a file the person attached), \
                 `memory` (a note the person wrote), `agent` (your notes), `compaction` \
                 (summaries)\n\
                 - `file:<path>`: the attached file with that path\n\
                 - `label:<text>`: items whose label, as `look` shows it, is exactly that text\n\
                 A selector that matches nothing changes nothing.{}",
                shape(
                    Layout::of(&self.schema),
                    r#"{"action": "elide", "select": "tool:shell:latest", "reason": "it passed"}"#
                )
            ),
        )
        .with_schema(self.schema.clone())
        .with_capabilities(["read", "state", "pin", "note", "revise"].map(domains::context))
    }

    /// note: decided by `action` and nothing else, so a rule about `context:read` covers reading
    /// however a call asked for it, and the reading half and changing half stay separately
    /// grantable although they are one tool. A call naming no operation this tool has is refused by
    /// `malformed` before the policy is asked; one that reaches here anyway declares all four
    /// subjects, which is the strictest reading of a call nobody can place.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match action_of(call, &self.ops) {
            Some(op) => subjects(&op),
            // a word this tool does not have is refused by `invoke` with a list of the ones it
            // does; what it must not be is a call that needed nothing and was therefore allowed
            None => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    fn unparsed(&self, written: &str) -> Option<String> {
        crate::tools::ops::advice(written, Layout::of(&self.schema))
    }

    fn malformed(&self, call: &ToolCall) -> Option<String> {
        super::malformed(call, &self.schema, &self.ops)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        // an operation this tool does not have, or an argument it does not take, is refused before
        // anything is done with it
        if let Some(refusal) = self.malformed(call) {
            return Ok(ToolOutput::error(refusal));
        }
        let args = inner(&call.args).map_err(|misread| misread.said(Layout::of(&self.schema)))?;
        let args = &*args;

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
                    // `whole` reads items named by `ids`, and a class is listed rather than read:
                    // said rather than passed over, or the model takes the rows for what it asked
                    Some(select) => {
                        let mut listed = matched(
                            &kernel,
                            select,
                            &named.ids,
                            &self.pinned.lock(),
                            own_turn(&kernel, &call.id),
                            self.reach.policy(),
                        );
                        if whole {
                            listed.push_str(
                                "`whole` reads the items `ids` names, and a `select` lists a \
                                 class rather than reading it: `look` with their numbers as \
                                 `ids` and `whole` reads them entire.\n",
                            );
                        }
                        listed
                    }
                    None => look(
                        &kernel,
                        &named.ids,
                        whole,
                        own_turn(&kernel, &call.id),
                        &self.pinned.lock(),
                        self.reach.policy(),
                    ),
                }))
            }
            "budget" => Ok(ToolOutput::new(budget(
                &kernel,
                &self.pinned.lock(),
                own_turn(&kernel, &call.id),
            ))),
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
            // note: asked for here *as well as* in the schema. A branch per operation lets seven
            // of the eleven require it and four not offer it at all, but nothing is sent
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

    /// Every operation needs subjects the tool declares, and every subject is needed by some
    /// operation.
    ///
    /// note: an argument the schema offers that no operation reads cannot happen - they are the
    /// same `Vec<Op>`, and the branch an argument appears in is the operation that reads it. What
    /// can still drift is a subject no operation reaches, which a rule can name and nothing ever
    /// asks about, or an operation needing a subject the policy was never told about, which is a
    /// call that cannot be refused by name.
    #[test]
    fn every_operation_needs_subjects_the_tool_declares() {
        let tool = tool();
        let spec = tool.spec();

        let needed: Vec<(&str, String)> = crate::tools::ops::offered(&spec.schema)
            .into_iter()
            .map(|action| {
                let call = ToolCall::new(
                    "c1",
                    "context",
                    serde_json::json!({ "call": { "action": action } }),
                );
                let needs: Vec<String> =
                    tool.needs(&call).iter().map(ToString::to_string).collect();
                (action, needs.join(" "))
            })
            .collect();
        let wanted = [
            ("look", "context:read"),
            ("budget", "context:read"),
            ("request", "context:read"),
            ("search", "context:read"),
            ("elide", "context:state"),
            ("exclude", "context:state"),
            ("pin", "context:pin"),
            ("reset_state", "context:state"),
            ("revise", "context:revise"),
            ("note", "context:note"),
            (
                "undo",
                "context:state context:pin context:note context:revise",
            ),
        ];
        assert_eq!(
            needed,
            wanted.map(|(action, needs)| (action, needs.to_owned()))
        );
        let declared: Vec<String> = spec.capabilities.iter().map(ToString::to_string).collect();
        assert_eq!(
            declared,
            [
                "context:read",
                "context:state",
                "context:pin",
                "context:note",
                "context:revise"
            ]
        );
    }

    /// The seven changing branches require a `reason`; the four read-only branches do not mention
    /// one.
    ///
    /// note: the test checks the schema shown to the model, not a table bound to `CHANGES`, which
    /// could only check that the tool would ask.
    #[test]
    fn the_seven_that_change_require_a_reason_and_the_four_that_read_do_not() {
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
        Context::new(
            Reach(
                std::sync::Weak::new(),
                Arc::new(crate::tools::Careful::new()),
            ),
            Limits::default(),
            Default::default(),
            Default::default(),
        )
    }
}
