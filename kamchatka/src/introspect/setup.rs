//! The tool that reads what the session is running *with*: the model, the tools, the policy, and
//! the rules that will rewrite a context whether anybody asks or not.
//!
//! note: state, not events, which is why there are two read tools rather than one. `log` says a
//! tool was taken away at record 118; this says which tools there are now. `log` says a permission
//! was answered; this says what the policy will say next time. Neither can be recovered from the
//! other, and a tool trying to be both would answer "what am I running with?" with a list of things
//! that have happened.
//!
//! note: and not *settings*, which is why the description opens with what you are running with
//! rather than with a noun that invites a write. Nothing here changes anything. `config` was
//! rejected for the same reason, and because it collides with the file this program's own
//! `--config-file` reads.
//!
//! note: each action answers something a model cannot otherwise find out: which model it is, at
//! what temperature, and whether its context was resumed from a snapshot taken under somebody else;
//! what its own tools are; what the permissions will say; and the rules behind whether an item goes
//! into the next request. `look` says whether it does without exposing the rules that decided, so
//! without this a model could read the verdict and never the rule.

use nachalnik::{
    BoxError, Capability, ContextId, Event, Kernel, OutputSink, Tool, ToolCall, ToolOutput,
    ToolSpec, Verdict, async_trait,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

use crate::{
    app::text::{short, thousands, verdict_word},
    tools::{
        Careful, Limits, Subject, domains,
        ops::{Layout, Op, action_of, actions, inner, schema},
    },
};

use super::{Reach, action, if_reachable, unknown};

/// The four things this reads, which take no arguments and are therefore one shape.
///
/// note: one branch and not four. A union exists to say which arguments go with which operation
/// and none of these has any, so four branches would be four copies of the same empty object -
/// scaffolding charged for on every request to gate nothing.
fn ops() -> Vec<Op> {
    vec![Op::these(
        &["model", "tools", "permissions", "policy"],
        // note: what each one does is in the tool's own description rather than here. With one
        // shape there is no choice for a branch description to inform - the four differ in what
        // they answer, not in what they take - so it is a fact about `setup`, and putting it on
        // the branch would leave `call` meaning something different here than in every tool that
        // has several
        "",
        vec![],
    )]
}

/// Reads what the session is running with: the model, the tools, the policy, the rules.
pub struct Setup {
    reach: Reach,
    policy: Arc<Careful>,
    limits: Limits,
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Setup {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, policy: Arc<Careful>, limits: Limits, layout: Layout) -> Self {
        let ops = ops();
        Self {
            reach,
            policy,
            limits,
            schema: Arc::new(schema(&ops, layout)),
            ops,
        }
    }
}

#[async_trait]
impl Tool for Setup {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "setup",
            "reads what you are running with, which you cannot otherwise find out, and changes \
             nothing. `model` is which model you are, what parameters it is being sent, how much \
             context it has, and whether this \
             conversation was resumed from a snapshot - which matters, because a resumed context \
             can be somebody else's earlier turns and nothing in them says so. `tools` is every \
             tool you are offered, what each declares it needs, and how much of its output you \
             are shown; one that went away mid-session is simply not here. `permissions` is what \
             the policy allows, refuses, or will stop and ask about, so you can tell a thing that \
             will be refused from a thing nobody has decided. `policy` is what the compactor and \
             the projector will do to your context unasked.",
        )
        .with_schema(self.schema.clone())
        .with_capabilities(
            actions(&self.ops)
                .into_iter()
                .map(domains::setup)
                .collect::<Vec<_>>(),
        )
    }

    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match action_of(call, &self.ops) {
            Some(action) => vec![domains::setup(action)],
            None => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    fn unparsed(&self, written: &str) -> Option<String> {
        crate::tools::ops::advice(written, Layout::of(&self.schema))
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(misread) => return Ok(ToolOutput::error(misread.said(Layout::of(&self.schema)))),
        };
        let args = &*args;

        let action = action(args)?;
        if let Some(refusal) = crate::tools::ops::unread(action, args, &self.ops) {
            return Ok(ToolOutput::error(refusal));
        }
        match action {
            "model" => Ok(ToolOutput::new(model(&kernel))),
            "tools" => Ok(ToolOutput::new(tools(&kernel, &self.limits, &self.policy))),
            "permissions" => Ok(ToolOutput::new(permissions(&kernel, &self.policy))),
            "policy" => Ok(ToolOutput::new(rules(&kernel, &self.policy))),
            other => Ok(ToolOutput::error(unknown(other, &actions(&self.ops)))),
        }
    }
}

/// Which model this is, what it is being sent, and whose conversation it inherited.
///
/// note: the resume line cannot be worked out from inside. A context restored from a snapshot
/// carries first-person turns this model never produced, possibly a different model's, and an
/// assistant turn says nothing about which hand wrote it. A model asked about its earlier reasoning
/// in a resumed session will own all of it unless this line tells it otherwise.
fn model(kernel: &Kernel) -> String {
    let mut out = match kernel.model_info() {
        Some(info) => {
            let mut said = format!("you are `{}`", info.model);
            // `0` is what an endpoint that does not know says; see `Budget::fraction_used`
            if let Some(limit) = info.context_limit.filter(|limit| *limit != 0) {
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

    // read from the record, not a flag: the record is where it is true. A resumed session is one
    // that emitted `session.resumed`; nothing else in a kernel remembers that it did
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
    // note: a model switch inside the session is the same blindness, with nothing to announce it.
    // After `/model`, turns the other model wrote read in the first person like the rest, and a
    // model told the conversation started here took every one of them for its own
    if let Some(info) = kernel.model_info() {
        for (model, items) in written_by_others(kernel, &info.model) {
            out.push_str(&format!(
                "not every turn here that reads as yours is: `{model}` wrote {}, before the \
                 model was switched.\n",
                match items.as_slice() {
                    [one] => format!("item {one}"),
                    many => format!(
                        "items {}",
                        many.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            ));
        }
    }

    out
}

/// Answers still in the context that models other than `current` wrote, grouped by model.
///
/// note: read from the record, which names the model each request went to and the item its answer
/// became. The answers are taken out before the context is asked which items remain, so no lock is
/// held across the two.
fn written_by_others(kernel: &Kernel, current: &str) -> BTreeMap<String, Vec<ContextId>> {
    let answered: Vec<(String, ContextId)> = kernel.with_history(|session| {
        let mut asked = None;
        let mut answered = Vec::new();
        for record in session.records() {
            match &record.event {
                Event::ModelRequested { model, .. } => asked = Some(model.model.clone()),
                Event::ModelFinished { item, .. } => {
                    if let Some(model) = asked.take() {
                        answered.push((model, *item));
                    }
                }
                Event::ModelFailed { .. } => asked = None,
                _ => {}
            }
        }
        answered
    });

    let mut by = BTreeMap::<String, Vec<ContextId>>::new();
    for (model, item) in answered {
        if model != current && kernel.item(item).is_some() {
            by.entry(model).or_default().push(item);
        }
    }

    by
}

/// Every tool on offer, what it declares, and how much of any one answer is shown.
///
/// note: the declared capabilities beside each one, because a model cannot see them and they decide
/// whether a call is worth making. A tool it may not use and a tool that does not exist fail
/// differently, so tell them apart before the call, not after.
///
/// note: limits are a sentence under the table, not a column, because they are keyed by subject and
/// a tool has several subjects. A column would give `fs:read` and `fs:grep` one number; keying by
/// subject stops that. A row per subject would repeat the shared figure. So: the shared number,
/// then the subjects that differ.
fn tools(kernel: &Kernel, limits: &Limits, policy: &Careful) -> String {
    let specs = kernel.tool_specs();
    if specs.is_empty() {
        return "you are offered no tools at all; whatever you ask for will come back unknown.\n"
            .to_owned();
    }

    let mut out = format!(
        "{} tool(s), which is every one that goes into the next request:\n{:<12}  {:<28}  what it \
         is\n",
        specs.len(),
        "tool",
        "needs",
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
        out.push_str(&format!(
            "{:<12}  {:<28}  {}\n",
            spec.id,
            needs,
            first_clause(&spec.description),
        ));
    }

    let config = kernel.config();
    out.push_str(&format!(
        "\n{}",
        shown(&specs, limits, kernel.default_tool_output_limit())
    ));
    // note: read from the configuration rather than stated, as `rules` does, so a session run with
    // `--discard-cut-off-output` is not told here that the whole of anything cut is still available
    // and the opposite is two actions away. The cost is that a model looks for content this session
    // was told to drop
    out.push_str(match config.keep_truncated_output {
        true => {
            "The whole of anything cut is excluded beside what you were shown and can be \
             restored. A tool that was taken away mid-session is not on this list.\n"
        }
        false => {
            "What is cut is not kept: this session was told to forget it. A tool that was taken \
             away mid-session is not on this list.\n"
        }
    });
    out.push_str(&if_reachable(kernel, policy, "log:read", || {
        "`log` with `kinds: [\"tools.changed\"]` says when one went.\n".to_owned()
    }));

    out
}

/// How much of an answer reaches the model, by subject: the shared figure, then the rest.
///
/// note: include only the subjects these tools declare, so a session that does not offer `fs` is
/// not told what `fs:read` would be cut at. This is the rule `if_reachable` is named for, one level
/// down: everything named in an answer is read as something that is there.
fn shown(specs: &[nachalnik::ToolSpec], limits: &Limits, floor: Option<usize>) -> String {
    let mut held: Vec<(String, usize)> = specs
        .iter()
        .flat_map(|spec| spec.capabilities.iter())
        .filter_map(|subject| {
            let subject = subject.to_string();
            limits.of(&subject).map(|bytes| (subject, bytes))
        })
        .collect();
    held.sort();
    held.dedup();

    // note: the kernel's own ceiling cuts a tool with no row in that table, including every tool
    // from an MCP server. Without it, a session offering only those tools would read
    // `Nothing here cuts an answer short` while the kernel cuts all of them at 32,000 bytes, and a
    // model reading that has no reason to ask for less
    let elsewhere = specs.iter().any(|spec| {
        spec.output_limit.is_none()
            && !spec
                .capabilities
                .iter()
                .any(|subject| limits.of(&subject.to_string()).is_some())
    });
    let floor = floor.filter(|_| elsewhere).map(|bytes| {
        format!(
            "A tool with no row of its own - one from a server - is cut at {}. ",
            thousands(bytes)
        )
    });
    if held.is_empty() {
        return match floor {
            Some(said) => format!("{said}\n"),
            None => "Nothing here cuts an answer short.\n".to_owned(),
        };
    }

    // the one most of them share, which is what a session nobody has changed anything in has
    let mut counted: Vec<(usize, usize)> = Vec::new();
    for (_, bytes) in &held {
        match counted.iter_mut().find(|(at, _)| at == bytes) {
            Some((_, seen)) => *seen += 1,
            None => counted.push((*bytes, 1)),
        }
    }
    let common = counted
        .iter()
        .max_by_key(|(_, seen)| *seen)
        .map(|(bytes, _)| *bytes)
        .unwrap_or_default();

    // note: group by the figure rather than giving a clause per row. The table ships with two
    // numbers (a fixed-shape report is cut at less than a piece of the session), so several
    // subjects share the second one. A clause each would repeat `at 8,000` instead of saying it
    // once
    let mut groups: Vec<(usize, Vec<&str>)> = Vec::new();
    for (subject, bytes) in held.iter().filter(|(_, bytes)| *bytes != common) {
        match groups.iter_mut().find(|(at, _)| at == bytes) {
            Some((_, named)) => named.push(subject),
            None => groups.push((*bytes, vec![subject])),
        }
    }
    let odd: Vec<String> = groups
        .iter()
        .map(|(bytes, named)| format!("{} at {}", named.join(", "), thousands(*bytes)))
        .collect();

    format!(
        "An answer is cut at {} bytes{}. {}",
        thousands(common),
        match odd.is_empty() {
            true => String::new(),
            false => format!(", except {}", odd.join(", ")),
        },
        floor.unwrap_or_default()
    )
}

/// What the policy will say, before anything is asked.
///
/// note: asked of the policy's own table rather than by running a call through it. `evaluate`
/// records the reason it refused something, so a read tool that asked it four questions would write
/// four refusals into a bounded queue of sixty-four and silently evict the explanations a real
/// refusal is going to need. A tool that reports state must not change it, so this reads the same
/// two lists the permissions tab draws.
///
/// note: this is a `Careful`, the policy this program ships and installs, and the answer says so. A
/// session whose policy was swapped for somebody else's would be reported on by this table and the
/// report would be about the wrong one, so `kernel.policy().name()` names the one actually
/// consulted and the two are printed together, making a disagreement between them visible.
fn permissions(kernel: &Kernel, policy: &Careful) -> String {
    let in_force = short(kernel.policy().name());
    let mut out = format!("the policy deciding your calls is `{in_force}`.\n");

    // which tools each capability binds, so a verdict is read as being about something
    //
    // note: `Careful::decides` as well as the declaration, because they are not the same list.
    // `mcp:call` is declared by every tool from a server and answered for by the server's own name
    // where this program spawned it, so read off the declaration alone, a session run
    // `--auto-allow mcp --mcp big=...` would list that server's tools under an allowed `mcp:call`
    // here and then refuse them. A row still appears - somebody wrote that rule - and it says what
    // is true, which is that nothing here is judged by it
    let mut binds: BTreeMap<Capability, Vec<String>> = BTreeMap::new();
    for spec in kernel.tool_specs() {
        for capability in spec.capabilities {
            let bound = policy.decides(&Subject::Capability(capability.clone()), &spec.id);
            let tools = binds.entry(capability).or_default();
            if bound {
                tools.push(spec.id.clone());
            }
        }
    }
    // note: a rule about a whole domain is a stance too, and it is deliberately not a row in
    // this table of operations: it is not declared by anything - a tool declares `context:revise`,
    // never `context` - so it would read `nothing here is judged by it` beside a verdict that
    // governs three rows above it. It gets a section of its own below, the way a path rule does.
    //
    // note: the flag says which of the two kinds a row is, because an undecided one means
    // different things for each, and one sentence below cannot say both. A domain is answered by
    // a row above it that names the same operation - `Careful::stance` reads the exact stance in
    // front of the domain's - and a server is consulted *beside* those rows, so an undecided
    // server stops every call from it whatever its tools' rows say.
    let mut broader: Vec<(String, String, Verdict, bool)> = Vec::new();
    for (subject, verdict) in policy.stances() {
        match subject {
            Subject::Capability(capability) => {
                let reaches = capability == Capability::net("reach");
                let tools = binds.entry(capability).or_default();
                // note: a capability the policy consults without a tool declaring it, which is
                // `net:reach` and the network gate. The gate is what asks about a command, and it
                // asks about a `shell`, so a row for it read `nothing here is judged by it` beside
                // a verdict that had just refused one of the model's commands - while the same
                // session's own `/permissions` said `shell, when the command reaches for it`. Named
                // here, because the operator's view and the model's are one answer written twice
                // and they cannot say opposite things
                if reaches {
                    for spec in kernel.tool_specs() {
                        if spec
                            .capabilities
                            .iter()
                            .any(|it| it == &Capability::exec("run"))
                            && !tools.contains(&spec.id)
                        {
                            tools.push(spec.id.clone());
                        }
                    }
                }
            }
            Subject::Domain(domain) => {
                broader.push((
                    domain.to_string(),
                    "every operation in it".to_owned(),
                    verdict,
                    false,
                ));
            }
            Subject::Server(name) => {
                broader.push((
                    format!("server {name}"),
                    "its tools".to_owned(),
                    verdict,
                    true,
                ));
            }
            Subject::Path(_) => {}
        }
    }

    if !binds.is_empty() {
        // note: what the rule *decides for*, which is not quite what declares it. `mcp:call` is
        // declared by every tool from a server and decides for none this program spawned, so a
        // column headed "the tools that declare it" had one true answer and one useful one and
        // could not give both. The useful one is the one a rule is read for
        out.push_str(&format!(
            "\n{:<28}  {:<8}  {}\n",
            "capability", "verdict", "the tools it decides for"
        ));
        for (capability, tools) in &binds {
            let verdict = policy.stance(&Subject::Capability(capability.clone()));
            out.push_str(&format!(
                "{:<28}  {:<8}  {}\n",
                capability.to_string(),
                verdict_word(verdict),
                match tools.is_empty() {
                    true => "nothing here is judged by it".to_owned(),
                    // note: the clause beside that row in the operator's view, so a model reading
                    // either view learns the same thing about the rule that decides whether a
                    // command may open a socket
                    false => format!(
                        "{}{}",
                        tools.join(", "),
                        match capability == &Capability::net("reach") {
                            true => ", when the command reaches for it",
                            false => "",
                        }
                    ),
                },
            ));
        }
    }

    // note: the decided ones listed and the rest counted, as the permissions tab does. A row for a
    // `.aws` rule nobody has thought about is not information. Most path rules ship as `ask`, and a
    // row each would spend tokens repeating "undecided" in a session where nobody has said anything
    // about a path. The count still goes out: listing the decided rules and standing silently for
    // the rest would be dishonest.
    let (decided, undecided): (Vec<_>, Vec<_>) = policy
        .paths()
        .into_iter()
        .partition(|(_, verdict)| *verdict != Verdict::Ask);
    if !decided.is_empty() {
        out.push_str("\nand rules about paths, which bind the tools handed one:\n");
        for (pattern, verdict) in &decided {
            out.push_str(&format!("{:<28}  {}\n", pattern, verdict_word(*verdict)));
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

    // the same split the path rules get, for the same reason: a row of `ask` nobody has thought
    // about is not information, and a report listing none of them and saying nothing would stand
    // silently for every one of those answers
    let (decided, undecided): (Vec<_>, Vec<_>) = broader
        .into_iter()
        .partition(|(_, _, verdict, _)| *verdict != Verdict::Ask);
    if !decided.is_empty() {
        // note: what these cover is the third column, because a domain is a set of operations and a
        // server is a set of tools. A heading about single actions binding the tool they name would
        // be true of neither: `--auto-allow log` is every operation in `log`, and `server big`
        // names no tool at all
        out.push_str("\nand the broader rules, which have no row of their own above:\n");
        for (rule, covers, verdict, _) in &decided {
            out.push_str(&format!(
                "{rule:<28}  {:<8}  {covers}\n",
                verdict_word(*verdict)
            ));
        }
    }
    let (servers, domains): (Vec<_>, Vec<_>) = undecided.iter().partition(|(.., server)| *server);
    let listed = |rules: &[&(String, String, Verdict, bool)]| {
        rules
            .iter()
            .map(|(rule, ..)| rule.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !domains.is_empty() {
        out.push_str(&format!(
            "\n{} domain rule(s) are undecided, so anything done in them stops and asks unless a \
             row above names that operation: {}.\n",
            domains.len(),
            listed(&domains),
        ));
    }
    if !servers.is_empty() {
        out.push_str(&format!(
            "\n{} server rule(s) are undecided, and a server is consulted beside the rows above - \
             so a call from one stops and asks whatever its own rows say: {}.\n",
            servers.len(),
            listed(&servers),
        ));
    }

    let waiting = kernel.pending_permissions();
    if !waiting.is_empty() {
        // note: shown on purpose. An agent that can see it is blocked on somebody's answer can
        // decide to do something else with the turn; otherwise the call appears to have hung
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
    out.push_str(&if_reachable(kernel, policy, "log:read", || {
        "How each of these was arrived at is in `log` with `kinds: [\"permission.decided\"]`.\n"
            .to_owned()
    }));

    out
}

/// What will happen to this context without anybody asking for it.
///
/// note: `look` says whether an item is going into the next request, but never what decided that,
/// and a model that can read the verdict but not the rule cannot argue with either. This names the
/// two seams that rewrite a context on their own, the projector and the compactor, and the counter
/// and the limit they are measured by, with the numbers that say when each acts.
fn rules(kernel: &Kernel, policy: &Careful) -> String {
    let config = kernel.config();
    let budget = kernel.budget();

    let mut out = format!(
        "the projector is `{}`. It is what turns your context into the messages of a request - \
         which items become which messages, in what order, and which are left out or repaired to \
         keep the request valid.{}\n",
        short(kernel.projector().name()),
        if_reachable(kernel, policy, "context:request", || {
            " `context` with `request` says what it did to the next one.".to_owned()
        }),
    );

    out.push_str(&match kernel.compactor() {
        Some(compactor) => format!(
            "\nthe compactor is `{}`. It is asked before every request, and may move items out \
             of it without being asked - it cannot take anything pinned, it says exactly what it \
             moved{}.{}\n",
            short(compactor.name()),
            if_reachable(kernel, policy, "context:restore", || {
                ", and `context` with `restore` puts any of it back".to_owned()
            }),
            if_reachable(kernel, policy, "log:read", || {
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
            "the budget it is measured against is {} tokens{}.\n",
            thousands(limit),
            if_reachable(kernel, policy, "context:budget", || {
                "; `context` with `budget` is where you stand against it".to_owned()
            }),
        ));
    }

    out.push_str(&format!(
        "\na tool result longer than its limit is truncated, and {}\n",
        match config.keep_truncated_output {
            true =>
                "the whole of it is excluded beside the copy you were shown, so it is still \
                     here.",
            false => "the rest is not kept: this session was told to forget it.",
        },
    ));
    if config.keep_truncated_output {
        out.push_str(&if_reachable(kernel, policy, "context:search", || {
            "`context` with `search` reaches what was cut without putting it back.\n".to_owned()
        }));
    }
    out.push_str(&format!(
        "one turn makes at most {} request(s) before it stops, whatever you are in the middle \
         of.\n",
        match kernel.max_requests_per_turn() {
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

/// The first sentence of a tool's description, which is the part that says what it is.
///
/// note: the whole sentence, however long, rather than cut to fit a column: a column is a
/// terminal's problem, not a reader's. A model has no way to tell a description that ends there
/// from one that was clipped, and nothing on the line would say which it is.
///
/// note: it costs nothing not already being paid. Every one of these descriptions is in the request
/// in full, in the tool definitions, so cutting one would save a few characters and make this
/// tool's account of what is there unreliable.
fn first_clause(description: &str) -> String {
    description
        .split_once(". ")
        .map(|(head, _)| head)
        .unwrap_or(description)
        .to_owned()
}
