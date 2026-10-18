//! What a tool that does several things declares: one table of operations, and the schema, the
//! refusal and the vocabulary all made out of it.
//!
//! note: one table instead of three declarations held together by a test: the `action` enum a model
//! chooses from, what each reads, and a flat schema of every argument any of them takes. A test
//! cannot stop the model seeing only the flat one: `{"action": "read", "old": "x"}` is valid
//! against it, reaches `invoke`, and is refused there, a round trip the schema could have avoided.
//!
//! note: so the table is the declaration and the schema is made from it. An operation is a branch
//! of an `anyOf` with its own arguments and `required`, which makes the call above unrepresentable
//! rather than merely refused. [`unread`] is the backstop, reading the same table: nothing is sent
//! `strict`, so the schema guides rather than binds, and a model that ignores it is still answered
//! by name.

use std::borrow::Cow;

use nachalnik::{PermissionRequest, ToolCall, ToolSpec};
use serde_json::{Map, Value, json};

/// Every one of these tools takes one property, `call`, with the operation inside it.
///
/// note: a wrapper because the root of a schema may not itself be an `anyOf`. OpenAI says so ("the
/// root level object of a schema must be an object, and not use `anyOf`"), and it is the one rule
/// that decides the shape here: a union must live under a property, so there is a property.
/// `action` names the operation inside.
///
/// note: a model that passes the arguments flat is understood rather than refused: the call is
/// unambiguous and refusing it costs a turn. Arguments in both places at once are refused, since
/// any reading of them could silently do something nobody asked for. See [`inner`].
///
/// note: a session can be offered no wrapper at all, with [`Layout::Flat`]. A model whose call
/// format is a flat list of keys and values can start `call` as a JSON object and finish it in its
/// own markup, losing the call. Flat tools give it nothing nested to slip inside. That loses the
/// per-operation `required` and the branch that keeps one operation's arguments away from
/// another's, so it is a setting for the models that need it, not the default.
pub(crate) const WRAPPER: &str = "call";

/// How this program's own tools lay out arguments for a model: inside one `call` object, or at the
/// top level. `fs`, `shell` and the four session-reading tools are built either way; an MCP tool is
/// offered as its server wrote it.
///
/// note: the schema carries it once built - a tool's schema has a `call` property in one layout and
/// not the other - so a tool holding its schema knows its layout, and so does the policy, which is
/// handed every tool's schema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layout {
    /// Every argument inside one `call` object, with a branch of an `anyOf` per operation.
    #[default]
    Wrapped,
    /// Every argument at the top level, beside an `action` that names the operation.
    Flat,
}

impl Layout {
    /// The layout a schema made by this module offers.
    pub(crate) fn of(schema: &Value) -> Self {
        match schema["properties"].get(WRAPPER) {
            Some(_) => Self::Wrapped,
            None => Self::Flat,
        }
    }
}

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
    /// note: `context`'s four moves read `label` so a call giving `label` instead of `ids` can be
    /// answered with the spelling it meant - a model may reach for it that way, and
    /// `Changes::moved` says `select: "label:<text>"` back. It is not a way to name the items to
    /// move, so advertising it would teach the mistake the answer corrects; leaving it out of the
    /// table would have [`unread`] refuse the call before that answer could be given.
    pub(crate) fn tolerated(name: &'static str) -> Self {
        Self {
            name,
            schema: None,
            required: false,
        }
    }

    /// Says this operation refuses a call that leaves the argument out.
    ///
    /// note: optional by default, because most of these are. The ones that are not say so in their
    /// branch's `required`, rather than letting `edit` with no `old` be well-formed right up until
    /// it runs.
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

/// One shape a call may take: the operations that share it, and what they read.
///
/// note: several operations rather than one, because a branch per operation is only worth its
/// scaffolding where the operations differ. `context`'s four moves take one argument list because
/// they are one function, so they are one branch under an `action` of four words, and the arguments
/// are written once rather than four times on every request.
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

/// The `action` of every operation, in declaration order.
///
/// note: derived from the table rather than written beside it. It is the `enum` a model chooses
/// from, the operation half of `<domain>:<operation>`, and the row `/limit` is keyed on - three
/// uses of one list; a second copy risks one being renamed.
pub(crate) fn actions(ops: &[Op]) -> Vec<&'static str> {
    ops.iter()
        .flat_map(|op| op.actions.iter().copied())
        .collect()
}

/// The schema a model is given: one property, and a branch of an `anyOf` per operation - or, in
/// [`Layout::Flat`], every argument at the top level; see [`flat`].
///
/// note: `enum` with one value rather than `const` for the discriminator. Both say the same thing,
/// and Google's `Schema` - a closed set of fields with no `const` - takes `enum`. One schema goes
/// to every dialect, so the discriminator is spelled the way the narrowest spells it.
///
/// note: nothing here is sent `strict`. To do so: `additionalProperties: false` on the root and
/// every branch, and every argument in its branch's `required` with optional ones typed
/// `["string", "null"]`. The shape is already what strict wants; it is the two keywords that are
/// not done.
pub(crate) fn schema(ops: &[Op], layout: Layout) -> Value {
    if layout == Layout::Flat {
        return flat(ops);
    }

    // note: no `anyOf` where there is nothing to choose between. A tool whose operations all read
    // the same arguments - `setup`, `log`, `shell` - has nothing for a union to gate, and wrapping
    // one branch in a one-element `anyOf` is clutter charged for on every request. The wrapper
    // stays either way because a model should not have to remember which tool is the exception:
    // `call` is where the arguments go, for all of them
    let inside = match ops {
        [only] => {
            let mut one = branch(only);
            // note: the branch's own words where it has any, and the wrapper's where it has not, so
            // a tool with one shape is never silent about what `call` is. Unconditionally writing
            // ABOUT over the top would throw away the description a lone branch carries
            if one["description"].is_null() {
                one["description"] = json!(ABOUT);
            }
            one
        }
        // note: no `type` beside `anyOf`; every branch says `object` already. With one there,
        // `nex-agi/nex-n2.5-mini` and `nvidia/nemotron-3-super-120b-a12b` write the operation's
        // name into the wrapper - `{"call": "read", "path": ...}` - naming no operation and judged
        // against all of them. Without it, `xiaomi/mimo-v2.6-flash` writes the call as a string of
        // JSON, and `inner` reads that
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

/// The table as one object: `action` names the operation, and every argument any operation takes
/// beside it, once.
///
/// note: `action`'s description says what each operation does. An argument taken by only some
/// operations says which. An argument several take with different descriptions keeps each one,
/// named by the operations it belongs to. Otherwise `path` reads as "the file to read" for `glob`
/// too, and a `revise`-only argument reads as one `look` takes.
///
/// note: only an argument every operation requires goes in `required`. One some require says so in
/// its description; the schema has nowhere else to say it. [`unread`] and the tool's own
/// missing-argument check hold the call either way.
///
/// note: when several operations give one argument different schemas, it keeps the keywords they
/// all agree on. `revise`'s `ids` is a list of exactly one, and the others' are lists of any
/// length: merged into one property, that is a list, and `revise`'s description says it takes one.
fn flat(ops: &[Op]) -> Value {
    let every = actions(ops);
    let mut action = json!({ "type": "string", "enum": every });
    let does: Vec<String> = ops
        .iter()
        .filter(|op| !op.does.is_empty())
        .map(|op| format!("{}: {}", named(&op.actions), op.does))
        .collect();
    if !does.is_empty() {
        action["description"] = json!(sentences(&does));
    }

    let mut properties = Map::new();
    properties.insert("action".to_owned(), action);
    let mut required = vec![json!("action")];

    let mut names: Vec<&str> = Vec::new();
    for arg in ops.iter().flat_map(|op| &op.takes) {
        if arg.schema.is_some() && !names.contains(&arg.name) {
            names.push(arg.name);
        }
    }

    for name in names {
        // the operations that take it, gathered by what each says about it
        let mut takers: Vec<(&Value, bool, Vec<&str>)> = Vec::new();
        for op in ops {
            for arg in op.takes.iter().filter(|arg| arg.name == name) {
                let Some(schema) = &arg.schema else {
                    continue;
                };
                match takers
                    .iter_mut()
                    .find(|(said, needed, _)| *said == schema && *needed == arg.required)
                {
                    Some((_, _, actions)) => actions.extend(&op.actions),
                    None => takers.push((schema, arg.required, op.actions.clone())),
                }
            }
        }

        if let [(schema, needed, actions)] = &takers[..]
            && actions.len() == every.len()
        {
            properties.insert(name.to_owned(), (*schema).clone());
            if *needed {
                required.push(json!(name));
            }
            continue;
        }

        let mut merged: Map<String, Value> = takers[0]
            .0
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, value)| {
                *key != "description"
                    && takers
                        .iter()
                        .all(|(schema, _, _)| schema.get(key.as_str()) == Some(value))
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let said: Vec<String> = takers
            .iter()
            .map(|(schema, needed, actions)| {
                format!(
                    "{}{}: {}",
                    named(actions),
                    if *needed { " (required)" } else { "" },
                    schema["description"].as_str().unwrap_or_default()
                )
            })
            .collect();
        merged.insert("description".to_owned(), json!(sentences(&said)));
        properties.insert(name.to_owned(), Value::Object(merged));
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Operations by name, as a description lists them: `` `read`, `write` ``.
fn named(actions: &[&str]) -> String {
    actions
        .iter()
        .map(|action| format!("`{action}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Several clauses as one description, a full stop between each.
fn sentences(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim_end_matches('.'))
        .collect::<Vec<_>>()
        .join(". ")
}

/// Providers once put a call's arguments under this key when they would not parse as JSON.
///
/// note: they keep that text on [`nachalnik::ToolCall::unparsed`] now, the kernel answers the call,
/// and a call recorded the old way is read back that way, so this key reaches a tool only where a
/// model wrote it. A session whose history shows calls in that shape copies the shape, and goes on
/// copying its own calls once one was read. If unread, the tool looks for `action`, does not find
/// one, and answers `the \`action\` argument is required` about a call whose text holds an
/// `action`.
const UNPARSED: &str = "_unparsed";

/// The text of arguments that never parsed, where that is what is under [`UNPARSED`].
///
/// note: only where the text does not read as an object on its own. A model copying the shape
/// writes the call it meant, whole and valid, inside the wrapper. Answered as not JSON, it is
/// copied again on every call after, and the session makes no call that works, so text there that
/// parses is the model's wrapper round a call, and the call inside it is read.
fn unparsed(args: &Value) -> Option<&str> {
    let written = args.get(UNPARSED)?.as_str()?;
    let alone = args.as_object().is_some_and(|entries| entries.len() == 1);
    match serde_json::from_str::<Value>(written) {
        Ok(Value::Object(_)) if alone => None,
        _ => Some(written),
    }
}

/// How much of a payload to quote back: enough to see the fault in its sentence, and not so much
/// that a long command is copied into the context twice over.
const SHOWN: usize = 200;

/// What is wrong with arguments that never parsed, and the part of them to look at.
///
/// note: for text a model put under [`UNPARSED`] itself; what a provider could not parse is
/// answered by the kernel, with [`advice`] added. The parse is done again here because nothing
/// handed over the error, and it costs a parse of a string already known not to parse.
///
/// note: the window is around the fault rather than the first [`SHOWN`] characters. A fault past
/// the cut is common, and a message quoting the start of the payload would quote the part that is
/// fine and leave out the part that is not.
///
/// note: it does not repair. `inclusionai/ling-3.0-flash-vl` ends every call but the last of a
/// multi-call turn one `}` short - reproducibly, streamed and whole alike, so it is the model
/// rather than anything in the way - and closing it here would be this program deciding what a
/// half-written call meant. Saying where it stops is the part that is known.
fn unreadable(written: &str, layout: Layout) -> String {
    let Err(why) = serde_json::from_str::<Value>(written) else {
        return format!(
            "the arguments arrived as text rather than as an object, so nothing was read and \
             nothing was done. What arrived was `{}`. Send the call again, as one JSON object.",
            around(written, 0, SHOWN)
        );
    };

    format!(
        "the arguments were not JSON, so nothing was read and nothing was done: {why}. What \
         arrived there was `{}`. Send the call again, as one JSON object{}.",
        around(written, at(written, why.line(), why.column()), SHOWN),
        remedy(written, &why, layout)
    )
}

/// What to write instead, where a model makes a fault without seeing it: a regex's `\(` put into a
/// string as it stands, a line break typed into one, or the action written bare where the wrapper's
/// object goes.
///
/// note: the line and column say where, but that was not enough. A model that writes `\(` in a
/// pattern reads it as the pattern it meant, sends it again unchanged and is told the same thing;
/// it lacks the rule, which is one clause. The bare action is the commonest of the three:
/// `{"call": glob, ...}`, often with the model's own markup after it, and the clause shows the
/// shape with the word it wrote.
fn remedy(written: &str, why: &serde_json::Error, layout: Layout) -> String {
    let said = why.to_string();
    if said.starts_with("invalid escape") {
        return ", with every backslash inside a string written twice, as `\\\\`".to_owned();
    }
    if said.starts_with("control character") {
        return ", with a line break inside a string written as `\\n`".to_owned();
    }

    let bare = written
        .strip_prefix(&format!("{{\"{WRAPPER}\":"))
        .map(str::trim_start)
        .map(|rest| {
            rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap_or_default()
        })
        .filter(|word| word.starts_with(|c: char| c.is_ascii_alphabetic()));
    match (bare, layout) {
        (Some(word), Layout::Flat) => format!(
            ", with every argument at the top level and the action named by `action`, as in \
             `{{\"action\": \"{word}\", ...}}`"
        ),
        (Some(word), _) => format!(
            ", with `{WRAPPER}` holding an object that names the action, as in \
             `{{\"{WRAPPER}\": {{\"action\": \"{word}\", ...}}}}`"
        ),
        (None, _) => String::new(),
    }
}

/// What a tool here adds to the kernel's answer to a call whose arguments did not parse: how to
/// write it, where the fault is one a model makes without seeing it; see
/// [`nachalnik::Tool::unparsed`].
///
/// note: the kernel says what the parser said, where, and what arrived there. It cannot know that a
/// word right after `{"call":` is the action written bare where the object goes, or that a line
/// break typed into a string is written `\n`, which a model told only that the JSON was invalid can
/// send again unchanged.
///
/// note: nothing for an `invalid escape`, which is a regex's `\(` put into a string as it stands.
/// Measured on three models, a sentence about writing the backslash twice made retries worse than
/// the kernel's answer alone, because a model told to double a backslash doubled it in the value as
/// well as in the JSON.
pub(crate) fn advice(written: &str, layout: Layout) -> Option<String> {
    let why = serde_json::from_str::<Value>(written).err()?;
    if why.to_string().starts_with("invalid escape") {
        return None;
    }
    let said = remedy(written, &why, layout);
    let how = said.strip_prefix(", ")?;

    Some(format!("Write it {how}."))
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
/// note: arguments under [`WRAPPER`] are what the schema asks for. Flat arguments are unambiguous,
/// so they are taken; a model that has learnt a flat shape loses nothing. Arguments in both places
/// cannot be read charitably: picking either drops the other half, and a call that ignored an
/// argument answers as though it had never been given one, the failure [`unread`] exists for, one
/// step further in. Arguments that never parsed are not arguments; see [`UNPARSED`].
///
/// note: a [`WRAPPER`] holding a string of JSON rather than an object is what some models produce
/// for every call. It is invited by a property that says `anyOf` and not what type it is, which
/// [`schema`] fixes; this is the backstop for a model that does it anyway, since nothing here can
/// make a schema binding. One that parses to an object is as unambiguous as the flat shape and is
/// taken for the same reason. One that does not is refused here, saying so, rather than falling
/// through to `the \`action\` argument is required`, which is the [`UNPARSED`] failure: a model
/// sent to fix an argument it did write.
///
/// note: what is wrong comes back as a [`Misread`] rather than a sentence because the sentence
/// depends on the [`Layout`] the tool offered and this does not know it. Most callers only want the
/// arguments and drop the error; the tool that answers the call says it with [`Misread::said`].
pub(crate) fn inner(args: &Value) -> Result<Cow<'_, Value>, Misread> {
    if let Some(written) = args.get(UNPARSED).and_then(Value::as_str) {
        return match serde_json::from_str::<Value>(written) {
            // the model's own wrapper round the call it meant; see [`unparsed`]
            Ok(wrapped) if unparsed(args).is_none() => {
                inner(&wrapped).map(|call| Cow::Owned(call.into_owned()))
            }
            _ => Err(Misread::Unparsed(written.to_owned())),
        };
    }

    let inside = match args.get(WRAPPER) {
        Some(it) if it.is_object() => Cow::Borrowed(it),
        Some(Value::String(written)) => match serde_json::from_str::<Value>(written) {
            Ok(parsed) if parsed.is_object() => Cow::Owned(parsed),
            _ => return Err(Misread::Text(written.chars().take(SHOWN).collect())),
        },
        _ => return Ok(Cow::Borrowed(args)),
    };

    let beside: Vec<String> = args
        .as_object()
        .map(|it| it.keys().filter(|k| *k != WRAPPER).cloned())
        .into_iter()
        .flatten()
        .collect();
    if !beside.is_empty() {
        return Err(Misread::Both(beside));
    }

    Ok(deeper(inside))
}

/// Why the arguments of a call could not be read; see [`inner`].
#[derive(Debug)]
pub(crate) enum Misread {
    /// Arguments that never parsed, as they arrived under [`UNPARSED`].
    Unparsed(String),
    /// A [`WRAPPER`] holding text that is not a JSON object: the start of that text.
    Text(String),
    /// Arguments inside [`WRAPPER`] and beside it: the names of the ones beside it.
    Both(Vec<String>),
}

impl Misread {
    /// What the model is told, in the layout it was offered.
    ///
    /// note: a model offered [`Layout::Flat`] is never told to put anything inside `call`, which
    /// was not in its schema. It is told that this tool takes no `call` only where it sent one -
    /// a resumed session's history can show it calls in the wrapped layout - since naming the key
    /// it wrote is the shortest way to say what to drop.
    pub(crate) fn said(&self, layout: Layout) -> String {
        match (self, layout) {
            (Self::Unparsed(written), _) => unreadable(written, layout),
            (Self::Text(written), Layout::Flat) => format!(
                "`{WRAPPER}` arrived as text, and this tool takes no `{WRAPPER}`; nothing was read \
                 and nothing was done. What was in it was `{written}`. Send the call again with \
                 every argument at the top level, beside `action`: `{{\"action\": ...}}`."
            ),
            (Self::Text(written), _) => format!(
                "`{WRAPPER}` arrived as text rather than as an object, and nothing was read and \
                 nothing was done. What was in it was `{written}`. Send the call again, with \
                 `{WRAPPER}` an object: `{{\"{WRAPPER}\": {{\"action\": ...}}}}`."
            ),
            (Self::Both(beside), Layout::Flat) => format!(
                "arguments inside `{WRAPPER}` and beside it ({}), and nothing was done. This tool \
                 takes no `{WRAPPER}`: every argument goes at the top level, beside `action`, and \
                 reading one of the two places would have answered a call nobody made.",
                named(&beside.iter().map(String::as_str).collect::<Vec<_>>())
            ),
            (Self::Both(beside), _) => format!(
                "arguments inside `{WRAPPER}` and beside it ({}), and nothing was done. Every \
                 argument goes inside `{WRAPPER}`, `action` included: reading one of the two \
                 places would have answered a call nobody made.",
                named(&beside.iter().map(String::as_str).collect::<Vec<_>>())
            ),
        }
    }
}

/// The call inside a wrapper whose one entry is the call; otherwise the wrapper as it is.
///
/// note: a model has written every call of a session a level too deep, `{"call": {"item": {...}}}`,
/// under a key nothing asked for. One entry holding an object that names an `action` can only be
/// the call. Anything else - an `action` beside it, a second entry, or no `action` inside - is left
/// as written and answered as a call whose operation cannot be read.
fn deeper(inside: Cow<'_, Value>) -> Cow<'_, Value> {
    let one = inside
        .as_object()
        .filter(|entries| entries.len() == 1)
        .and_then(|entries| entries.values().next())
        .filter(|only| only.get("action").is_some_and(Value::is_string))
        .cloned();
    match one {
        Some(call) => Cow::Owned(call),
        None => inside,
    }
}

/// The operation a call names, if it names one of these.
pub(crate) fn action_of(call: &ToolCall, ops: &[Op]) -> Option<String> {
    let named = inner(&call.args).ok()?.get("action")?.as_str()?.to_owned();
    actions(ops).contains(&named.as_str()).then_some(named)
}

/// Why a question is about everything a tool does: the call did not say which operation it wanted.
///
/// note: [`action_of`] answering `None` is not a silent fallback. It widens what the call declares
/// to every capability the tool has, which is the strictest reading of a call nobody can place and
/// the right one. This adds somebody saying so. A session started with `--auto-allow fs:read` is
/// then asked about `fs:write` as well, the rule it was given matches nothing, and with nobody at
/// the prompt the refusal reads `this call was refused when it was asked about`. That sends a model
/// looking for a different *approach* when what is wrong is the shape of the call it just made.
///
/// note: only for a tool whose schema is laid out the way this module lays one out - arguments
/// under [`WRAPPER`], or an `action` with an `enum` of the operations at the top level - because
/// that is the convention the sentence is about. Read the tool's own schema rather than a list of
/// names kept here: a tool that takes no `action` may declare several capabilities for reasons of
/// its own, and telling its caller that such a call named no operation would invent a vocabulary it
/// never claimed.
pub(crate) fn unnamed_operation(spec: &ToolSpec, request: &PermissionRequest) -> Option<String> {
    let offered = &spec.schema["properties"];
    let layout = match offered.get(WRAPPER) {
        Some(_) => Layout::Wrapped,
        None if offered["action"]["enum"].is_array() => Layout::Flat,
        None => return None,
    };

    let ops: Vec<&str> = spec
        .capabilities
        .iter()
        .map(|capability| capability.op.as_str())
        .collect();
    if ops.len() < 2 || request.capabilities != spec.capabilities {
        return None;
    }

    // note: arguments a model wrote under `_unparsed` that never parsed are reported as unparsed,
    // with what arrived; ones a provider could not parse never reach a policy. A policy that
    // refuses the widened call is answered before the tool is handed it, so the tool's own sentence
    // about them is never read. "names no operation" beside `{"call":` does not look like a call
    // naming nothing to whoever wrote it, and it was sent again unchanged, over and over
    if let Some(written) = unparsed(&request.args) {
        // note: the error gives what is wrong and where to look, as `unreadable` gives them. A
        // model told only that it was not JSON and shown the end of the payload has nothing to
        // change if the fault is in the middle - such as a `\(` in a pattern - and sends the same
        // call again
        let (why, at, rule) = match serde_json::from_str::<Value>(written) {
            Err(e) => (
                format!(" ({e})"),
                at(written, e.line(), e.column()),
                remedy(written, &e, layout),
            ),
            Ok(_) => (String::new(), written.len(), String::new()),
        };
        return Some(format!(
            "the arguments were not JSON{why} - what arrived there was `{}` - so no operation \
             could be read from them, and the call is judged against all {} `{}` has; send it \
             again as one JSON object{rule}",
            around(written, at, SHOWN),
            ops.len(),
            spec.id
        ));
    }

    // note: the same for a `call` that arrived as text the tool cannot read as an object. A model
    // whose own call markup leaks into the arguments sends `{"call": "{\"action\": ... </arg_key>"}`,
    // and told only that it named no operation it sends the next call the same way
    if let Some(Value::String(written)) = request.args.get(WRAPPER)
        && !matches!(serde_json::from_str::<Value>(written), Ok(Value::Object(_)))
    {
        let (why, at, rule) = match serde_json::from_str::<Value>(written) {
            Err(e) => (
                format!(" ({e})"),
                at(written, e.line(), e.column()),
                remedy(written, &e, layout),
            ),
            Ok(_) => (String::new(), 0, String::new()),
        };
        let again = match layout {
            Layout::Flat => format!(
                "with every argument at the top level, since this tool takes no `{WRAPPER}`"
            ),
            _ => format!("with `{WRAPPER}` an object"),
        };
        return Some(format!(
            "`{WRAPPER}` arrived as text that is not a JSON object{why} - what arrived there was \
             `{}` - so no operation could be read from it, and the call is judged against all {} \
             `{}` has; send it again {again}{rule}",
            around(written, at, SHOWN),
            ops.len(),
            spec.id
        ));
    }

    // the same reading the tool will do, so that a call this cannot place is one the tool could
    // not place either
    let args = inner(&request.args).ok();
    let named = args
        .as_ref()
        .and_then(|args| args["action"].as_str().map(str::to_owned));
    let why = match named {
        Some(action) if ops.contains(&action.as_str()) => return None,
        // note: an operation's name with more after it is what a model whose call format is a
        // list of keys and values sends when it slips into that format after `action`. Told only
        // that the name is not an operation, it sent the same call over and over
        Some(action) if let Some(op) = prefix(&action, &ops) => format!(
            "`{action}` is not one of the {} operations `{}` has, so the call is judged against \
             all of them; it is `{op}` with `{}` after it, which belongs in arguments of its own: \
             send `action` as `{op}` alone",
            ops.len(),
            spec.id,
            around(&action[op.len()..], 0, SHOWN)
        ),
        Some(action) => format!(
            "`{action}` is not one of the {} operations `{}` has, so the call is judged against \
             all of them",
            ops.len(),
            spec.id
        ),
        // what it held instead, which is the thing to fix: arguments one level too deep read as
        // a call with no `action`, and naming the key they are under says where they went
        None => format!(
            "the call names no operation{}, so it is judged against all {} `{}` has",
            held(&request.args, args.as_deref()),
            ops.len(),
            spec.id
        ),
    };

    Some(format!(
        "{why} - a rule about one of them, like `{}:{}`, does not answer it",
        spec.capabilities[0].domain, ops[0]
    ))
}

/// The longest of `ops` that `action` begins with, where what follows it cannot continue a name.
fn prefix<'a>(action: &str, ops: &[&'a str]) -> Option<&'a str> {
    ops.iter()
        .copied()
        .filter(|op| {
            action.strip_prefix(op).is_some_and(|rest| {
                !rest.is_empty() && !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
            })
        })
        .max_by_key(|op| op.len())
}

/// What a call that named no operation held where its arguments go, for saying so.
fn held(given: &Value, args: Option<&Value>) -> String {
    let keys: Vec<String> = args
        .and_then(Value::as_object)
        .map(|it| it.keys().map(|key| format!("`{key}`")).collect())
        .unwrap_or_default();
    let place = match given.get(WRAPPER).is_some() {
        true => format!("its `{WRAPPER}` holds"),
        false => "its arguments are".to_owned(),
    };
    // an `action` that is there and is not a name - an object or a list, which is what a call
    // written in some other syntax arrives as - is said to be what it is: "holds `action` and no
    // `action`" is the one reading of it that cannot be acted on
    let what = match args.and_then(|args| args.get("action")) {
        Some(Value::String(_)) | None => {
            return match keys.is_empty() {
                true => String::new(),
                false => format!(" - {place} {} and no `action`", keys.join(", ")),
            };
        }
        Some(other) => super::what(other),
    };
    format!(" - its `action` is {what} rather than the name of one")
}

/// What is wrong with the arguments a call gave, if anything: an argument the operation it named
/// does not read.
///
/// note: the same failure [`super::whole`] is about, one step earlier. An argument that is silently
/// ignored comes back as a *real answer* - the answer to the call without it - so nothing in the
/// reply says that what was asked for did not happen, and a read that was meant to be narrowed
/// arrives as the whole file.
///
/// note: kept, though the schema now says the same thing. The schema is not sent `strict`, so it is
/// advice; this is the part that holds. The two cannot disagree, because they are one table.
pub(crate) fn unread(op: &str, args: &Value, ops: &[Op]) -> Option<String> {
    let mine = ops.iter().find(|it| it.actions.contains(&op))?;
    let given = args.as_object()?;
    let stray = given
        .keys()
        .map(String::as_str)
        .find(|key| *key != "action" && !mine.takes.iter().any(|arg| arg.name == *key))?;

    // note: named only when one operation has it, because the sentence is a *pointer* and there is
    // nowhere to point otherwise. `old` is `edit`'s and saying so is the whole answer; `ids` is
    // several of `context`'s, and "that one is `look`'s" - the first row that has it - would
    // present this table's order as a fact about the argument. A model that has just been told its
    // call was wrong is in no position to discount what it is told next
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
        // note: what it *offers*, so a tolerated argument is not named here. `label` is read by the
        // four that move an item only so that a call giving one can be told what it meant, and
        // listing it among the arguments they take would be the advertisement `Arg::tolerated`
        // exists to withhold, printed in the refusal correcting that mistake
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
    let inside = match Layout::of(schema) {
        Layout::Flat => schema,
        _ => &schema["properties"][WRAPPER],
    };
    let branches = match inside["anyOf"].as_array() {
        Some(several) => several.iter().collect::<Vec<_>>(),
        // a tool with one shape carries it directly, and a flat schema is one object; see `schema`
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

    /// The flat layout is one object: no wrapper and no union, every operation in `action`'s enum,
    /// every argument once, and what a branch would have said kept in the descriptions.
    #[test]
    fn a_flat_schema_keeps_what_the_branches_said() {
        let flat = schema(&ops(), Layout::Flat);
        assert_eq!(Layout::of(&flat), Layout::Flat);
        assert!(
            flat["properties"].get(WRAPPER).is_none() && !flat.to_string().contains("anyOf"),
            "nothing nested to write a call inside: {flat}"
        );
        assert_eq!(
            flat["properties"]["action"]["enum"],
            json!(["read", "grep", "list"])
        );
        let does = flat["properties"]["action"]["description"]
            .as_str()
            .unwrap();
        assert!(
            does.contains("`read`: reads one") && does.contains("`grep`: searches"),
            "{does}"
        );

        // `path` is required by `read` and described differently by `grep`: both are said, and
        // it is not required of the call, which `list` makes without it
        let path = flat["properties"]["path"]["description"].as_str().unwrap();
        assert!(
            path.contains("`read` (required): which") && path.contains("`grep`: where"),
            "{path}"
        );
        assert_eq!(flat["properties"]["path"]["type"], "string");
        assert_eq!(flat["required"], json!(["action"]));
        // and an argument one operation takes says whose it is
        let case = flat["properties"]["ignore_case"]["description"]
            .as_str()
            .unwrap();
        assert!(case.starts_with("`grep`"), "{case}");
    }

    /// A model offered the flat layout is never told to put its arguments inside `call`.
    #[test]
    fn a_flat_refusal_does_not_ask_for_the_wrapper() {
        for misread in [
            Misread::Unparsed("{\"action\": glob, \"pattern\": 1".to_owned()),
            Misread::Text("{\"action\": \"read\", \"path</arg_key>".to_owned()),
            Misread::Both(vec!["path".to_owned()]),
        ] {
            let flat = misread.said(Layout::Flat);
            assert!(
                !flat.contains(&format!("{{\"{WRAPPER}\"")) && !flat.contains("an object:"),
                "{flat}"
            );
        }
        let why = serde_json::from_str::<Value>("{\"call\": glob, \"p\": 1}").unwrap_err();
        let rule = remedy("{\"call\": glob, \"p\": 1}", &why, Layout::Flat);
        assert!(rule.contains("`{\"action\": \"glob\", ...}`"), "{rule}");
    }

    /// A flat schema offers each argument name once, and only one that has a schema: an argument
    /// that is read but not offered is left out rather than given a property of its own.
    #[test]
    fn a_flat_argument_is_offered_once_and_only_where_it_has_a_schema() {
        // one name taken by two operations that both need it is one property, and one `required`
        let shared = vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed()],
            ),
            Op::new(
                "grep",
                "searches",
                vec![Arg::text("path", "which").needed()],
            ),
        ];
        let flat = schema(&shared, Layout::Flat);
        assert_eq!(flat["required"], json!(["action", "path"]), "{flat}");

        // a tolerated argument is read, not offered, so it has no property and makes no taker
        let read_only = vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed(), Arg::tolerated("label")],
            ),
            Op::new("grep", "searches", vec![Arg::text("path", "which")]),
        ];
        let flat = schema(&read_only, Layout::Flat);
        assert!(flat["properties"]["label"].is_null(), "{flat}");
        // `path` is needed by `read` alone, so it is not required of the call
        assert_eq!(flat["required"], json!(["action"]), "{flat}");
    }

    /// Where the operations that take one argument give it one schema and one need, it is offered
    /// as that schema, said once. Where they differ, each says its own, and a keyword only some of
    /// them give is not carried over to the others.
    #[test]
    fn a_flat_argument_is_described_per_taker_only_where_they_disagree() {
        // the same schema, needed by both: one property, the schema itself
        let same = vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed()],
            ),
            Op::new(
                "grep",
                "searches",
                vec![Arg::text("path", "which").needed()],
            ),
        ];
        let flat = schema(&same, Layout::Flat);
        assert_eq!(
            flat["properties"]["path"],
            json!({ "type": "string", "description": "which" }),
            "{flat}"
        );
        assert_eq!(flat["required"], json!(["action", "path"]), "{flat}");

        // the same schema, needed by one and not the other: both say which
        let needed_by_one = vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed()],
            ),
            Op::new("grep", "searches", vec![Arg::text("path", "which")]),
        ];
        let flat = schema(&needed_by_one, Layout::Flat);
        let path = flat["properties"]["path"]["description"].as_str().unwrap();
        assert!(
            path.contains("`read` (required): which") && path.contains("`grep`: which"),
            "{path}"
        );
        assert_eq!(flat["required"], json!(["action"]), "{flat}");

        // a different schema for the same name, needed by both: each says its own
        let differ = vec![
            Op::new(
                "read",
                "reads one",
                vec![Arg::text("path", "which").needed()],
            ),
            Op::new(
                "grep",
                "searches",
                vec![Arg::text("path", "where").needed()],
            ),
        ];
        let flat = schema(&differ, Layout::Flat);
        let path = flat["properties"]["path"]["description"].as_str().unwrap();
        assert!(
            path.contains("`read` (required): which") && path.contains("`grep` (required): where"),
            "{path}"
        );
        assert_eq!(flat["required"], json!(["action"]), "{flat}");

        // a list of exactly one, beside a list of any length: the `minItems` and `maxItems` only
        // one of them gives are not said for the argument as a whole
        let ranged = vec![
            Op::new(
                "revise",
                "changes",
                vec![Arg::one_of("ids", "integer", "which").needed()],
            ),
            Op::new(
                "grep",
                "searches",
                vec![Arg::list("ids", "integer", "what")],
            ),
        ];
        let flat = schema(&ranged, Layout::Flat);
        let ids = &flat["properties"]["ids"];
        assert_eq!(ids["type"], "array", "{flat}");
        assert_eq!(ids["items"], json!({ "type": "integer" }), "{flat}");
        assert!(
            ids.get("minItems").is_none() && ids.get("maxItems").is_none(),
            "{flat}"
        );
    }

    /// Every branch is its own operation, and carries its own arguments and nobody else's.
    ///
    /// note: a flat schema would have `read` and `grep` share one property bag, with only a
    /// description saying `ignore_case` is not `read`'s. The branch says it, so this pins the
    /// branch.
    #[test]
    fn an_operation_declares_its_own_arguments() {
        let schema = schema(&ops(), Layout::Wrapped);
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
    /// note: the narrower is Google's: a closed set of fields, not a JSON Schema document. The list
    /// is that set, from the `generativelanguage` protobuf, intersected with what OpenAI supports.
    /// One schema goes to both, so a keyword outside it gets a 400 from one endpoint and silence
    /// from the other. `const`, `additionalProperties`, `$ref`, `$defs`, `oneOf` and `allOf` are
    /// absent; the first two are what `strict` would need, and Google has neither.
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
        walk(&schema(&ops(), Layout::Wrapped), "", &mut keywords);
        assert!(!keywords.is_empty(), "nothing was walked");

        for keyword in &keywords {
            assert!(
                BOTH.contains(&keyword.as_str()),
                "`{keyword}` is not a keyword both dialects take"
            );
        }
    }

    /// The wrapper has a `type` where it is one shape; a union of several has no `type` beside its
    /// branches.
    ///
    /// note: the single-branch arm gets its `type` from `branch` by accident of construction, so it
    /// is pinned. The union's absence is pinned for the reason in `schema`.
    #[test]
    fn the_wrapper_is_typed_only_where_it_is_one_shape() {
        let several = schema(&ops(), Layout::Wrapped);
        assert!(
            several["properties"][WRAPPER].get("type").is_none(),
            "a `type` beside the union has models write an operation's name there"
        );
        assert!(several["properties"][WRAPPER]["anyOf"].is_array());

        let one = schema(
            &[Op::new(
                "run",
                "runs it",
                vec![Arg::text("cmd", "what").needed()],
            )],
            Default::default(),
        );
        assert_eq!(one["properties"][WRAPPER]["type"], json!("object"));
    }

    /// A call nobody can place is one the question says so about, and a call that names its
    /// operation is left alone.
    ///
    /// note: the cases it stays silent about matter as much. A sentence on every question is a
    /// sentence nobody reads, and this one is only true of a call that did not say which operation
    /// it wanted - which a wrapper written as text has, since [`inner`] reads through one. What is
    /// left is arguments that name nothing and arguments that never parsed.
    #[test]
    fn a_call_that_names_no_operation_says_so() {
        let ops = ops();
        let spec = ToolSpec::new("fs", "the filesystem")
            .with_schema(std::sync::Arc::new(schema(&ops, Layout::Wrapped)))
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
            &asking(json!({ WRAPPER: { "action": { "action": "read" }, "path": "x" } })),
        )
        .expect("an `action` that is not a name places nothing");
        assert!(
            said.contains("its `action` is an object"),
            "and says what it is, rather than that there is none: {said}"
        );
        let said = unnamed_operation(
            &spec,
            &asking(json!({ WRAPPER: { "action": ["read", "read"], "path": "x" } })),
        )
        .expect("nor does a list of them");
        assert!(said.contains("its `action` is a list"), "{said}");
        // in the words every other refusal names a value's kind in
        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: { "action": true } })))
            .expect("nor does a yes");
        assert!(
            said.contains(&format!(
                "its `action` is {}",
                super::super::what(&json!(true))
            )),
            "{said}"
        );

        let said = unnamed_operation(
            &spec,
            &asking(json!({ UNPARSED: "{\"call\": {\"action\"" })),
        )
        .expect("arguments that never parsed name nothing either");
        assert!(said.contains("not JSON"), "{said}");
        assert!(
            said.contains("`{\"call\": {\"action\"`"),
            "and it says what arrived, which is the thing to fix: {said}"
        );
        // and so does a `call` written as text that does not parse, which is what a model's own
        // call markup leaking into the arguments arrives as
        let leaked = "{\"action\": \"grep\", \"pattern</arg_key><arg_value>fn x";
        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: leaked })))
            .expect("a wrapper that does not parse places nothing");
        assert!(said.contains("not a JSON object"), "{said}");
        assert!(said.contains("EOF while parsing"), "{said}");
        assert!(
            said.contains("</arg_key><arg_value>"),
            "and it says what arrived, which is the thing to fix: {said}"
        );
        // and the fault is named and shown where it is, not only the end of what arrived
        let escaped = format!(
            "{{\"call\": {{\"action\": \"grep\", \"pattern\": \"problems\\(\", \"path\": \"{}\"}}}}",
            "x".repeat(300)
        );
        let said = unnamed_operation(&spec, &asking(json!({ UNPARSED: escaped })))
            .expect("arguments that never parsed name nothing");
        assert!(said.contains("invalid escape"), "{said}");
        assert!(
            said.contains("problems\\("),
            "and it shows the fault: {said}"
        );
        assert!(
            said.contains("every backslash inside a string written twice, as `\\\\`"),
            "and the rule it broke, which the position alone does not give: {said}"
        );
        // and an action written bare where its object goes is shown in it
        let said = unnamed_operation(
            &spec,
            &asking(json!({ UNPARSED: "{\"call\": read, \"path\": \"x\"}" })),
        )
        .expect("arguments that never parsed name nothing");
        assert!(
            said.contains("`{\"call\": {\"action\": \"read\", ...}}`"),
            "{said}"
        );
        // and a call the model wrapped in that key itself names its operation, which is read
        // through it
        assert_eq!(
            unnamed_operation(
                &spec,
                &asking(json!({ UNPARSED: "{\"call\": {\"action\": \"read\"}}" })),
            ),
            None
        );

        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: { "action": "fly" } })))
            .expect("`fly` is not one of these");
        assert!(said.contains("`fly` is not one of the"), "{said}");

        // an operation with the next argument written into it, in the model's own markup, is
        // said to be that operation with the rest after it
        let said = unnamed_operation(
            &spec,
            &asking(json!({
                "action": "read<arg_key>from</arg_key><arg_value>658",
                "path": "x"
            })),
        )
        .expect("the action is not an operation");
        assert!(
            said.contains("it is `read` with `<arg_key>from</arg_key><arg_value>658` after it"),
            "{said}"
        );
        assert!(said.contains("send `action` as `read` alone"), "{said}");
        // and a name that merely begins with one is not taken for it
        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: { "action": "reader" } })))
            .expect("`reader` is not one of these");
        assert!(!said.contains("after it"), "{said}");

        // one operation cannot be widened to, and a tool that takes no wrapper is not this
        // convention, so neither gets a sentence about it
        let one = ToolSpec::new("shell", "runs things")
            .with_schema(std::sync::Arc::new(schema(
                &[Op::new("run", "runs it", vec![])],
                Default::default(),
            )))
            .with_capabilities(vec![Capability::exec("run")]);
        assert_eq!(unnamed_operation(&one, &asking(json!({}))), None);

        // and a call whose `needs` answered for less than the whole tool is not one nobody could
        // place, whatever its arguments look like
        let asking_for = |capabilities: Vec<Capability>, args| PermissionRequest {
            id: nachalnik::PermissionId(1),
            call: nachalnik::ToolCallId("c1".to_owned()),
            tool: "fs".to_owned(),
            capabilities,
            args: std::sync::Arc::new(args),
        };
        assert_eq!(
            unnamed_operation(
                &spec,
                &asking_for(
                    vec![Capability::fs("read")],
                    json!({ WRAPPER: { "path": "x" } })
                ),
            ),
            None,
            "and a rule did match, so the widening the call brought is not what is being judged"
        );

        let flat = ToolSpec::new("fs", "the filesystem")
            .with_schema(std::sync::Arc::new(json!({ "type": "object" })))
            .with_capabilities(spec.capabilities.clone());
        assert_eq!(unnamed_operation(&flat, &asking(json!({}))), None);
    }

    /// A flat tool has no `call` to put arguments in. It is told so when its call names no
    /// operation, and when a `call` it was never given arrives.
    #[test]
    fn a_flat_tool_says_a_call_named_no_operation() {
        let ops = ops();
        let spec = ToolSpec::new("fs", "the filesystem")
            .with_schema(std::sync::Arc::new(schema(&ops, Layout::Flat)))
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

        let said = unnamed_operation(&spec, &asking(json!({ "path": "x" })))
            .expect("a flat call that names no operation is one nobody can place");
        assert!(said.contains("names no operation"), "{said}");
        assert!(said.contains("`fs:read`"), "{said}");

        // a `call` that is text and not an object is told to go at the top level, not inside `call`
        let written = "{\"action\": \"read\", \"path";
        let said = unnamed_operation(&spec, &asking(json!({ WRAPPER: written })))
            .expect("a wrapper that does not parse places nothing");
        assert!(
            said.contains("with every argument at the top level, since this tool takes no `call`"),
            "{said}"
        );
    }

    /// The wrapper is asked for, understood without, and refused in both places at once.
    #[test]
    fn arguments_are_read_wherever_a_model_put_them() {
        let wrapped = json!({ WRAPPER: { "action": "read", "path": "x" } });
        assert_eq!(inner(&wrapped).expect("wrapped")["path"], json!("x"));

        let flat = json!({ "action": "read", "path": "x" });
        assert_eq!(inner(&flat).expect("flat")["path"], json!("x"));

        let both = json!({ WRAPPER: { "action": "read" }, "path": "x" });
        let refusal = inner(&both)
            .expect_err("arguments in two places is not a call")
            .said(Layout::Wrapped);
        assert!(refusal.contains("`path`"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
    }

    /// A wrapper holding a string of JSON is read. One holding anything else is refused as that
    /// rather than as an argument nobody gave.
    ///
    /// note: some models write a nested object as a string on every call. If it is not read,
    /// `action` is not found, the call declares every operation the tool has, and a session granted
    /// `fs:read` cannot read a file.
    #[test]
    fn a_wrapper_written_as_text_is_still_a_call() {
        let written = json!({ WRAPPER: "{\"action\": \"read\", \"path\": \"x\"}" });
        assert_eq!(inner(&written).expect("written")["path"], json!("x"));

        let neither = json!({ WRAPPER: "read the file" });
        let refusal = inner(&neither)
            .expect_err("that is not a call")
            .said(Layout::Wrapped);
        assert!(refusal.contains("as text"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
        assert!(refusal.contains("read the file"), "{refusal}");

        // text that parses is still only a call when what it parses to is one
        for not_a_call in ["42", "[1, 2, 3]", "null", "\"read the file\""] {
            let written = json!({ WRAPPER: not_a_call });
            let refusal = inner(&written)
                .expect_err("text that is not a call is not one")
                .said(Layout::Wrapped);
            assert!(refusal.contains("as text"), "{not_a_call}: {refusal}");
            assert!(refusal.contains(not_a_call), "{not_a_call}: {refusal}");
        }
    }

    /// A wrapper whose one entry is the call, one level too deep, is read as the call.
    ///
    /// note: read as written, each such call named no operation and was judged against everything
    /// the tool does. One entry holding an object that names an `action` is as unambiguous as the
    /// flat shape. Two entries, or one with no `action` in it, are not read through: that is a call
    /// whose arguments are somewhere else, and it is answered as one.
    #[test]
    fn a_call_one_level_too_deep_is_still_a_call() {
        let deep = json!({ WRAPPER: { "item": { "action": "read", "path": "x" } } });
        let read = inner(&deep).expect("deep");
        assert_eq!(read["action"], json!("read"));
        assert_eq!(read["path"], json!("x"));

        let beside = json!({ WRAPPER: { "item": { "action": "read" }, "path": "x" } });
        assert_eq!(
            inner(&beside).expect("read as written")["action"],
            Value::Null
        );

        let nameless = json!({ WRAPPER: { "item": { "path": "x" } } });
        assert_eq!(
            inner(&nameless).expect("read as written")["path"],
            Value::Null
        );
    }

    /// Arguments that never parsed are answered as that, not as an argument nobody gave.
    ///
    /// note: the text is a JSON string closed with an XML tag, the way a model can end one, written
    /// under `_unparsed` by a model copying the old shape. Read as missing arguments, the report
    /// would be `the \`action\` argument is required`, about a call whose text says
    /// `"action": "note"` near its start.
    #[test]
    fn arguments_that_never_parsed_are_not_a_missing_argument() {
        let broken = json!({
            UNPARSED: "{\"call\": {\"action\": \"note\", \"content</arg_key>\nthe project is",
        });

        let refusal = inner(&broken)
            .expect_err("this is not a call")
            .said(Layout::Wrapped);
        assert!(refusal.contains("not JSON"), "{refusal}");
        assert!(refusal.contains("nothing was done"), "{refusal}");
        assert!(
            refusal.contains("</arg_key>"),
            "and it says what arrived, because that is the part to look at: {refusal}"
        );
    }

    /// A tool adds to the kernel's answer: how to write a line break and where the action goes, and
    /// nothing about backslashes.
    #[test]
    fn advice_is_given_where_it_was_measured_to_help() {
        assert_eq!(
            advice(
                "{\"call\": {\"action\": \"grep\", \"pattern\": \"a\nb\"}}",
                Layout::Wrapped
            )
            .as_deref(),
            Some("Write it with a line break inside a string written as `\\n`."),
        );
        assert!(
            advice("{\"call\": read, \"path\": \"x\"}", Layout::Wrapped)
                .is_some_and(|said| said.contains("names the action")),
        );
        assert_eq!(
            advice("{\"call\": {\"pattern\": \"f\\(x\"}}", Layout::Wrapped),
            None,
            "advice about backslashes sent models to escape the value as well"
        );
        assert_eq!(advice("{\"call\": {}}", Layout::Wrapped), None, "it parses");
    }

    /// Arguments a model wrote under `_unparsed` itself are read as the call inside them.
    ///
    /// note: `inclusionai/ling-3.1-flash` copied the shape its one broken call was sent back in onto
    /// every call after it, each one valid inside the wrapper, and was told each time that its
    /// arguments were not JSON. Text there that does not parse, or that sits beside something else,
    /// is not a call, and is still answered as not JSON.
    #[test]
    fn a_call_the_model_wrapped_as_unparsed_is_still_a_call() {
        let copied = json!({ UNPARSED: "{\"call\": {\"action\": \"read\", \"path\": \"x\"}}" });
        let read = inner(&copied).expect("a call inside the model's own wrapper");
        assert_eq!(read["action"], json!("read"));
        assert_eq!(read["path"], json!("x"));

        let flat = json!({ UNPARSED: "{\"action\": \"read\", \"path\": \"x\"}" });
        assert_eq!(inner(&flat).expect("flat inside it")["path"], json!("x"));

        for still in [
            json!({ UNPARSED: "{\"call\": read}" }),
            json!({ UNPARSED: "42" }),
            json!({ UNPARSED: "{\"action\": \"read\"}", "path": "x" }),
        ] {
            let refusal = inner(&still).expect_err("not a call").said(Layout::Wrapped);
            assert!(refusal.contains("nothing was done"), "{refusal}");
        }
    }

    /// A payload broken by how a string was written says how to write it.
    ///
    /// note: both are shapes a model writes without seeing them. The regex is the commoner: a
    /// pattern copied into a string as it stands, `\(` and all, which reads to the model as the
    /// pattern it meant and was sent again unchanged after being told only where it failed.
    #[test]
    fn a_string_written_wrong_says_how_to_write_it() {
        let said = unreadable(
            "{\"call\": {\"action\": \"grep\", \"pattern\": \"fn held\\(\"}}",
            Layout::Wrapped,
        );
        assert!(said.contains("invalid escape"), "{said}");
        assert!(
            said.contains("every backslash inside a string written twice, as `\\\\`"),
            "{said}"
        );

        let said = unreadable(
            "{\"call\": {\"action\": \"note\", \"content\": \"one\ntwo\"}}",
            Layout::Wrapped,
        );
        assert!(said.contains("control character"), "{said}");
        assert!(
            said.contains("a line break inside a string written as `\\n`"),
            "{said}"
        );
    }

    /// An action written bare where the wrapper's object goes is shown in that object.
    ///
    /// note: both are shapes `inclusionai/ling-3.1-flash` sent in the friction sweep, the second
    /// with its own markup for the rest of the call after the word. Told where the parse stopped,
    /// it sent the next call the same way about as often as it fixed it.
    #[test]
    fn a_bare_action_is_shown_in_the_object_it_belongs_in() {
        for written in [
            "{\"call\": glob, \"path\": \".\", \"pattern\": \"src/kernel/mod.rs\"}",
            "{\"call\": grep\n<arg_key>path</arg_key>\n<arg_value>., \"pattern\": \"odd\"}",
        ] {
            let said = unreadable(written, Layout::Wrapped);
            let word = if written.contains("glob") {
                "glob"
            } else {
                "grep"
            };
            assert!(
                said.contains(&format!(
                    "`call` holding an object that names the action, as in \
                     `{{\"call\": {{\"action\": \"{word}\", ...}}}}`"
                )),
                "{said}"
            );
        }

        let quoted = unreadable(
            "{\"call\": {\"action\": \"read\", \"path\": x}}",
            Layout::Wrapped,
        );
        assert!(
            !quoted.contains("names the action"),
            "a bare word anywhere else is not the action out of its object: {quoted}"
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
        let said = unreadable(short, Layout::Wrapped);
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
        let said = unreadable(&escaped, Layout::Wrapped);
        assert!(
            !said.contains("backslash"),
            "a quote left open is not a backslash to double: {said}"
        );
        assert!(
            said.contains("import sys,json"),
            "the window follows the fault rather than quoting the start: {said}"
        );
        assert!(
            said.contains('…'),
            "and says there is more either side: {said}"
        );
    }

    /// A fault before the end of a payload is shown where it occurs, not at the end.
    ///
    /// note: the position arrives as a line and a column, counted out of the text - the step
    /// between what the parser said and what the model is shown. A payload written over several
    /// lines is ordinary: a model indenting a command produces one. A count that lands anywhere but
    /// the fault quotes the part that was fine.
    #[test]
    fn a_fault_before_the_end_of_a_payload_is_shown_where_it_is() {
        let payload = [
            format!("{{\"pad\": \"{}\",", "L".repeat(300)),
            format!(" \"cmd\": \"{}\" THERE oops,", "M".repeat(200)),
            format!(" \"tail\": \"{}\"", "N".repeat(300)),
            "}".to_owned(),
        ]
        .join("\n");

        let said = unreadable(&payload, Layout::Wrapped);
        assert!(said.contains("not JSON"), "{said}");
        assert!(
            said.contains("THERE"),
            "the window is around the fault, which is where the count landed: {said}"
        );
        assert!(
            !said.contains('L'),
            "and not the start of the payload: {said}"
        );
    }

    /// The window around a fault takes in the text before it as well as after.
    ///
    /// note: a fault is usually a missing `}` or an unescaped `"`, recognisable from the text in
    /// front of it. A window that opened at the fault quotes the rest of the command and none of
    /// the reason it is wrong.
    #[test]
    fn the_window_says_what_is_around_the_fault() {
        let payload = format!(
            "{{\"call\": {{\"action\": \"run\", \"cmd\": \"{}\" oops \"tail\": \"{}\"}}}}",
            "B".repeat(400),
            "A".repeat(400)
        );

        let said = unreadable(&payload, Layout::Wrapped);
        assert!(said.contains("not JSON"), "{said}");
        assert!(
            said.contains('B') && said.contains('A'),
            "the window is on both sides of the fault: {said}"
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
