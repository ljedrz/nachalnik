//! What a tool that does several things declares: one table of operations, and the schema, the
//! refusal and the vocabulary all made out of it.
//!
//! note: one table rather than three declarations held together by a test - the `action` enum a
//! model chooses from, what each of those reads, and a flat schema of every argument any of them
//! takes. A test can keep three lists from drifting; what it cannot fix is that the *model* is
//! shown only the flat one: `{"action": "read", "old": "x"}` is valid against it, gets as far as
//! `invoke`, and is refused there - a round trip spent telling a model what the schema could have
//! told it for nothing.
//!
//! note: so the table is the declaration and the schema is made from it. An operation is a branch
//! of an `anyOf`, carrying its own arguments and its own `required`, which is what makes the call
//! above unrepresentable rather than merely refused. [`unread`] stays as the backstop, reading the
//! same table: nothing is sent `strict`, so the schema guides rather than binds, and a model that
//! ignores it is still answered by name.

use std::borrow::Cow;

use nachalnik::{PermissionRequest, ToolCall, ToolSpec};
use serde_json::{Map, Value, json};

/// The one property every one of these tools takes, with the operation inside it.
///
/// note: a wrapper because the root of a schema may not itself be an `anyOf` - OpenAI says so
/// outright ("the root level object of a schema must be an object, and not use `anyOf`") and it is
/// the one rule that decides the shape here. A union has to live under a property, so there is a
/// property. Everything else about the call is as it would be without one: `action` names the
/// operation, inside.
///
/// note: read forgivingly. A model that passes the arguments flat is understood rather than
/// refused - the call is unambiguous and refusing it costs a
/// turn. What is refused is arguments in both places at once, which is the only reading that could
/// silently do something nobody asked for; see [`inner`].
pub(crate) const WRAPPER: &str = "call";

/// What the wrapper says it is, which is the same sentence for every tool that has one.
const ABOUT: &str = "the operation to perform, and the arguments that operation takes";

/// One argument of one operation: its name, its schema, and whether leaving it out is a call.
pub(crate) struct Arg {
    name: &'static str,
    /// How the schema describes it, or nothing where it is read but not offered; see
    /// [`Arg::tolerated`].
    schema: Option<Value>,
    required: bool,
}

impl Arg {
    /// A string.
    pub(crate) fn text(name: &'static str, about: impl Into<String>) -> Self {
        Self::of(name, "string", about)
    }

    /// A whole number, read back by [`super::whole`].
    pub(crate) fn whole(name: &'static str, about: impl Into<String>) -> Self {
        Self::of(name, "integer", about)
    }

    /// A yes or no, read back by [`super::truth`].
    pub(crate) fn truth(name: &'static str, about: impl Into<String>) -> Self {
        Self::of(name, "boolean", about)
    }

    /// A list of one kind of thing: `"integer"`, `"string"`.
    pub(crate) fn list(name: &'static str, of: &'static str, about: impl Into<String>) -> Self {
        Self {
            name,
            schema: Some(json!({
                "type": "array",
                "items": { "type": of },
                "description": about.into(),
            })),
            required: false,
        }
    }

    /// A list of exactly one thing, which is a different claim from a list.
    pub(crate) fn one_of(name: &'static str, of: &'static str, about: impl Into<String>) -> Self {
        let mut arg = Self::list(name, of, about);
        if let Some(schema) = arg.schema.as_mut() {
            schema["minItems"] = json!(1);
            schema["maxItems"] = json!(1);
        }
        arg
    }

    /// An argument the operation reads but does not offer.
    ///
    /// note: for the one case that is neither. `context`'s four moves read `label` so that a call
    /// giving one instead of `ids` can be answered with the spelling it meant - a model may reach
    /// for it that way, and `Changes::moved` says `select: "label:<text>"` back. It is not a way to
    /// name the items to move, so advertising it would teach exactly the mistake the answer exists
    /// to correct; and leaving it out of the table altogether would have [`unread`] refuse the call
    /// before that answer could be given.
    pub(crate) fn tolerated(name: &'static str) -> Self {
        Self {
            name,
            schema: None,
            required: false,
        }
    }

    /// Says this operation refuses a call that leaves the argument out.
    ///
    /// note: optional by default, because most of these are. The ones that are not say so in
    /// their branch's `required`, rather than `edit` with no `old` being a well-formed call right
    /// up until it runs.
    pub(crate) fn needed(mut self) -> Self {
        self.required = true;
        self
    }

    fn of(name: &'static str, kind: &'static str, about: impl Into<String>) -> Self {
        Self {
            name,
            schema: Some(json!({ "type": kind, "description": about.into() })),
            required: false,
        }
    }
}

/// One *shape* a call may take: the operations that share it, and what they read.
///
/// note: several operations rather than one, because a branch per operation is only worth its
/// scaffolding where the operations differ. `context`'s four moves take one argument list,
/// because they are one function - so they are one branch under an `action` of four words, and
/// the arguments are written once rather than paid for four times on every request.
pub(crate) struct Op {
    actions: Vec<&'static str>,
    does: String,
    takes: Vec<Arg>,
}

impl Op {
    /// One operation, what it does, and what it reads.
    pub(crate) fn new(action: &'static str, does: impl Into<String>, takes: Vec<Arg>) -> Self {
        Self::these(&[action], does, takes)
    }

    /// Several operations that read the same arguments, as the one shape they are.
    pub(crate) fn these(
        actions: &[&'static str],
        does: impl Into<String>,
        takes: Vec<Arg>,
    ) -> Self {
        Self {
            actions: actions.to_vec(),
            does: does.into(),
            takes,
        }
    }
}

/// The `action` of every operation, in the order they were declared.
///
/// note: derived from the table rather than written down beside it. It is the `enum` a model
/// chooses from, the operation half of `<domain>:<operation>`, and the row `/limit` is keyed on -
/// three uses of one list, and a second copy of it would be a standing invitation for one to be
/// renamed.
pub(crate) fn actions(ops: &[Op]) -> Vec<&'static str> {
    ops.iter()
        .flat_map(|op| op.actions.iter().copied())
        .collect()
}

/// The schema a model is given: one property, and a branch of an `anyOf` per operation.
///
/// note: `enum` with one value rather than `const` for the discriminator. Both say the same thing
/// and OpenAI takes either, but Google's `Schema` is a closed set of fields with no `const` in it -
/// and one schema goes to both dialects, so the discriminator has to be spelled the way the
/// narrower of the two spells it.
///
/// note: nothing here is sent `strict`. What that would take, the day an endpoint is worth asking
/// for it: `additionalProperties: false` on the root and on every branch, every argument in its
/// branch's `required` with the optional ones typed `["string", "null"]`, and - because
/// `additionalProperties` is not a field of Google's `Schema` either - `gemini.rs` sending this
/// under `parametersJsonSchema` rather than `parameters`. The shape is already what strict wants;
/// it is the two keywords and the provider change that are not done.
pub(crate) fn schema(ops: &[Op]) -> Value {
    // note: no `anyOf` where there is nothing to choose between. A tool whose operations all read
    // the same arguments - `setup`, `log`, `shell` - has nothing for a union to gate, and wrapping
    // one branch in a one-element `anyOf` is scaffolding charged for on every request. The wrapper
    // itself stays either way, because a model should not have to remember which of these tools is
    // the exception: `call` is where the arguments go, for all of them
    let inside = match ops {
        [only] => {
            let mut one = branch(only);
            // note: the branch's own words where it has any, and the wrapper's where it has not,
            // so that a tool with one shape is never silent about what `call` is. Writing ABOUT
            // over the top unconditionally would throw away the description a lone branch carries,
            // such as `setup`'s account of its four operations
            if one["description"].is_null() {
                one["description"] = json!(ABOUT);
            }
            one
        }
        // note: `type` beside `anyOf`, though every branch already says `object` and a reader
        // that resolves the union learns it there. It is the property's own declaration that
        // decides what a model writes into it: without one, `xiaomi/mimo-v2.6-flash` writes the
        // arguments as a *string* of JSON - `{"call": "{\"action\": ...}"}` - on every call to a
        // tool with several branches, and on none to a tool whose single branch is the wrapper
        // and carries a `type` of its own. The two assertions cannot disagree - a branch is an
        // object either way - so this costs a keyword and settles a question the model should not
        // have had to guess at
        several => json!({
            "type": "object",
            "description": ABOUT,
            "anyOf": several.iter().map(branch).collect::<Vec<_>>(),
        }),
    };

    json!({
        "type": "object",
        "properties": { WRAPPER: inside },
        "required": [WRAPPER],
    })
}

/// One operation, as the branch a call has to match.
fn branch(op: &Op) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "action".to_owned(),
        json!({ "type": "string", "enum": op.actions }),
    );

    let mut required = vec![json!("action")];
    for arg in &op.takes {
        let Some(schema) = arg.schema.clone() else {
            continue;
        };
        properties.insert(arg.name.to_owned(), schema);
        if arg.required {
            required.push(json!(arg.name));
        }
    }

    let mut branch = json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    // left off where the tool's own description already says it, which is every tool that does one
    // thing: a branch called `run` under a tool called `shell` needs no sentence of its own
    if !op.does.is_empty() {
        branch["description"] = json!(op.does);
    }

    branch
}

/// The key a provider puts a call's arguments under when they would not parse as JSON.
///
/// note: `nachalnik-providers` plants it so that "a model that produces invalid JSON gets to see
/// that it did". Unread, the tool goes looking for `action`, does not find one, and answers
/// `the \`action\` argument is required` - about a call whose text holds an `action` and a brace
/// that was never closed. The model is sent to fix the wrong thing, when the one fact it needs is
/// one the provider already knew.
const UNPARSED: &str = "_unparsed";

/// How much of a payload to quote back: enough to see the fault in its sentence, and not so much
/// that a long command is copied into the context twice over.
const SHOWN: usize = 200;

/// What is wrong with arguments that never parsed, and the part of them to look at.
///
/// note: the parse is done again here because the provider throws the answer away.
/// `nachalnik-providers` tries it, keeps the text under [`UNPARSED`] when it fails, and drops the
/// error - so the one thing that knows *what* was wrong knows it in another crate. Reading it
/// again costs a parse of a string that is already known not to parse, which is the cheapest
/// thing in the exchange.
///
/// note: and the window is around the fault rather than the first [`SHOWN`] characters. A fault
/// past the cut is common, and a message quoting the start of the payload then quotes the part
/// that is fine and leaves out the part that is not - the failure `_unparsed` exists to end, one
/// level further out.
///
/// note: what it does not do is repair. `inclusionai/ling-3.0-flash-vl` ends every call but the
/// last of a multi-call turn one `}` short - reproducibly, streamed and whole alike, so it is the
/// model rather than anything in the way - and closing it here would be this program deciding what
/// a half-written call meant. Saying where it stops is the part that is known.
fn unreadable(written: &str) -> String {
    let Err(why) = serde_json::from_str::<Value>(written) else {
        return format!(
            "the arguments arrived as text rather than as an object, so nothing was read and \
             nothing was done. What arrived was `{}`. Send the call again, as one JSON object.",
            around(written, 0, SHOWN)
        );
    };

    format!(
        "the arguments were not JSON, so nothing was read and nothing was done: {why}. What \
         arrived there was `{}`. Send the call again, as one JSON object.",
        around(written, at(written, why.line(), why.column()), SHOWN)
    )
}

/// Where in the text a line and column land, both counted from one; the end of it when they name
/// nowhere, which is what a payload that simply stopped reports.
fn at(written: &str, line: usize, column: usize) -> usize {
    if line == 0 {
        return written.len();
    }

    let mut start = 0;
    for (n, text) in written.split_inclusive('\n').enumerate() {
        if n + 1 == line {
            return start
                + text
                    .char_indices()
                    .nth(column.saturating_sub(1))
                    .map_or(text.len(), |(at, _)| at);
        }
        start += text.len();
    }

    written.len()
}

/// A window of the text around a position, saying on which side there is more.
fn around(written: &str, at: usize, width: usize) -> String {
    let chars: Vec<(usize, char)> = written.char_indices().collect();
    let here = chars
        .iter()
        .position(|(byte, _)| *byte >= at)
        .unwrap_or(chars.len());

    let from = here.saturating_sub(width / 2).min(chars.len());
    let to = (from + width).min(chars.len());
    let from = to.saturating_sub(width);

    format!(
        "{}{}{}",
        if from > 0 { "…" } else { "" },
        chars[from..to].iter().map(|(_, it)| it).collect::<String>(),
        if to < chars.len() { "…" } else { "" },
    )
}

/// The object a call's arguments are really in, or what is wrong with where they are.
///
/// note: arguments under [`WRAPPER`] are what the schema asks for. Arguments flat are
/// unambiguous, so they are taken - a model that has learnt a flat shape loses nothing. Arguments
/// in both places are the one reading that cannot be charitable: picking either would drop the
/// other half, and a call that ignored an argument answers as though it had never been given one,
/// which is the failure [`unread`] exists for one step further in. And arguments that never parsed
/// are not arguments; see [`UNPARSED`].
///
/// note: a [`WRAPPER`] holding a *string* of JSON rather than an object is what some models
/// produce for every call they make - the whole session's worth, not the occasional one. What
/// invites it is a property that says `anyOf` and not what type it is, which [`schema`] fixes;
/// this is the backstop for a model that does it anyway, since nothing here can make a schema
/// binding. One that parses to an object is as unambiguous as the flat shape and is taken for the
/// same reason. One that does not is refused here, saying so, rather than falling through to
/// `the \`action\` argument is required` - which is the [`UNPARSED`] failure again: a model sent
/// to fix an argument it did write.
pub(crate) fn inner(args: &Value) -> Result<Cow<'_, Value>, String> {
    if let Some(written) = args.get(UNPARSED).and_then(Value::as_str) {
        return Err(unreadable(written));
    }

    let inside = match args.get(WRAPPER) {
        Some(it) if it.is_object() => Cow::Borrowed(it),
        Some(Value::String(written)) => match serde_json::from_str::<Value>(written) {
            Ok(parsed) if parsed.is_object() => Cow::Owned(parsed),
            _ => {
                return Err(format!(
                    "`{WRAPPER}` arrived as text rather than as an object, and nothing was read \
                     and nothing was done. What was in it was `{}`. Send the call again, with \
                     `{WRAPPER}` an object: `{{\"{WRAPPER}\": {{\"action\": ...}}}}`.",
                    written.chars().take(200).collect::<String>()
                ));
            }
        },
        _ => return Ok(Cow::Borrowed(args)),
    };

    let beside: Vec<&str> = args
        .as_object()
        .map(|it| it.keys().map(String::as_str).filter(|k| *k != WRAPPER))
        .into_iter()
        .flatten()
        .collect();
    if !beside.is_empty() {
        return Err(format!(
            "arguments inside `{WRAPPER}` and beside it ({}), and nothing was done. Every \
             argument goes inside `{WRAPPER}`, `action` included: reading one of the two places \
             would have answered a call nobody made.",
            beside
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Ok(inside)
}

/// The operation a call names, if it names one of these.
pub(crate) fn action_of(call: &ToolCall, ops: &[Op]) -> Option<String> {
    let named = inner(&call.args).ok()?.get("action")?.as_str()?.to_owned();
    actions(ops).contains(&named.as_str()).then_some(named)
}

/// Why a question is about everything a tool does, where the reason is that the call did not say
/// which operation it wanted.
///
/// note: [`action_of`] answering `None` is not a quiet fallback - it widens what the call declares
/// to every capability the tool has, which is the strictest reading of a call nobody can place and
/// the right one. What this adds is somebody saying so. A session started with `--allow fs:read`
/// is then asked about `fs:write` as well, the rule it was given matches nothing, and in a run
/// with nobody at the prompt the refusal reads `this call was refused when it was asked about` -
/// which sends a model looking for a different *approach* when what is wrong is the shape of the
/// call it just made.
///
/// note: only for a tool whose schema puts its arguments under [`WRAPPER`], because that is the
/// convention the sentence is about. Read off the tool's own schema rather than off a list of
/// names kept here: a tool that takes no `call` may declare several capabilities for reasons of
/// its own, and telling its caller that such a call named no operation would be inventing a
/// vocabulary it never claimed.
pub(crate) fn unnamed_operation(spec: &ToolSpec, request: &PermissionRequest) -> Option<String> {
    spec.schema["properties"].get(WRAPPER)?;

    let ops: Vec<&str> = spec
        .capabilities
        .iter()
        .map(|capability| capability.op.as_str())
        .collect();
    if ops.len() < 2 || request.capabilities != spec.capabilities {
        return None;
    }

    // the same reading the tool will do, so that a call this cannot place is one the tool could
    // not place either. Arguments that never parsed are their own answer and are given it there
    let named = inner(&request.args)
        .ok()
        .and_then(|args| args["action"].as_str().map(str::to_owned));
    let why = match named {
        Some(action) if ops.contains(&action.as_str()) => return None,
        Some(action) => format!(
            "`{action}` is not one of the {} operations `{}` has, so the call is judged against \
             all of them",
            ops.len(),
            spec.id
        ),
        None => format!(
            "the call names no operation, so it is judged against all {} `{}` has",
            ops.len(),
            spec.id
        ),
    };

    Some(format!(
        "{why} - a rule about one of them, like `{}:{}`, does not answer it",
        spec.capabilities[0].domain, ops[0]
    ))
}

/// What is wrong with the arguments a call gave, if anything: an argument the operation it named
/// does not read.
///
/// note: the same failure [`super::whole`] is about, one step earlier. An argument that is
/// silently ignored comes back as a *real answer* - the answer to the call without it - so nothing
/// in the reply says that what was asked for did not happen, and a read that was meant to be
/// narrowed arrives as the whole file looking like the thing that was asked for.
///
/// note: kept, though the schema now says the same thing. The schema is not sent `strict`, so it
/// is advice; this is the part that holds. The two cannot disagree, because they are one table.
pub(crate) fn unread(op: &str, args: &Value, ops: &[Op]) -> Option<String> {
    let mine = ops.iter().find(|it| it.actions.contains(&op))?;
    let given = args.as_object()?;
    let stray = given
        .keys()
        .map(String::as_str)
        .find(|key| *key != "action" && !mine.takes.iter().any(|arg| arg.name == *key))?;

    // note: named only when one operation has it, because the sentence is a *pointer* and there is
    // nowhere to point otherwise. `old` is `edit`'s and saying so is the whole answer; `ids` is
    // several of `context`'s, and "that one is `look`'s" - the first row that has it - is
    // a fact about this table's order being read as a fact about the argument. A model that has
    // just been told its call was wrong is in no position to discount what it is told next
    let others: Vec<&str> = ops
        .iter()
        .filter(|it| !it.actions.contains(&op) && it.takes.iter().any(|arg| arg.name == stray))
        .flat_map(|it| it.actions.iter().copied())
        .collect();
    let whose = match others[..] {
        [only] => format!(" - that one is `{only}`'s"),
        _ => String::new(),
    };

    let offers: Vec<String> = mine
        .takes
        .iter()
        .filter(|arg| arg.schema.is_some())
        .map(|arg| format!("`{}`", arg.name))
        .collect();

    Some(format!(
        "`{op}` does not take `{stray}`{whose}. It takes {}, and nothing was done: a call that \
         ignored an argument would have answered as if you had never given it.",
        // note: what it *offers*, so a tolerated argument is not named here. `label` is read by
        // the four that move an item only so that a call giving one can be told what it meant,
        // and listing it as one of the arguments they take is the advertisement `Arg::tolerated`
        // exists to withhold - printed, of all places, in the refusal correcting that mistake
        match &offers[..] {
            [] => "no arguments beside `action`".to_owned(),
            offers => offers.join(", "),
        }
    ))
}

/// The operations a built schema offers, in the order it offers them.
///
/// note: for the test every one of these tools keeps - that the vocabulary the schema is written
/// in and the vocabulary the permissions are written in are one list. Both come off the same
/// table, so this guards against that ceasing to be true.
#[cfg(test)]
pub(crate) fn offered(schema: &Value) -> Vec<&str> {
    let inside = &schema["properties"][WRAPPER];
    let branches = match inside["anyOf"].as_array() {
        Some(several) => several.iter().collect::<Vec<_>>(),
        // a tool with one shape carries it directly; see `schema`
        None => vec![inside],
    };

    branches
        .into_iter()
        .flat_map(|branch| {
            branch["properties"]["action"]["enum"]
                .as_array()
                .expect("a branch names the actions it covers")
                .iter()
                .map(|it| it.as_str().expect("an action is a word"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use nachalnik::Capability;

    use super::*;

    fn ops() -> Vec<Op> {
        vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed()],
            ),
            Op::new(
                "grep",
                "searches",
                vec![
                    Arg::text("pattern", "for what").needed(),
                    Arg::text("path", "where"),
                    Arg::truth("ignore_case", "case"),
                ],
            ),
            Op::new("list", "", vec![]),
        ]
    }

    /// Every branch is its own operation, and carries its own arguments and nobody else's.
    ///
    /// note: a flat schema would have `read` and `grep` share one property bag, with only a
    /// description saying `ignore_case` is not `read`'s. The branch says it, so this pins the
    /// branch.
    #[test]
    fn an_operation_declares_its_own_arguments() {
        let schema = schema(&ops());
        let branches = schema["properties"][WRAPPER]["anyOf"]
            .as_array()
            .expect("a branch per operation")
            .clone();
        assert_eq!(branches.len(), 3);

        let read = &branches[0];
        assert_eq!(read["properties"]["action"]["enum"], json!(["read"]));
        assert_eq!(read["required"], json!(["action", "path"]));
        assert!(
            read["properties"]["ignore_case"].is_null(),
            "`ignore_case` is `grep`'s and `read` should not offer it"
        );

        let grep = &branches[1];
        assert_eq!(
            grep["required"],
            json!(["action", "pattern"]),
            "the optional ones stay optional"
        );
        assert!(grep["properties"]["ignore_case"].is_object());

        // an operation that does nothing but be named still says so
        assert_eq!(branches[2]["required"], json!(["action"]));
        assert!(
            branches[2]["description"].is_null(),
            "an empty `does` is left off rather than sent empty"
        );
    }

    /// Every keyword the generated schema uses is one both dialects accept.
    ///
    /// note: the narrower of the two is Google's, whose `Schema` is a closed set of fields rather
    /// than a JSON Schema document - the list below is that set, from the `generativelanguage`
    /// protobuf, intersected with what OpenAI documents as supported. One schema goes to both, so
    /// a keyword outside it is a 400 from one endpoint and silence from the other. `const`,
    /// `additionalProperties`, `$ref`, `$defs`, `oneOf` and `allOf` are the ones worth knowing are
    /// absent: the first two are what `strict` would need, and neither is a field Google has.
    #[test]
    fn the_schema_is_written_in_the_words_both_dialects_know() {
        const BOTH: [&str; 22] = [
            "type",
            "format",
            "title",
            "description",
            "nullable",
            "enum",
            "items",
            "maxItems",
            "minItems",
            "properties",
            "required",
            "minProperties",
            "maxProperties",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "pattern",
            "example",
            "anyOf",
            "propertyOrdering",
            "default",
        ];

        fn walk(node: &Value, at: &str, keywords: &mut Vec<String>) {
            match node {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        // the keys of `properties` are argument names, not keywords
                        match at {
                            "properties" => walk(value, key, keywords),
                            _ => {
                                keywords.push(key.clone());
                                walk(value, key, keywords);
                            }
                        }
                    }
                }
                Value::Array(items) => items.iter().for_each(|it| walk(it, at, keywords)),
                _ => {}
            }
        }

        let mut keywords = Vec::new();
        walk(&schema(&ops()), "", &mut keywords);
        assert!(!keywords.is_empty(), "nothing was walked");

        for keyword in &keywords {
            assert!(
                BOTH.contains(&keyword.as_str()),
                "`{keyword}` is not a keyword both dialects take"
            );
        }
    }

    /// The wrapper says it is an object, whether it holds one shape or several.
    ///
    /// note: the case that matters is the `anyOf` one: without a `type` there, a model can write
    /// the whole call as a string into a property that never said what it was. Both arms are
    /// asserted because the single-branch arm gets its `type` from `branch` by accident of
    /// construction rather than on purpose, and an accident is a thing to pin.
    #[test]
    fn the_wrapper_says_it_is_an_object() {
        let several = schema(&ops());
        assert_eq!(several["properties"][WRAPPER]["type"], json!("object"));
        assert!(
            several["properties"][WRAPPER]["anyOf"].is_array(),
            "the case this is about is the union"
        );

        let one = schema(&[Op::new(
            "run",
            "runs it",
            vec![Arg::text("cmd", "what").needed()],
        )]);
        assert_eq!(one["properties"][WRAPPER]["type"], json!("object"));
    }

    /// A call nobody can place is one the question says so about, and a call that names its
    /// operation is left alone.
    ///
    /// note: the cases it stays silent about are the ones worth keeping. A sentence on every
    /// question is a sentence nobody reads, and this one is only true of a call that did not say
    /// which operation it wanted - which a wrapper written as text has, since [`inner`] reads
    /// through one. What is left is arguments that name nothing and arguments that never parsed.
    #[test]
    fn a_call_that_names_no_operation_says_so() {
        let ops = ops();
        let spec = ToolSpec::new("fs", "the filesystem")
            .with_schema(std::sync::Arc::new(schema(&ops)))
            .with_capabilities(
                actions(&ops)
                    .into_iter()
                    .map(Capability::fs)
                    .collect::<Vec<_>>(),
            );
        let asking = |args| PermissionRequest {
            id: nachalnik::PermissionId(1),
            call: nachalnik::ToolCallId("c1".to_owned()),
            tool: "fs".to_owned(),
            capabilities: spec.capabilities.clone(),
            args: std::sync::Arc::new(args),
        };

        assert_eq!(
            unnamed_operation(
                &spec,
                &asking(json!({ WRAPPER: { "action": "read", "path": "x" } }))
            ),
            None,
            "a call that placed itself is judged by one capability and needs no sentence"
        );

        assert_eq!(
            unnamed_operation(&spec, &asking(json!({ WRAPPER: "{\"action\": \"read\"}" }))),
            None,
            "a wrapper written as text is read through, so the call it holds placed itself"
        );

        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: { "path": "x" } })))
            .expect("arguments that name no operation are a call nobody can place");
        assert!(said.contains("names no operation"), "{said}");
        assert!(
            said.contains("`fs:read`"),
            "and says what a rule that would have answered looks like: {said}"
        );

        let said = unnamed_operation(
            &spec,
            &asking(json!({ UNPARSED: "{\"call\": {\"action\"" })),
        )
        .expect("arguments that never parsed name nothing either");
        assert!(said.contains("names no operation"), "{said}");

        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: { "action": "fly" } })))
            .expect("`fly` is not one of these");
        assert!(said.contains("`fly` is not one of the"), "{said}");

        // one operation cannot be widened to, and a tool that takes no wrapper is not this
        // convention, so neither gets a sentence about it
        let one = ToolSpec::new("shell", "runs things")
            .with_schema(std::sync::Arc::new(schema(&[Op::new(
                "run",
                "runs it",
                vec![],
            )])))
            .with_capabilities(vec![Capability::exec("run")]);
        assert_eq!(unnamed_operation(&one, &asking(json!({}))), None);

        let flat = ToolSpec::new("fs", "the filesystem")
            .with_schema(std::sync::Arc::new(json!({ "type": "object" })))
            .with_capabilities(spec.capabilities.clone());
        assert_eq!(unnamed_operation(&flat, &asking(json!({}))), None);
    }

    /// The wrapper is asked for, understood without, and refused in both places at once.
    #[test]
    fn arguments_are_read_wherever_a_model_put_them() {
        let wrapped = json!({ WRAPPER: { "action": "read", "path": "x" } });
        assert_eq!(inner(&wrapped).expect("wrapped")["path"], json!("x"));

        let flat = json!({ "action": "read", "path": "x" });
        assert_eq!(inner(&flat).expect("flat")["path"], json!("x"));

        let both = json!({ WRAPPER: { "action": "read" }, "path": "x" });
        let refusal = inner(&both).expect_err("arguments in two places is not a call");
        assert!(refusal.contains("`path`"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
    }

    /// A wrapper holding a string of JSON is read, and one holding anything else is refused as
    /// that rather than as an argument nobody gave.
    ///
    /// note: some models write a nested object as a string, and do it on every call. Unread,
    /// `action` is not found, the call declares every operation the tool has, and a session
    /// granted `fs:read` cannot read a file.
    #[test]
    fn a_wrapper_written_as_text_is_still_a_call() {
        let written = json!({ WRAPPER: "{\"action\": \"read\", \"path\": \"x\"}" });
        assert_eq!(inner(&written).expect("written")["path"], json!("x"));

        let neither = json!({ WRAPPER: "read the file" });
        let refusal = inner(&neither).expect_err("that is not a call");
        assert!(refusal.contains("as text"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
        assert!(refusal.contains("read the file"), "{refusal}");
    }

    /// Arguments that never parsed are answered as that, not as an argument nobody gave.
    ///
    /// note: the text is a JSON string closed with an XML tag, the way a model can end one. The
    /// provider hands it over under `_unparsed` precisely so it can be reported, and read as
    /// missing arguments the report would be `the \`action\` argument is required`, about a call
    /// whose text says `"action": "note"` near its start.
    #[test]
    fn arguments_that_never_parsed_are_not_a_missing_argument() {
        let broken = json!({
            UNPARSED: "{\"call\": {\"action\": \"note\", \"content</arg_key>\nthe project is",
        });

        let refusal = inner(&broken).expect_err("this is not a call");
        assert!(refusal.contains("not JSON"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
        assert!(
            refusal.contains("</arg_key>"),
            "and it says what arrived, because that is the part to look at: {refusal}"
        );
    }

    /// A payload that never parsed says what is wrong with it and shows that part.
    ///
    /// note: both texts are shapes `inclusionai/ling-3.0-flash-vl` produces. The first is what
    /// that model sends for every call but the last of a multi-call turn: complete, and one `}`
    /// short. The second is an unescaped `"` inside the command, past the two hundredth
    /// character, which is why the window follows the fault.
    #[test]
    fn a_payload_that_never_parsed_says_where_it_stops() {
        let short = "{\"call\": {\"action\": \"run\", \"cmd\": \"curl -sL \
                     'https://api.github.com/repos/ljedrz/nac' | python3 -m json.tool\"}";
        let said = unreadable(short);
        assert!(said.contains("not JSON"), "{said}");
        assert!(said.contains("nothing was done"), "{said}");
        assert!(
            said.contains("EOF") || said.contains("end"),
            "a payload that simply stops says so, rather than leaving the model to count \
             braces: {said}"
        );
        assert!(
            said.contains("json.tool"),
            "and shows where it stopped: {said}"
        );

        let escaped = format!(
            "{{\"call\": {{\"action\": \"run\", \"cmd\": \"curl -sL '{}' 2>/dev/null \
             | python3 -c \"import sys,json; print(json.load(sys.stdin))\"}}}}",
            "https://api.github.com/repos/contributor-covenant/contributor-covenant/git/trees/\
             main?recursive=1&per_page=100&page=1&ref=refs/heads/main&filter=blobs"
        );
        assert!(
            escaped
                .find("python3 -c \"import")
                .expect("the fault is in there")
                > SHOWN,
            "the case is a fault past the first {SHOWN} characters"
        );
        let said = unreadable(&escaped);
        assert!(
            said.contains("import sys,json"),
            "the window follows the fault rather than quoting the start: {said}"
        );
        assert!(
            said.contains('…'),
            "and says there is more either side: {said}"
        );
    }

    /// A stray argument is refused by name, and told whose it is where exactly one owns it.
    #[test]
    fn an_argument_the_operation_does_not_read_is_refused() {
        let ops = ops();
        assert!(unread("read", &json!({ "action": "read", "path": "x" }), &ops).is_none());

        let refusal = unread(
            "read",
            &json!({ "action": "read", "ignore_case": true }),
            &ops,
        )
        .expect("`ignore_case` is not `read`'s");
        assert!(refusal.contains("that one is `grep`'s"), "{refusal}");

        // `path` is two operations', so there is nowhere to point
        let refusal = unread("list", &json!({ "action": "list", "path": "x" }), &ops)
            .expect("`list` takes none");
        assert!(!refusal.contains("that one is"), "{refusal}");
        assert!(
            refusal.contains("no arguments beside `action`"),
            "{refusal}"
        );
    }

    /// The operation is found under the wrapper and without it, and only when it is one of ours.
    #[test]
    fn the_operation_is_the_one_the_call_names() {
        let ops = ops();
        let call = |args| ToolCall::new("1", "t", args);

        assert_eq!(
            action_of(&call(json!({ WRAPPER: { "action": "grep" } })), &ops).as_deref(),
            Some("grep")
        );
        assert_eq!(
            action_of(&call(json!({ "action": "grep" })), &ops).as_deref(),
            Some("grep")
        );
        assert_eq!(action_of(&call(json!({ "action": "fly" })), &ops), None);
        assert_eq!(action_of(&call(json!({})), &ops), None);
        assert_eq!(
            action_of(&call(json!({ WRAPPER: "{\"action\": \"grep\"}" })), &ops).as_deref(),
            Some("grep")
        );
    }
}
