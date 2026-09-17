//! What a tool that does several things declares: one table of operations, and the schema, the
//! refusal and the vocabulary all made out of it.
//!
//! note: it was three declarations held together by a test. `OPS` was the `action` enum a model
//! chooses from, `TAKES` was what each of those reads, and the schema was a flat bag of every
//! argument any of them takes - with each description opening `for \`grep\`:` to say which rows it
//! belonged to. The three could not drift, because a unit test in each file compared them. What no
//! test could fix is that the *model* was only ever shown the flat one: `{"action": "read", "old":
//! "x"}` is valid against it, gets as far as `invoke`, and is refused there. A round trip spent
//! telling a model what the schema could have told it for nothing.
//!
//! note: so the table is the declaration and the schema is made from it. An operation is a branch
//! of an `anyOf`, carrying its own arguments and its own `required`, which is what makes the call
//! above unrepresentable rather than merely refused. [`unread`] stays as the backstop, reading the
//! same table: nothing is sent `strict`, so the schema guides rather than binds, and a model that
//! ignores it is still answered by name.

use nachalnik::ToolCall;
use serde_json::{Map, Value, json};

/// The one property every one of these tools takes, with the operation inside it.
///
/// note: a wrapper because the root of a schema may not itself be an `anyOf` - OpenAI says so
/// outright ("the root level object of a schema must be an object, and not use `anyOf`") and it is
/// the one rule that decides the shape here. A union has to live under a property, so there is a
/// property. Everything else about the call is unchanged: `action` names the operation, inside.
///
/// note: read forgivingly. A model that passes the arguments flat, the way the old schema asked
/// for them, is understood rather than refused - the call is unambiguous and refusing it costs a
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
    /// note: for the one case that is neither. `context`'s five moves read `label` so that a call
    /// giving one instead of `ids` can be answered with the spelling it meant - a live run reached
    /// for it that way, and `Amend::moved` says `select: "label:<text>"` back. It is not a way to
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
    /// note: optional by default, because most of these are. What it buys over the old schema is
    /// that the ones which are not can finally say so: `required` was `["action"]` on five of the
    /// six tools, so `edit` with no `old` was a well-formed call right up until it ran.
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
/// scaffolding where the operations differ. `context`'s five moves take one argument list - which
/// the `MOVES` constant they replace said outright, "one list because they are one function" - so
/// they are one branch under an `action` of five words, and the arguments are written once. Five
/// copies of `reason` cost 728 bytes on every request to say a thing that was already true.
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
/// note: what `OPS` was, derived rather than written down beside the table that repeats it. It is
/// the `enum` a model chooses from, the operation half of `<domain>:<operation>`, and the row
/// `/limit` is keyed on - three uses of one list, which is why having two of it was a standing
/// invitation for one to be renamed.
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
            // over the top unconditionally is what this used to do, and it threw away `setup`'s
            // account of its four operations - the only description that lived on a lone branch
            if one["description"].is_null() {
                one["description"] = json!(ABOUT);
            }
            one
        }
        several => json!({
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

/// The object a call's arguments are really in, or what is wrong with where they are.
///
/// note: three readings, and the third is the only one refused. Arguments under [`WRAPPER`] is
/// what the schema asks for. Arguments flat is what the schema used to ask for, and it is
/// unambiguous, so it is taken - a model that has learnt the old shape loses nothing. Arguments in
/// both places is the one that cannot be read charitably: picking either would drop the other
/// half, and a call that ignored an argument answers as though it had never been given one, which
/// is the failure [`unread`] exists for one step further in.
pub(crate) fn inner(args: &Value) -> Result<&Value, String> {
    let Some(inside) = args.get(WRAPPER).filter(|it| it.is_object()) else {
        return Ok(args);
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
pub(crate) fn action_of<'a>(call: &'a ToolCall, ops: &[Op]) -> Option<&'a str> {
    inner(&call.args)
        .ok()?
        .get("action")?
        .as_str()
        .filter(|named| actions(ops).contains(named))
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
/// is advice; this is the part that holds. What changed is that the two cannot disagree any more -
/// they are one table - which is what the per-file unit tests were doing by hand.
pub(crate) fn unread(op: &str, args: &Value, ops: &[Op]) -> Option<String> {
    let mine = ops.iter().find(|it| it.actions.contains(&op))?;
    let given = args.as_object()?;
    let stray = given
        .keys()
        .map(String::as_str)
        .find(|key| *key != "action" && !mine.takes.iter().any(|arg| arg.name == *key))?;

    // note: named only when one operation has it, because the sentence is a *pointer* and there is
    // nowhere to point otherwise. `old` is `edit`'s and saying so is the whole answer; `ids` is
    // eleven of `context`'s thirteen, and "that one is `look`'s" - the first row that has it - is
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
        // the five that move an item only so that a call giving one can be told what it meant,
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
/// in and the vocabulary the permissions are written in are one list. They come off the same table
/// now, so this is a regression guard rather than the load-bearing check it replaced: what each
/// tool used to assert was that two hand-written lists agreed, and there are no longer two.
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
    /// note: this is the whole of what the change bought, so it is the thing to pin. Under the old
    /// schema `read` and `grep` shared one property bag, and the only thing saying `ignore_case`
    /// was not `read`'s was the word "for `grep`:" at the front of its description.
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
            action_of(&call(json!({ WRAPPER: { "action": "grep" } })), &ops),
            Some("grep")
        );
        assert_eq!(
            action_of(&call(json!({ "action": "grep" })), &ops),
            Some("grep")
        );
        assert_eq!(action_of(&call(json!({ "action": "fly" })), &ops), None);
        assert_eq!(action_of(&call(json!({})), &ops), None);
    }
}
