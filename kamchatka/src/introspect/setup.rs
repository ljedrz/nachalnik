//! The tool that reads what the session is running *with*: the model, the tools, the policy, and
//! the rules that will rewrite a context whether anybody asks or not.
//!
//! note: state, not events, and the split is the reason there are two read tools rather than one.
//! `log` says a tool was taken away at record 118; this says which tools there are now. `log` says
//! a permission was answered; this says what the policy will say next time. Neither can be
//! recovered from the other, and a tool trying to be both would answer "what am I running with?"
//! with a list of things that have happened.
//!
//! note: and not *settings*, which is why the description opens with what you are running with
//! rather than with a noun that invites a write. Nothing here changes anything. The word `config`
//! was rejected for the same reason, on top of colliding with the file this program's own
//! `--config-file` reads.
//!
//! note: what each action is for is a question the program could not answer before. The model
//! could not tell which model it was, at what temperature, or that its context had been resumed
//! from a snapshot taken under somebody else. Nothing anywhere let an agent enumerate its own
//! tools. The permissions surface was claimed in the readme and demonstrated nowhere. And `look`
//! says whether an item is going into the next request without exposing the rules that decided -
//! so a model could read the verdict and never the law.

use nachalnik::{
    BoxError, Capability, Kernel, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, Verdict,
    async_trait,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

use crate::{
    app::text::{short, thousands},
    tools::{Careful, Limits, Subject},
};

use super::{Reach, action, if_offered, unknown};

/// Reads what the session is running with: the model, the tools, the policy, the rules.
pub struct Setup {
    reach: Reach,
    policy: Arc<Careful>,
    limits: Limits,
}

impl Setup {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, policy: Arc<Careful>, limits: Limits) -> Self {
        Self {
            reach,
            policy,
            limits,
        }
    }
}

#[async_trait]
impl Tool for Setup {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new(
            "setup",
            "reads what you are running with, which is not something you can otherwise find out. \
             `model` is which model you are, what parameters it is being sent, how much context it \
             has, and whether this conversation was resumed from a snapshot - which matters, \
             because a resumed context can be somebody else's earlier turns and nothing in them \
             says so. `tools` lists every tool you are offered, what each one declares it needs, \
             and how much of its output you are shown; a tool that went away mid-session is simply \
             not here, and `log` says when. `permissions` is what the policy allows, refuses, or \
             will stop and ask about - so you can tell a thing that will be refused from a thing \
             that has not come up. `policy` is what the compactor and the projector will do to \
             your context without being asked. All of it is read-only; `amend` is what changes a \
             context, and nothing here changes a session.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["model", "tools", "permissions", "policy"],
                },
            },
            "required": ["action"],
        }))
        .with_capabilities([Capability::Custom("setup".into())]);

        self.limits.apply(spec)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        match action(&call.args)? {
            "model" => Ok(ToolOutput::new(model(&kernel))),
            "tools" => Ok(ToolOutput::new(tools(&kernel, &self.limits))),
            "permissions" => Ok(ToolOutput::new(permissions(&kernel, &self.policy))),
            "policy" => Ok(ToolOutput::new(rules(&kernel))),
            other => Ok(ToolOutput::error(unknown(
                other,
                &["model", "tools", "permissions", "policy"],
            ))),
        }
    }
}

/// Which model this is, what it is being sent, and whose conversation it inherited.
///
/// note: the resume line is the one worth having and the one that could not be worked out from
/// inside. A context restored from a snapshot carries turns in the first person that this model
/// never produced - possibly a different model's - and there is nothing in an assistant turn that
/// says which hand wrote it. A model asked about its earlier reasoning in a resumed session will
/// own all of it, because it has no way not to. This is the way not to.
fn model(kernel: &Kernel) -> String {
    let mut out = match kernel.model_info() {
        Some(info) => {
            let mut said = format!("you are `{}`", info.model);
            if let Some(limit) = info.context_limit {
                said.push_str(&format!(", with {} tokens of context", thousands(limit)));
            }
            said.push('\n');
            said
        }
        None => "no provider is plugged in, so there is no model and nothing to ask\n".to_owned(),
    };

    let params = kernel.params();
    out.push_str(&match params.is_empty() {
        true => {
            "no parameters are being sent; the provider's own defaults are in force\n".to_owned()
        }
        false => format!(
            "parameters, sent verbatim: {}\n",
            serde_json::to_string(&params).unwrap_or_default()
        ),
    });

    // read off the record rather than off a flag, because the record is where it is true: a
    // resumed session is one that emitted `session.resumed`, and nothing else in a kernel
    // remembers that it did
    let resumed = kernel.with_history(|session| {
        session.records().find_map(|record| match &record.event {
            nachalnik::Event::SessionResumed { items, tokens, .. } => Some((*items, *tokens)),
            _ => None,
        })
    });
    out.push_str(&match resumed {
        Some((items, tokens)) => format!(
            "this conversation was resumed from a snapshot: {} item(s), ~{} tokens, already here \
             before you said anything. Turns in it that read as yours were produced by whatever \
             model that session was running, which may not be you, and nothing in a turn records \
             which.\n",
            thousands(items),
            thousands(tokens),
        ),
        None => "this conversation started here; nothing in it was inherited from another \
                 session.\n"
            .to_owned(),
    });

    out
}

/// Every tool on offer, what it declares, and how much of it you are shown.
///
/// note: the declared capabilities beside each one, because that is the thing a model cannot see
/// and the thing that decides whether a call is worth making. A tool it may not use and a tool
/// that does not exist fail differently and are worth telling apart before the call rather than
/// after it.
fn tools(kernel: &Kernel, limits: &Limits) -> String {
    let specs = kernel.tool_specs();
    if specs.is_empty() {
        return "you are offered no tools at all; whatever you ask for will come back unknown.\n"
            .to_owned();
    }

    let mut out = format!(
        "{} tool(s), which is every one that goes into the next request:\n{:<12}  {:<28}  {:>9}  \
         what it is\n",
        specs.len(),
        "tool",
        "needs",
        "shown",
    );
    for spec in &specs {
        let needs = match spec.capabilities.is_empty() {
            true => "nothing declared".to_owned(),
            false => spec
                .capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        };
        let shown = match limits.of(&spec.id).or(spec.output_limit) {
            Some(bytes) => format!("{} B", thousands(bytes)),
            None => "all of it".to_owned(),
        };
        out.push_str(&format!(
            "{:<12}  {:<28}  {shown:>9}  {}\n",
            spec.id,
            needs,
            first_clause(&spec.description),
        ));
    }
    out.push_str(
        "\n`shown` is how much of a result reaches you; the whole of anything cut is archived \
         beside it and can be restored. A tool that was taken away mid-session is not on this \
         list.\n",
    );
    out.push_str(&if_offered(kernel, "log", || {
        "`log` with `kinds: [\"tools.changed\"]` says when one went.\n".to_owned()
    }));

    out
}

/// What the policy will say, before anything is asked.
///
/// note: asked of the policy's own table rather than by running a call through it. `evaluate`
/// records the reason it refused something, so a read tool that asked it four questions would
/// write four refusals into a bounded queue of sixty-four and quietly evict the explanations a
/// real refusal is going to need. A tool that reports state must not change it, and the way it
/// does not here is by reading the same two lists the permissions tab draws.
///
/// note: this is a `Careful`, the policy this program ships and installs, and the answer says so.
/// A session whose policy was swapped for somebody else's would be reported on by this table and
/// the report would be about the wrong one, so `kernel.policy().name()` names the one actually
/// consulted and the two are printed together - a disagreement between them is then visible
/// rather than implied.
fn permissions(kernel: &Kernel, policy: &Careful) -> String {
    let in_force = short(kernel.policy().name());
    let mut out = format!("the policy deciding your calls is `{in_force}`.\n");

    // which tools each capability binds, so a verdict is read as being about something
    let mut binds: BTreeMap<Capability, Vec<String>> = BTreeMap::new();
    for spec in kernel.tool_specs() {
        for capability in spec.capabilities {
            binds.entry(capability).or_default().push(spec.id.clone());
        }
    }
    for (capability, _) in policy.stances() {
        binds.entry(capability).or_default();
    }

    if !binds.is_empty() {
        out.push_str(&format!(
            "\n{:<28}  {:<8}  {}\n",
            "capability", "verdict", "the tools that declare it"
        ));
        for (capability, tools) in &binds {
            let verdict = policy.stance(&Subject::Capability(capability.clone()));
            out.push_str(&format!(
                "{:<28}  {:<8}  {}\n",
                capability.to_string(),
                said(verdict),
                match tools.is_empty() {
                    true => "nothing you have declares it".to_owned(),
                    false => tools.join(", "),
                },
            ));
        }
    }

    // note: the decided ones listed and the rest counted, which is what the permissions tab does
    // and for the reason its own note gives: a row for a `.aws` rule nobody has thought about is
    // not information. Eleven of them ship as `ask`, so a session where nobody has said anything
    // about a path was spending ninety tokens saying "undecided" eleven times. The count still
    // goes out, because an answer that listed two rules and stood silently for thirteen would be
    // a different kind of dishonest.
    let (decided, undecided): (Vec<_>, Vec<_>) = policy
        .paths()
        .into_iter()
        .partition(|(_, verdict)| *verdict != Verdict::Ask);
    if !decided.is_empty() {
        out.push_str("\nand rules about paths, which bind the tools handed one:\n");
        for (pattern, verdict) in &decided {
            out.push_str(&format!("{:<28}  {}\n", pattern, said(*verdict)));
        }
    }
    if !undecided.is_empty() {
        out.push_str(&format!(
            "\n{} path rule(s) are undecided and will stop and ask - the ones that look like \
             credentials: {}.\n",
            undecided.len(),
            undecided
                .iter()
                .map(|(pattern, _)| pattern.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    let waiting = kernel.pending_permissions();
    if !waiting.is_empty() {
        // note: shown on purpose. An agent that can see it is blocked on somebody's answer is an
        // agent that can decide to do something else with the turn, which is the whole argument
        // for any of this - and the alternative is a call that appears to have hung
        out.push_str(&format!(
            "\n{} call(s) of yours are waiting on somebody to answer: {}\n",
            waiting.len(),
            waiting
                .iter()
                .map(|request| request.tool.clone())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    out.push_str(
        "\n`ask` is nobody having decided yet, not a refusal: the call stops and somebody is \
         asked. A refusal is a standing answer and the same call will be refused again.\n",
    );
    out.push_str(&if_offered(kernel, "log", || {
        "How each of these was arrived at is in `log` with `kinds: [\"permission.decided\"]`.\n"
            .to_owned()
    }));

    out
}

/// What will happen to this context without anybody asking for it.
///
/// note: `look` says whether an item is going into the next request. It has never said what
/// decided that, and a model that can read the verdict but not the rule cannot argue with either.
/// These are the four seams that rewrite a context on their own, named so that they can be looked
/// up, with the numbers that say when each of them acts.
fn rules(kernel: &Kernel) -> String {
    let config = kernel.config();
    let budget = kernel.budget();

    let mut out = format!(
        "the projector is `{}`. It is what turns your context into the messages of a request - \
         which items become which messages, in what order, and which are left out or repaired to \
         keep the request valid. `context` with `request` says what it did to the next one.\n",
        short(kernel.projector().name()),
    );

    out.push_str(&match kernel.compactor() {
        Some(compactor) => format!(
            "\nthe compactor is `{}`. It runs when the context gets too full and moves items out \
             of the request without being asked - it cannot take anything pinned, it says exactly \
             what it moved, and `amend` with `restore` puts any of it back.{}\n",
            short(compactor.name()),
            if_offered(kernel, "log", || {
                " `log` with `kinds: [\"context.compacted\"]` is every pass it has made.".to_owned()
            }),
        ),
        None => "\nthere is no compactor: nothing will be moved out of your context on its own, \
                 and a context that outgrows the limit is a request the provider refuses.\n"
            .to_owned(),
    });

    out.push_str(&format!(
        "\nthe counter is `{}`, and it is an estimate rather than the model's own tokenizer.\n",
        short(kernel.counter().name()),
    ));
    if let Some(limit) = budget.limit {
        out.push_str(&format!(
            "the budget it is measured against is {} tokens; `context` with `budget` is where you \
             stand against it.\n",
            thousands(limit),
        ));
    }

    out.push_str(&format!(
        "\na tool result longer than its limit is cut, and {}\n",
        match config.keep_truncated_output {
            true =>
                "the whole of it is archived beside the copy you were shown, so it is still \
                     here.",
            false => "the rest is not kept: this session was told to forget it.",
        },
    ));
    if config.keep_truncated_output {
        out.push_str(&if_offered(kernel, "context", || {
            "`context` with `search` reaches what was cut without putting it back.\n".to_owned()
        }));
    }
    out.push_str(&format!(
        "one turn makes at most {} request(s) before it stops, whatever you are in the middle \
         of.\n",
        match config.max_requests_per_turn {
            Some(n) => thousands(n),
            None => "unlimited".to_owned(),
        },
    ));
    out.push_str(&format!(
        "your tool calls run {}.\n",
        match config.parallel_tool_calls {
            true => "at the same time, so the order you asked in is not the order they happen in",
            false => "one at a time, in the order you asked for them",
        },
    ));

    out
}

/// A verdict, in the word a person would use about it.
fn said(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
        // the runtime's enum is not exhaustive, and `ask` is the honest answer for anything this
        // build has no word for: it is the one that stops and involves somebody
        _ => "ask",
    }
}

/// The first sentence of a tool's description, which is the part that says what it is.
fn first_clause(description: &str) -> String {
    /// How much of it fits the column.
    const ROOM: usize = 56;

    let first = description
        .split_once(". ")
        .map(|(head, _)| head)
        .unwrap_or(description);
    match first.chars().count() > ROOM {
        true => format!("{}…", first.chars().take(ROOM - 1).collect::<String>()),
        false => first.to_owned(),
    }
}
