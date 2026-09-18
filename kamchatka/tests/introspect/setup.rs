//! `setup`: what the session is running with, and what the policy will say before it is asked.

use crate::{agent, answered, answers_from, one_turn};
use kamchatka::{introspect, tools::Careful, tools::Limits, tools::Subject};
use nachalnik::{
    Config, ContextItem, Kernel, ModelResponse, Verdict, test::ScriptedProvider, test::call,
};
use serde_json::json;
use std::sync::Arc;

/// `setup tools` says how much of an answer reaches the model, by subject and not by tool.
///
/// note: the figure they share and then whichever ones do not, rather than a column. A column
/// would have had one number standing for `fs:read` and `fs:grep` alike, which is the thing
/// keying a limit by subject exists to stop - and this is the one place a model can find out what
/// its own answers are being cut at, so a number that is wrong here is one it cannot check.
#[tokio::test]
async fn setup_tools_says_what_each_answer_is_cut_at_by_subject() {
    let limits = Limits::new();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "tools" }),
    )]))));
    let policy = Arc::new(Careful::new());
    for domain in ["context", "log", "setup", "fork"] {
        policy.set(&Subject::parse(domain), Verdict::Allow);
    }
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, limits.clone());

    // one subject held to something else, which is what the sentence has to be able to say
    limits.set("context:search", 4_000).expect("a row for it");

    kernel.push(ContextItem::user("what are you running with?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("cut at 32,000 bytes"), "{said}");
    assert!(
        said.contains("context:search at 4,000"),
        "the one somebody changed is named, or the sentence is wrong about it: {said}"
    );
    // and the ones the table ships holding to something else are named together rather than a
    // clause each, since they are one figure said once
    assert!(
        said.contains(
            "context:budget, context:note, context:revise, setup:model, setup:policy \
                       at 8,000"
        ),
        "{said}"
    );
    // and only the subjects this session's tools declare: no `fs` here, so no `fs:read` in the
    // answer, for the reason `if_offered` exists
    assert!(
        !said.contains("fs:read"),
        "it names a subject nothing here declares: {said}"
    );
}

/// What a session was told to forget, `tools` says it has forgotten - the way `policy` does.
///
/// note: `rules` reads the configuration for this sentence and `tools` stated the opposite
/// outright, so one tool gave two answers about the same setting two actions apart. What it costs
/// is a model going looking for content this session was told to drop.
#[tokio::test]
async fn setup_tools_says_whether_what_is_cut_is_kept() {
    for keep in [true, false] {
        let kernel = Kernel::new(Config {
            keep_truncated_output: keep,
            ..Config::default()
        });
        kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
            "c1",
            "setup",
            json!({ "action": "tools" }),
        )]))));
        let policy = Arc::new(Careful::new());
        policy.set(&Subject::parse("setup"), Verdict::Allow);
        kernel.set_policy(policy.clone());
        let _anchor = introspect::install(&kernel, policy, Limits::default());

        kernel.push(ContextItem::user("what are you offered?"));
        kernel.turn().await.expect("the turn failed");

        let said = answered(&kernel);
        match keep {
            true => assert!(
                said.contains("archived beside what you were shown"),
                "{said}"
            ),
            false => assert!(
                said.contains("not kept: this session was told to forget it"),
                "`--forget-truncated` was set and this says it can be restored: {said}"
            ),
        }
    }
}

/// A tool taken away mid-session is not on the list, which is the point of there being a list.
///
/// note: the shape this exists for. Nothing anywhere let an agent enumerate its own tools, so a
/// model whose `shell` was removed between two turns had no way to find that out and every reason
/// to keep asking for it - or, worse, to explain confidently why it had not used it. With `log`
/// beside it the pair answers both halves: this says what there is, `tools.changed` says when it
/// went.
#[tokio::test]
async fn setup_tools_reflects_a_tool_taken_away_mid_session() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call("c1", "setup", json!({ "action": "tools" }))]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call("c2", "setup", json!({ "action": "tools" }))]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what have you got?"));
    kernel.add_tool(Arc::new(nachalnik::test::ConstTool::new(
        "secret", "hunter2",
    )));
    kernel.turn().await.expect("the turn failed");

    // the registry is live and `Tool::spec` is read afresh for every request, which is what makes
    // this a second question rather than a second session
    kernel.remove_tool("secret");
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.expect("the second turn failed");

    let said = answers_from(&kernel, &["setup"]);
    assert!(said[0].contains("secret"), "{}", said[0]);
    assert!(
        !said[1].contains("secret"),
        "a tool that has gone is not on the list: {}",
        said[1]
    );
    // and what each one declares, which is the thing a model cannot see and the thing that
    // decides whether a call is worth making at all
    assert!(said[0].contains("nothing declared"), "{}", said[0]);
    assert!(said[0].contains("tools.changed"), "{}", said[0]);
}

/// It says which model it is, and whether the conversation is one it started.
///
/// note: the resumed line is the one that could not be worked out from inside. A context restored
/// from a snapshot carries first-person turns this model never produced, and nothing in a turn
/// records which hand wrote it - so a model asked about its own earlier reasoning in a resumed
/// session owns all of it, because it has no way not to.
#[tokio::test]
async fn setup_model_says_whether_this_conversation_was_inherited() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "model" }),
    )]));
    kernel.push(ContextItem::user("who are you?"));
    kernel.turn().await.expect("the turn failed");

    let fresh = answered(&kernel);
    assert!(
        fresh.contains("started here"),
        "a session nobody resumed says so: {fresh}"
    );

    // the same question on a session resumed from this one's snapshot
    let second = Kernel::resume(Config::default(), kernel.snapshot());
    second.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c2",
        "setup",
        json!({ "action": "model" }),
    )]))));
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::parse("setup"), Verdict::Allow);
    second.set_policy(policy.clone());
    let _anchor = introspect::install(&second, policy, Limits::default());
    second.push(ContextItem::user("who are you?"));
    second.turn().await.expect("the turn failed");

    // note: the *last* one, and the reason is the whole point of the action. The resumed context
    // carries the first session's answer as an item, so the earliest thing `setup model` says in
    // here is the old session's "started here" - written by a model that was right when it wrote
    // it and is being read by one for whom it is false. This is what a model has no way to notice
    // from the inside, which is what the new line is for.
    let carried = answers_from(&second, &["setup"]);
    assert!(
        carried[0].contains("started here"),
        "the inherited answer came along, unchanged and now wrong: {}",
        carried[0]
    );
    let now = carried.last().expect("it answered");
    assert!(now.contains("resumed from a snapshot"), "{now}");
    assert!(
        now.contains("may not be you"),
        "and says the turns in it are not necessarily its own: {now}"
    );
}

/// An undecided domain is not the last word, and an undecided server is; the report says which.
///
/// note: one sentence used to cover both - "will stop and ask, whatever the rows above say" -
/// which is true of a server and false of a domain. `Careful::stance` reads the exact stance in
/// front of the domain's, so `fs` undecided beside `fs:read` allowed is a read that does not stop,
/// and the table said it did. A model that believes it will be stopped does not try.
#[tokio::test]
async fn an_undecided_domain_says_that_a_row_above_it_can_answer_for_one_operation() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )])));
    kernel.set_provider(provider);
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::parse("setup"), Verdict::Allow);
    // somebody has looked at `fs` and left it, and answered for one operation in it
    policy.set(&Subject::parse("fs"), Verdict::Ask);
    policy.set(&Subject::parse("fs:read"), Verdict::Allow);
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy.clone(), Limits::default());

    kernel.push(ContextItem::user("what may you do?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("unless a row above names that operation"),
        "an undecided domain is answerable by an exact rule: {said}"
    );
    assert!(
        !said.contains("whatever the rows above say"),
        "which is what the old sentence claimed for it: {said}"
    );
    // and the policy agrees with the sentence
    assert_eq!(
        policy.stance(&Subject::parse("fs:read")),
        Verdict::Allow,
        "the report and the policy disagree about the same call"
    );
}

/// The verdicts come from the policy's own table, and say which policy that is.
#[tokio::test]
async fn setup_permissions_says_what_will_be_refused_before_it_is_asked() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]));
    kernel.push(ContextItem::user("what may you do?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    // the policy actually consulted, named, beside the table being reported
    assert!(said.contains("Careful"), "{said}");
    // the four this session answered for, and the word for a thing nobody has decided
    assert!(said.contains("context") && said.contains("allow"), "{said}");
    assert!(
        said.contains("`ask` is nobody having decided yet, not a refusal"),
        "the difference a model has to be able to act on: {said}"
    );
    // a path rule is in the same answer, because it binds the same calls
    assert!(said.contains(".env"), "{said}");
    // and asking is not deciding: reporting the policy must not have changed it
    assert!(kernel.pending_permissions().is_empty(), "{said}");
}

/// What will happen to the context without anybody asking for it, named so it can be looked up.
#[tokio::test]
async fn setup_policy_names_the_seams_that_rewrite_a_context_on_their_own() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "policy" }),
    )]));
    kernel.set_compactor(Some(Arc::new(kamchatka::tools::Trim {
        threshold: 0.8,
        target: 0.6,
    })));
    kernel.push(ContextItem::user("what will be done to me?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("the projector is"), "{said}");
    assert!(said.contains("Trim"), "the compactor by name: {said}");
    assert!(
        said.contains("cannot take anything pinned"),
        "and the promise the kernel keeps against it: {said}"
    );
    assert!(
        said.contains("one at a time, in the order you asked"),
        "{said}"
    );
}

/// A seam that is generic is named as the type it is, not as the type inside it.
///
/// note: `short` took the last `::` segment of the whole string, so the counter this program
/// ships - `Calibrating<BytesPerToken>` - came out as `BytesPerToken>`: the wrong type, the outer
/// one silently dropped, and a stray bracket as the only sign anything had gone wrong. It was on
/// the trace pane before it was here. A seam names itself so that somebody can look it up, and a
/// name that is not the type's cannot be looked up.
#[tokio::test]
async fn a_generic_seam_is_named_as_itself() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "policy" }),
    )]));
    kernel.push(ContextItem::user("what is plugged in?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("Calibrating<BytesPerToken>"), "{said}");
    assert!(
        !said.contains("BytesPerToken>,"),
        "the outer type is not dropped: {said}"
    );
    // and the module paths are off, because this is read by something paying for every token
    assert!(!said.contains("nachalnik::"), "{said}");
}

/// The undecided path rules are counted rather than listed, the way the permissions tab does it.
#[tokio::test]
async fn setup_permissions_counts_the_rules_nobody_has_thought_about() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]));
    kernel.push(ContextItem::user("what may you touch?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("path rule(s) are undecided"),
        "a count, not eleven rows of the same verdict: {said}"
    );
    // still named, because an answer standing silently for eleven rules would be its own kind of
    // dishonest - it is the row per rule that is not worth the tokens, not the fact of them
    assert!(said.contains(".env*"), "{said}");
}

/// Every row is one operation in one domain, and the tools it decides for are named beside it.
///
/// note: the table used to have a row per *capability*, and seven of the ten tools declared one
/// named after themselves - `read` the capability, declared by `read` the tool. The column that
/// was meant to say what a rule covers read as a tautology on those rows and said something only
/// on the one where two tools shared a capability. An operation is what a tool actually does, so
/// the column says something on every row.
#[tokio::test]
async fn setup_permissions_lists_one_row_per_operation() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]));
    kernel.push(ContextItem::user("what may you touch?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    // two tools act on one object, and the rows say so rather than naming the tools
    assert!(said.contains("context:look"), "{said}");
    assert!(said.contains("context:revise"), "{said}");
    assert!(
        !said.contains("nothing here is judged by it"),
        "every operation here decides for something: {said}"
    );
    // and every row is an operation rather than a tool's own name, which is what the old table
    // had on seven of its ten rows
    let rows = said
        .lines()
        .skip_while(|line| !line.starts_with("capability"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty());
    for row in rows {
        let subject = row.split_whitespace().next().expect("a row names one");
        assert!(
            subject.contains(':'),
            "`{subject}` is a tool's name, not an operation: {said}"
        );
    }
}

/// A rule about a whole domain gets a section of its own, because it is not a row in a table of
/// operations: nothing declares `context`, and it governs every row that starts with it.
#[tokio::test]
async fn setup_permissions_puts_a_domain_rule_in_a_section_of_its_own() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )])));
    kernel.set_provider(provider);
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::parse("setup"), Verdict::Allow);
    policy.set(&Subject::parse("context"), Verdict::Deny);
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());

    kernel.push(ContextItem::user("what may you touch?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("every operation in it"), "{said}");
    assert!(
        said.contains("context") && said.contains("deny"),
        "the rule and what it answers: {said}"
    );
    // note: a domain is a set of operations and a server is a set of tools, which is what the
    // third column of that section says; the heading used to call both of them "rules about
    // single actions, which bind the tool they name", which is true of neither
    assert!(
        !said.contains("single action"),
        "the section is not about single actions: {said}"
    );
}

/// `mcp:call` names no tool that came from a server this program spawned, because it does not
/// decide for one.
///
/// note: live, on a session run `--allow mcp --mcp big=...`. The answer read `mcp:call  allow
/// big__add, big__spew` and the very next call to `big__spew` was refused: `Careful::judges` takes
/// `mcp:call` back out for a tool whose server it knows and puts the server's own name in its
/// place, so that `--allow-server` is one flag rather than two. The table was filled from what the
/// tools *declare*, which is the list before that swap - so it named a rule as being in force over
/// two tools it is never consulted about, in the one answer a model has for checking what it may
/// do.
#[tokio::test]
async fn setup_permissions_does_not_credit_mcp_with_a_server_s_tools() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]))));
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::parse("setup"), Verdict::Allow);
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy.clone(), Limits::default());

    let unvouched = nachalnik::Capability::of(nachalnik::Domain::Other("mcp".to_owned()), "call");
    for id in ["big__spew", "loose"] {
        kernel.add_tool(Arc::new(
            nachalnik::test::ConstTool::new(id, "did it").with_capabilities([unvouched.clone()]),
        ));
    }
    policy.came_from("big__spew", "big");

    kernel.push(ContextItem::user("what may you touch?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let row = said
        .lines()
        .find(|line| line.starts_with("mcp:call"))
        .unwrap_or_else(|| panic!("the row is there: {said}"));
    assert!(
        !row.contains("big__spew"),
        "the server answers for that one: {row}"
    );
    assert!(row.contains("loose"), "and this one it decides: {row}");
}
