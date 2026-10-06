//! The question that stops a turn, and the tab that shows every answer already given.
//!
//! note: two views of one thing. A permission is asked about once, at the moment it is least
//! convenient to think about it, and read back afterwards on a tab where it can be changed - so
//! these check both ends: that the question names what it is about and cannot be answered by
//! somebody's next keystroke, and that the tab reports the same policy the calls actually meet.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{
    app::{Focus, Speaker, Tab},
    sandbox::Confinement,
    tools::{Limits, Subject},
};
use nachalnik::{
    Capability, ContextItem, Domain, ModelResponse, Verdict,
    test::{ConstTool, call},
};
use ratatui::style::Color;
use serde_json::json;

use crate::harness::Harness;

#[tokio::test]
async fn a_tool_that_needs_permission_stops_the_turn_and_puts_the_question_on_the_screen() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("I could not, so I did not"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig somewhere").await;
    harness.settle().await;

    // the question, with the arguments it would run with
    let screen = harness.screen();
    assert!(screen.contains("a tool wants to run"), "{screen}");
    assert!(screen.contains("dig wants: exec:run"), "{screen}");
    assert!(screen.contains("\"where\""), "{screen}");

    // looking closer, and then coming back: the tool is still waiting, so the question has to
    // still be there. It was not, and there was no way left to answer it
    harness.answer(KeyCode::Char('i')).await;
    let inspected = harness.screen();
    assert!(inspected.contains("what it was asked to do"), "{inspected}");
    assert!(
        inspected.contains("\"capabilities\""),
        "the tool itself: {inspected}"
    );

    harness.press(KeyCode::Esc).await;
    let back = harness.screen();
    assert!(back.contains("dig wants: exec:run"), "{back}");
    assert!(back.contains("[y] once"), "{back}");

    // saying no answers the call rather than abandoning it: the model is told
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    let denied = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| item.label == "dig")
        .expect("the refusal is a tool result like any other");
    assert!(
        denied.content.to_text().contains("not permitted"),
        "{denied:?}"
    );
    assert!(harness.app.overlay.is_none());
}

/// A question takes the letters its help lines name, and not their capitals.
#[tokio::test]
async fn a_capital_letter_does_not_answer_a_question() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("dug"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    for capital in ['Y', 'A', 'N', 'D', 'I'] {
        harness.answer(KeyCode::Char(capital)).await;
        assert!(
            harness.app.overlay.is_none() && harness.screen().contains("dig wants: exec:run"),
            "{capital} did something: {}",
            harness.screen()
        );
    }

    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.screen().contains("dug"), "{}", harness.screen());
}

/// An answer of "always" is in the record as a rule, tied to the question it answered.
///
/// note: the rule went into the policy's table and nowhere else, so every later call it let
/// through was recorded as allowed by the policy in force, and nothing said which rule, where it
/// came from, or when.
#[tokio::test]
async fn saying_always_is_a_rule_in_the_record() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("dug"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    let asked = harness.app.asked().expect("a question").id;
    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    let ruled = ruled(&harness);
    assert!(
        ruled.contains(&(
            "exec:run".to_owned(),
            nachalnik::Verdict::Allow,
            Some(asked),
            false
        )),
        "{ruled:?}"
    );
}

#[tokio::test]
async fn saying_always_stops_the_question_being_asked_again() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::tool_calls(vec![call("c2", "dig", json!({}))]),
        ModelResponse::text("twice"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig twice").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "the first one is a question");

    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    // the second call went through without stopping, and the policy says why - the prompt's
    // "always" and the permissions tab are the same table
    assert!(harness.app.overlay.is_none());
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::exec("run"))),
        nachalnik::Verdict::Allow
    );
    let results = harness
        .app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.label == "dig")
        .count();
    assert_eq!(results, 2);
}

#[tokio::test]
async fn dropping_the_pending_calls_tells_the_model_rather_than_losing_them() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![
        call("c1", "danger", json!({})),
        call("c2", "danger", json!({})),
    ])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    let asked = harness.screen();
    assert!(asked.contains("[d] drop them all"), "{asked}");

    harness.answer(KeyCode::Char('d')).await;
    harness.drain();

    assert!(
        harness.app.kernel.pending_permissions().is_empty(),
        "nothing should still be waiting"
    );
    // the model is told, rather than left waiting for calls that silently vanished
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(sent.contains("cancelled"), "{sent}");
    assert!(harness.screen().contains("2 call(s) dropped"));
}

#[tokio::test]
async fn the_permissions_tab_shows_every_answer_the_policy_would_give() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    harness.tab(Tab::Permissions);

    // nothing has been decided, so there is nothing to list: `ask` is what this policy does about
    // everything nobody has answered for, and a screenful of it would bury the line that says what
    // can happen without stopping. The rest are counted along the bottom instead
    let screen = harness.sized(110, 30);
    assert_eq!(
        harness.app.permissions().len(),
        0,
        "`ask` is not a decision, and nobody has made one"
    );
    assert!(screen.contains("nothing has been decided"), "{screen}");
    assert!(screen.contains("more it will ask about"), "{screen}");

    // ... and deciding one puts it on the tab, naming what it covers
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let shell = screen
        .lines()
        .find(|line| line.contains("exec:run"))
        .expect("a decided capability is listed");
    assert!(shell.contains("deny"), "{shell}");
    // and names what it covers, which for a rule about one operation is that operation. It named
    // the tool, and a tool is wider than an operation of it: `fs:glob  allow  fs` reads as the
    // narrow rule answering for the whole thing
    assert_eq!(
        shell.matches("exec:run").count(),
        2,
        "the subject, and the operation it covers: {shell}"
    );
    assert!(!shell.contains("rm"), "which is not the tool: {shell}");

    // no tool declares `network` - but the shell is judged against it anyway, on what the command
    // says, so its row names the tool the answer actually reaches rather than claiming that
    // nothing needs it
    harness.app.policy.set(
        &Subject::Capability(Capability::net("reach")),
        Verdict::Deny,
    );
    let screen = harness.sized(110, 30);
    let network = screen
        .lines()
        .find(|line| line.contains("net:reach"))
        .expect("the decision is listed");
    assert!(network.contains("deny"), "{network}");
    assert!(
        network.contains("rm, when the command reaches for it"),
        "{network}"
    );

    // and a capability nothing needs at all still reads as a fact rather than a gap
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("write")), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let write = screen
        .lines()
        .find(|line| line.contains("write"))
        .expect("what the policy has been told about is listed");
    assert!(write.contains("nothing registered needs it"), "{write}");

    // a rule about a whole domain names the capabilities it answers for, which is what somebody
    // wrote it to decide. It read "nothing registered needs it" - a row was only filled where a
    // tool declared that exact capability, and no tool declares a bare domain - and naming its
    // tools instead would have told somebody who wrote `--allow log` that it covered `log`
    harness
        .app
        .policy
        .set(&Subject::Domain(Domain::Fs), Verdict::Allow);
    let screen = harness.sized(110, 30);
    let domain = screen
        .lines()
        .find(|line| {
            line.replace('\u{2502}', " ")
                .trim_start()
                .starts_with("fs ")
        })
        .unwrap_or_else(|| panic!("the domain rule is listed: {screen}"));
    assert!(
        domain.contains("fs:read"),
        "a rule about `fs` answers for the operations in it: {domain}"
    );
    assert!(!domain.contains("nothing registered needs it"), "{domain}");
}

/// An operation the domain above it answers for is not a second row saying the same thing.
///
/// note: every capability a registered tool declares was a subject, and a domain rule made each of
/// them a *decided* one - so `--allow log` wrote one rule and drew two rows, `log  allow  log:read`
/// and `log:read  allow  log`, each naming the other in the column beside it. `--allow context`
/// drew fourteen. Rows on this tab are decisions somebody took, and the two halves of that are
/// checked here: an operation somebody answered about separately keeps its row, and one nobody has
/// decided is still counted along the bottom rather than quietly dropped with the rest.
#[tokio::test]
async fn an_operation_the_domain_above_it_answers_for_has_no_row_of_its_own() {
    let mut harness = Harness::new(Vec::new());
    for (id, capability) in [
        ("read", Capability::fs("read")),
        ("write", Capability::fs("write")),
        ("rm", Capability::exec("run")),
    ] {
        harness.app.kernel.add_tool(Arc::new(
            ConstTool::new(id, "did it").with_capabilities([capability]),
        ));
    }
    harness.tab(Tab::Permissions);
    let untold = harness.app.undecided();

    harness
        .app
        .policy
        .set(&Subject::Domain(Domain::Fs), Verdict::Allow);
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("write")), Verdict::Deny);

    let screen = harness.sized(110, 30);
    let row = |named: &str| {
        screen
            .lines()
            .map(|line| line.replace('\u{2502}', " "))
            .find(|line| line.trim_start().starts_with(&format!("{named} ")))
    };

    let domain = row("fs").unwrap_or_else(|| panic!("the rule somebody wrote is listed: {screen}"));
    assert!(
        domain.contains("fs:read") && domain.contains("fs:write"),
        "naming every operation it answers for: {domain}"
    );
    assert!(
        row("fs:read").is_none(),
        "and that operation has no row of its own: {screen}"
    );

    let refused = row("fs:write").unwrap_or_else(|| panic!("this one was decided too: {screen}"));
    assert!(
        refused.contains("deny"),
        "a rule of its own, and the strictest wins: {refused}"
    );

    // the shell's `exec:run` is nobody's decision, so it is not a row and never was one - it is
    // counted along the bottom, and the two the rules answered are the only two that left
    assert!(row("exec:run").is_none(), "{screen}");
    assert_eq!(harness.app.undecided(), untold - 2);

    // and cycling the domain back to `ask` takes that decision back, so the operation under it is
    // undecided again rather than hidden behind a rule that no longer answers anything. Which is
    // the count it started at: `fs:read` is a question again, `fs:write` is still refused, and the
    // `fs` row somebody cycled is now a subject nobody has decided either
    harness
        .app
        .policy
        .set(&Subject::Domain(Domain::Fs), Verdict::Ask);
    assert_eq!(harness.app.undecided(), untold);

    let screen = harness.sized(110, 30);
    assert!(
        screen
            .lines()
            .map(|line| line.replace('\u{2502}', " "))
            .any(|line| line.trim_start().starts_with("fs:write ")),
        "the one somebody did decide stays: {screen}"
    );
}

/// A rule about an MCP server names the tools that came from it.
///
/// note: the same fault as the domain row, on the subject that had it worse - the policy's list of
/// servers existed to answer this and had no caller at all, so `--allow-server files` listed a row
/// saying nothing registered needed it while every tool it covered sat above it.
#[tokio::test]
async fn a_rule_about_a_server_names_the_tools_that_came_from_it() {
    let mut harness = Harness::new(Vec::new());
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("files__read", "read it").with_capabilities([Capability::fs("read")]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
    ));
    harness.app.policy.came_from("files__read", "files");
    harness.tab(Tab::Permissions);

    harness
        .app
        .policy
        .set(&Subject::Server("files".to_owned()), Verdict::Allow);
    let screen = harness.sized(110, 30);
    let row = screen
        .lines()
        .find(|line| line.contains("server files"))
        .unwrap_or_else(|| panic!("the server rule is listed: {screen}"));

    assert!(row.contains("files__read"), "{row}");
    assert!(
        !row.contains("grep"),
        "`grep` did not come from that server: {row}"
    );
    assert!(!row.contains("nothing registered needs it"), "{row}");
}

/// And `mcp:call` does not, because a tool from a server this program spawned is not judged by it.
///
/// note: the coverage column was filled from what a tool *declares* while `Careful::judges` decides
/// from something narrower: every tool from a server declares `mcp:call`, and `judges` swaps it for
/// the server's own name so that `--allow-server files` is one flag rather than two. So a session
/// run `--allow mcp --mcp big=...` was shown `mcp:call  allow  big__spew` and then refused the very
/// call - a table that reads as a rule in force over a tool the rule is never consulted about, and
/// the table somebody checks their flags against.
#[tokio::test]
async fn a_rule_about_mcp_covers_nothing_a_spawned_server_offers() {
    let mut harness = Harness::new(Vec::new());
    let unvouched = Capability::of(nachalnik::Domain::Other("mcp".to_owned()), "call");
    for id in ["files__read", "loose"] {
        harness.app.kernel.add_tool(Arc::new(
            ConstTool::new(id, "did it").with_capabilities([unvouched.clone()]),
        ));
    }
    // one of them came from a server this program spawned, and the other is a tool that declares
    // the same subject with nobody holding the far end of it
    harness.app.policy.came_from("files__read", "files");
    harness
        .app
        .policy
        .set(&Subject::Capability(unvouched), Verdict::Allow);
    harness.tab(Tab::Permissions);

    let screen = harness.sized(110, 30);
    let row = screen
        .lines()
        .find(|line| line.contains("mcp:call"))
        .unwrap_or_else(|| panic!("the rule is listed: {screen}"))
        .to_owned();

    assert!(
        !row.contains("files__read"),
        "the server answers for that one, not this rule: {row}"
    );
    assert!(
        !row.contains("nothing here is judged by it")
            && !row.contains("nothing registered needs it"),
        "and something is: the tool with nobody holding the far end of it: {row}"
    );

    // take that one away and the rule is inert, which is the row's other answer and the one it
    // owes somebody who wrote a flag that decides nothing
    harness.app.kernel.remove_tool("loose");
    let screen = harness.sized(110, 30);
    let row = screen
        .lines()
        .find(|line| line.contains("mcp:call"))
        .unwrap_or_else(|| panic!("somebody wrote the rule, so it is still listed: {screen}"));
    assert!(row.contains("nothing registered needs it"), "{row}");
}

/// And a domain rule names every capability in the domain it answers for, and nothing else.
///
/// note: the domain arm lists the capabilities a rule answers for rather than the tools it
/// reaches. `mcp:call` is the one that must not appear beside them: every tool from a server
/// declares it and `Careful::judges` swaps it for the server's own name, so an `fs` row that
/// read `fs:read, mcp:call` would be naming an operation the rule is never consulted about - a
/// `deny` for `mcp:call` is, and covers those tools, which is why the refusal is the case here.
#[tokio::test]
async fn a_domain_rule_names_the_capabilities_it_answers_for_and_no_others() {
    let harness = Harness::new(Vec::new());
    let unvouched = Capability::of(nachalnik::Domain::Other("mcp".to_owned()), "call");
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("files__read", "did it").with_capabilities([unvouched.clone()]),
    ));
    harness.app.policy.came_from("files__read", "files");
    // a refusal *is* consulted about a server's own tool, so it is the one capability outside
    // `fs` a rule about `fs` still must not claim
    harness
        .app
        .policy
        .set(&Subject::Capability(unvouched), Verdict::Deny);
    harness
        .app
        .policy
        .set(&Subject::Domain(Domain::Fs), Verdict::Allow);

    let row = harness
        .app
        .permissions()
        .into_iter()
        .find(|row| row.subject == Subject::Domain(Domain::Fs))
        .expect("the rule somebody wrote is listed");
    assert_eq!(
        row.covers(),
        "fs:read",
        "every capability in the domain it answers for, and nothing else"
    );
}

/// And a capability the server answers for is not a question this session has, either.
///
/// note: the other half of the same swap, on the count rather than on a row. Every tool from a
/// server declares `mcp:call` and none of them is judged by it, so a session run `--mcp big=...`
/// with nobody having written a rule about `mcp` counted a subject it will never stop and ask
/// about among the ones it will - the figure somebody reads to know how much of the session is
/// still undecided.
#[tokio::test]
async fn a_capability_a_server_answers_for_is_not_a_question_this_session_has() {
    let mut harness = Harness::new(Vec::new());
    harness.tab(Tab::Permissions);
    let untold = harness.app.undecided();

    let unvouched = Capability::of(nachalnik::Domain::Other("mcp".to_owned()), "call");
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("files__read", "did it").with_capabilities([unvouched.clone()]),
    ));
    harness.app.policy.came_from("files__read", "files");

    // the server is what a call to it is judged as, so what was added is that server and nothing
    // else - `mcp:call` is not a decision here and not a question here
    assert_eq!(harness.app.permissions().len(), 0);
    assert_eq!(harness.app.undecided(), untold + 1);

    // a tool declaring the same thing with nobody holding the far end of it really will be asked
    // about under that name, and it counts
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("loose", "did it").with_capabilities([unvouched]),
    ));
    assert_eq!(harness.app.undecided(), untold + 2);
}

#[tokio::test]
async fn a_shell_reaching_for_the_network_is_judged_as_reaching_for_it() {
    use kamchatka::tools::reaches_the_network as reaches;

    // what a model writes when it wants the network
    assert!(reaches("curl https://example.com"));
    assert!(reaches("pip install requests"));
    assert!(reaches("git push origin master"));
    assert!(reaches("wc -l x.py && curl -s https://example.com"));
    assert!(reaches(
        "HTTPS_PROXY=http://p:8080 curl https://example.com"
    ));
    assert!(reaches("/usr/bin/curl https://example.com"));

    // and what it writes the rest of the time
    assert!(!reaches("wc -l ledger.py"));
    assert!(!reaches("cat curl.txt"));
    assert!(!reaches("echo 'curl is a program'"));
    assert!(!reaches(""));
}

#[tokio::test]
async fn a_denied_network_reaches_the_shell_that_would_have_used_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "curl https://example.com" }),
        )]),
        ModelResponse::text("refused, then"),
    ]);
    // a stand-in for the shell: what is under test is the policy reading the command, not the
    // program that would run it
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));
    // the default is a question, not a refusal: reaching the network is a thing somebody may
    // perfectly well want, and the sandbox is what makes either answer mean something
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::net("reach"))),
        Verdict::Ask
    );
    harness.app.policy.set(
        &Subject::Capability(Capability::net("reach")),
        Verdict::Deny,
    );

    harness.send("fetch it").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        !screen.contains("allow this?"),
        "a refusal is not a question: {screen}"
    );
    let refused = harness
        .app
        .kernel
        .items()
        .iter()
        .any(|item| item.content.to_text().contains("permitted"));
    assert!(refused, "the model is told, rather than left waiting");

    // and the screen says which stance did it. Without this the tab reads `shell: ask` beside a
    // refused shell call and nothing anywhere accounts for the refusal
    assert!(
        screen.contains("net:reach"),
        "a refusal nobody can account for is the thing this program is not for: {screen}"
    );
}

#[tokio::test]
async fn the_permissions_tab_admits_what_a_shell_can_do() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
    ));
    harness.tab(Tab::Permissions);

    // nothing here runs commands, so the five verdicts are the whole story
    let screen = harness.sized(120, 30);
    assert!(!screen.contains("shell:"), "{screen}");

    // ... and once something does, the tab has to account for it: `shell` subsumes every other
    // row unless something is confining it, and nothing is here
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("sh", "output").with_capabilities([Capability::exec("run")]),
    ));
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: a command can do any of these"),
        "{screen}"
    );

    // ... and when something is, it says what a command can reach instead of what it cannot
    harness.app.confinement = Confinement::Full;
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: confined, network not gated"),
        "{screen}"
    );
    assert!(
        !screen.contains("can do any of these"),
        "a confined shell cannot: {screen}"
    );
    // and whether its network is asked about when it tries, which is the difference between a UDP
    // datagram going out and not, and nowhere else on the screen
    harness.app.policy.gate_the_network();
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: confined, network gated"),
        "{screen}"
    );
    // ... and one the kernel took only part of says so, rather than being rounded up to confined
    // or down to nothing
    harness.app.confinement = Confinement::Partial;
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: partly confined, network gated"),
        "{screen}"
    );

    // and a sandbox that was asked for and could not be applied is not `--no-sandbox`, which a
    // person chose and needs no telling about
    harness.app.confinement = Confinement::Unavailable;
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: could not be confined, so a command can do any of these"),
        "{screen}"
    );
    // and why, where something said: a kernel with Landlock that refused this ruleset is a
    // different problem from one with none
    harness.app.unconfined_because = Some("the ruleset could not be applied: EPERM".to_owned());
    // wide enough for the reason on the one line the row has
    let screen = harness.sized(200, 30);
    assert!(
        screen.contains("shell: could not be confined (the ruleset could not be applied: EPERM)"),
        "{screen}"
    );

    // refusing it outright puts the other rows back in charge either way
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);
    let screen = harness.sized(120, 30);
    assert!(!screen.contains("shell:"), "{screen}");
}

#[tokio::test]
async fn a_path_rule_is_finer_than_the_capability_above_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "path": "src/main.rs" }))]),
        ModelResponse::tool_calls(vec![call("c2", "read", json!({ "path": ".env" }))]),
        ModelResponse::text("as you wish"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));

    // answered for the capability and nothing else, which is what `always` on an ordinary read
    // does. The ordinary file is no longer a question and `.env` still is, because the strictest
    // of what is consulted wins and a rule about the *file* is finer than a verdict about the tool
    // that opened it. One turn, both reads, one question
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    harness.send("read both").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        screen.contains("src/main.rs") && screen.contains("contents"),
        "the ordinary one ran without being asked about: {screen}"
    );
    let asked = screen
        .lines()
        .find(|line| line.contains("wants:"))
        .expect("this one is a question");
    assert!(asked.contains(".env*"), "the rule is named: {asked}");
    assert!(
        asked.contains("fs:read"),
        "and so is the operation: {asked}"
    );
}

#[tokio::test]
async fn saying_always_answers_for_everything_the_question_named() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "path": ".env" }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness.send("read it").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "the rule made it a question");

    // `a` has to answer the path rule as well as the capability. Answering for `read` alone would
    // leave the question exactly where it was, since the rule is the stricter of the two
    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Path(".env*".to_owned())),
        Verdict::Allow,
        "the rule that raised the question is the one that was answered"
    );
}

/// The permissions tab says which policy is in force and what it does with the rest.
///
/// note: the tab was every answer somebody had given and no account of what was deciding in
/// between - so the first question a screen of permissions raises was the one thing not on it, and
/// answering it meant reading `/seams` for the name and the source for the behaviour. Both halves
/// come out of the policy: the name is what the kernel answers when asked, and the verdict is the
/// value `Careful::stance` falls back to, so neither can drift from what actually happens.
#[tokio::test]
async fn the_permissions_tab_says_which_policy_is_deciding() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness.tab(Tab::Permissions);

    // with nothing decided, which is when it matters most: the list is empty and something has to
    // say the emptiness is not permission
    let empty = harness.flat();
    assert!(empty.contains("Careful"), "the policy is named: {empty}");
    assert!(
        empty.contains("anything it has not been told about: ask"),
        "and says what it does with everything not listed: {empty}"
    );
    assert!(
        empty.contains("empty rather than permissive"),
        "an empty list is not an open one, and the tab has to say so: {empty}"
    );

    // and it is still there once there are rows, because it is not an empty-state message
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    let filled = harness.flat();
    assert!(filled.contains("Careful"), "{filled}");
    assert!(filled.contains("fs:read allow fs:read"), "{filled}");

    // the short name, not the path `PermissionPolicy::name` defaults to - that belongs to /seams
    assert!(
        !filled.contains("kamchatka::tools"),
        "thirty columns of a list spent saying `Careful`: {filled}"
    );
    // at the margin rather than indented under itself like a row: the two columns the rows share
    // put it in the `capability` column, reading as the table's first and oddest entry
    let screen = harness.sized(84, 12);
    assert!(
        screen.lines().any(|line| line.starts_with("│Careful")),
        "{screen}"
    );
    // and the verdict is read off the policy rather than written into the screen
    assert_eq!(kamchatka::tools::Careful::untold(), Verdict::Ask);
}

/// The line saying which policy is deciding stands above the table by exactly one blank row.
///
/// note: the block holding it is sized from what is in it - the policy's line, and in a build that
/// can rate commands a second line saying that nothing is rating them - and from nothing else. A
/// row too tall pushes the table down the screen for no reason, and a row too short draws the
/// heading over the line saying what the table is. That line is what somebody reads first.
#[tokio::test]
async fn the_policy_line_is_the_only_row_above_the_table() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    harness.tab(Tab::Permissions);

    let screen = harness.sized(60, 12);
    let rows: Vec<&str> = screen.lines().collect();

    // the strip of tabs, then what is deciding - a line, or two in a build that can rate commands
    // - then the one blank row that keeps the table off it, and the heading after that, which is
    // where somebody looking for the answers looks
    assert!(
        rows[1].starts_with("│Careful"),
        "what is deciding is the first thing under the strip: {screen}"
    );
    let heading = rows
        .iter()
        .position(|row| row.contains("capability or path"))
        .unwrap_or_else(|| panic!("the table has a heading: {screen}"));
    let blank = |row: &str| row.replace('│', "").trim().is_empty();
    assert!(
        blank(rows[heading - 1]),
        "a gap between what is deciding and the heading: {screen}"
    );
    assert!(
        rows[2..heading - 1].iter().all(|row| !blank(row)),
        "and only the one: {screen}"
    );
}

/// A window too narrow for the columns to be their usual widths still gets all three of them.
///
/// note: `22.min(width / 3)` and the width left over for what a rule covers are settled here and
/// nowhere else, and a terminal is as narrow as somebody chooses to make it. What a rule covers is
/// what somebody checking a flag came to read, so it is the column that has to be there at forty
/// columns as much as at a hundred and ten, and the two columns beside it are what pays for it.
#[tokio::test]
async fn a_narrow_window_divides_a_row_between_its_three_columns() {
    let mut harness = Harness::new([]);
    for id in ["alpha", "beta", "gamma"] {
        harness.app.kernel.add_tool(Arc::new(
            ConstTool::new(id, "a tool").with_capabilities([Capability::fs("read")]),
        ));
        harness.app.policy.came_from(id, "files");
    }
    harness
        .app
        .policy
        .set(&Subject::Server("files".to_owned()), Verdict::Allow);
    harness.tab(Tab::Permissions);

    // thirty-eight columns inside the border, a third of them for the subject, and what is left
    // after the subject and the answer for what the rule covers
    let screen = harness.sized(40, 12);
    let row = screen
        .lines()
        .find(|line| line.contains("server files"))
        .unwrap_or_else(|| panic!("the rule is listed: {screen}"));
    assert_eq!(
        row, "│  server files allow       alpha, bet…│",
        "all three columns, and what it covers in the eleven that are left: {screen}"
    );
}

#[tokio::test]
async fn the_permissions_tab_draws_the_path_rules_too() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness.tab(Tab::Permissions);

    // nobody has decided anything about `.env*`, so it is not a row - it is one of the things the
    // footer counts, and it arrives here the moment somebody answers a question about it
    let screen = harness.sized(120, 40);
    assert!(!screen.contains(".env*"), "{screen}");
    assert!(
        screen.contains("more it will ask about"),
        "what is not listed is still counted: {screen}"
    );

    harness
        .app
        .policy
        .set(&Subject::Path(".env*".to_owned()), Verdict::Deny);

    let screen = harness.sized(120, 40);
    let rule = screen
        .lines()
        .find(|line| line.contains(".env*"))
        .expect("a decision about a path is a decision, and is drawn like any other");
    assert!(rule.contains("deny"), "{rule}");
    assert!(rule.contains("read"), "it names the tools it binds: {rule}");

    // ... and it cycles like any other row
    pick(&mut harness, &Subject::Path(".env*".to_owned())).await;
    harness.press(KeyCode::Char(' ')).await;
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Path(".env*".to_owned())),
        Verdict::Ask
    );
}

#[tokio::test]
async fn a_refusal_says_which_stance_made_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({ "cmd": "rm -rf /tmp/x" }))]),
        ModelResponse::text("no, then"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));
    // this time it is the tool's own capability that is refused, not something the command reached
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

    harness.send("clean up").await;
    harness.settle().await;

    let screen = harness.screen();
    let note = screen
        .lines()
        .find(|line| line.contains("refused by"))
        .expect("the refusal is accounted for");
    assert!(note.contains("exec:run"), "{note}");
    assert!(
        !note.contains("net:reach"),
        "the command never reached for it: {note}"
    );

    // and the other refusal there is: a rule about something else in the call, on a tool whose own
    // capability is allowed. `shell` is allowed, so nothing about the tool says no - the path does
    // - and a session left with only what the tool result said was left with a refused call and
    // nothing accounting for it. The reason is said here as well, under the tool's own name
    let mut other = Harness::new([
        ModelResponse::tool_calls(vec![call("c2", "shell", json!({ "path": ".env" }))]),
        ModelResponse::text("no, then"),
    ]);
    other.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));
    other.app.policy.set(
        &Subject::Capability(Capability::exec("run")),
        Verdict::Allow,
    );
    other
        .app
        .policy
        .set(&Subject::Path(".env*".to_owned()), Verdict::Deny);

    other.send("read the env").await;
    other.settle().await;

    let note = other
        .app
        .loose
        .iter()
        .map(|entry| (entry.speaker, entry.text.as_str()))
        .find(|(speaker, _)| *speaker == Speaker::Note)
        .unwrap_or_else(|| panic!("nothing accounted for the refusal"));
    assert_eq!(note.0, Speaker::Note);
    assert!(
        note.1.starts_with("shell: ") && note.1.contains(".env*"),
        "the refusal is named against the tool that was refused: {}",
        note.1
    );
}

/// Puts the cursor on the row for `subject`, which has to be a decision for it to be listed.
async fn pick(harness: &mut Harness, subject: &Subject) {
    let at = harness
        .app
        .permissions()
        .iter()
        .position(|row| row.subject == *subject)
        .unwrap_or_else(|| {
            panic!(
                "the tab lists decisions, and nobody has decided about `{subject}`: {:?}",
                harness
                    .app
                    .permissions()
                    .iter()
                    .map(|row| row.subject.to_string())
                    .collect::<Vec<_>>()
            )
        });
    harness.app.chosen = at;
}

#[tokio::test]
async fn changing_a_permission_changes_what_happens_next() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
        ModelResponse::text("as you wish"),
        ModelResponse::tool_calls(vec![call("c2", "rm", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));

    // shell is a question by default, so this turn stops and asks
    harness.send("tidy up").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some());
    // answering resumes the turn, so let it finish before starting another
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    // `n` at the prompt refuses that call and decides nothing, so the tab still has nothing to
    // say about the shell. It lists decisions
    assert!(
        !harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == Subject::Capability(Capability::exec("run"))),
        "a refusal of one call is not a decision about the capability"
    );

    // deciding it *is* what puts it there, and changes what the same call does next time
    harness.app.policy.set(
        &Subject::Capability(Capability::exec("run")),
        nachalnik::Verdict::Deny,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::exec("run"))).await;
    harness.press(KeyCode::Char('a')).await;

    assert_eq!(
        harness.app.permissions()[harness.app.chosen].verdict,
        nachalnik::Verdict::Allow
    );
    harness.tab(Tab::Chat);
    assert!(harness.screen().contains("runs without asking"));

    harness.send("tidy up again").await;
    harness.settle().await;

    assert!(
        harness.app.overlay.is_none(),
        "it was decided in advance, so there is nothing to ask"
    );
    let screen = harness.screen();
    assert!(screen.contains("gone"), "the tool ran: {screen}");
}

#[tokio::test]
async fn a_capability_can_be_refused_outright_rather_than_asked_about() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
        ModelResponse::text("fine, I will not"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));

    // the tab lists decisions, so there is one to make first: `a` at a question is what puts a
    // subject on it, and `n` there is what changes its mind
    harness.app.policy.set(
        &Subject::Capability(Capability::exec("run")),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::exec("run"))).await;
    harness.press(KeyCode::Char('n')).await;

    harness.tab(Tab::Chat);
    harness.send("tidy up").await;
    harness.settle().await;

    // no question, no run, and the model is told rather than left guessing
    assert!(harness.app.overlay.is_none(), "a refusal is not a question");
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(!sent.contains("gone"), "it should not have run: {sent}");
    assert!(sent.contains("not permitted"), "{sent}");
}

#[tokio::test]
async fn cycling_a_permission_goes_round_rather_than_getting_stuck() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    harness.app.policy.set(
        &Subject::Capability(Capability::exec("run")),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::exec("run"))).await;

    use nachalnik::Verdict::{Allow, Ask, Deny};
    let shell = Subject::Capability(Capability::exec("run"));
    let mut seen = vec![harness.app.policy.stance(&shell)];
    for _ in 0..2 {
        harness.press(KeyCode::Char(' ')).await;
        seen.push(harness.app.policy.stance(&shell));
    }

    assert_eq!(seen, vec![Allow, Deny, Ask], "space goes allow, deny, ask");

    // ... and arriving back at `ask` takes the row off the tab, because `ask` is not a decision -
    // it is what the policy does when nobody has made one. That is what taking a decision back
    // looks like here, and the subject returns the next time somebody answers a question about it
    assert!(
        !harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == shell),
        "cycling back to `ask` is undeciding it"
    );
    harness.app.policy.set(&shell, Allow);
    assert!(
        harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == shell)
    );
}

/// A decision can be taken back from the row it was made on, and taking it back takes the row off
/// the tab.
///
/// note: `ask` is what this policy does about everything nobody has answered for, and the tab
/// lists answers rather than defaults - so `r` does not draw a third word on the row, it removes
/// it, and the next question about that subject is asked again.
#[tokio::test]
async fn a_decision_can_be_taken_back_from_the_row_it_is_on() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found").with_capabilities([Capability::fs("read")]),
    ));
    let shell = Subject::Capability(Capability::exec("run"));
    let read = Subject::Capability(Capability::fs("read"));
    for subject in [&shell, &read] {
        harness.app.policy.set(subject, Verdict::Deny);
    }
    harness.tab(Tab::Permissions);

    // `r` and backspace are one gesture, and the tab says so on both of them
    for (key, subject) in [
        (KeyCode::Char('r'), shell.clone()),
        (KeyCode::Backspace, read.clone()),
    ] {
        pick(&mut harness, &subject).await;
        harness.press(key).await;

        assert_eq!(
            harness.app.policy.stance(&subject),
            Verdict::Ask,
            "{key:?} puts it back to being a question"
        );
        assert!(
            !harness
                .app
                .permissions()
                .iter()
                .any(|row| row.subject == subject),
            "and an undecided subject is not a row: {key:?}"
        );
        assert!(
            ruled(&harness).contains(&(subject.to_string(), Verdict::Ask, None, false)),
            "{key:?} is in the record like any other decision: {:?}",
            ruled(&harness)
        );
    }
}

#[tokio::test]
async fn ctrl_d_leaves_even_when_a_tool_is_waiting_to_run() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "danger",
        json!({}),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "a question is up");

    // `d` is a key at this prompt, and the overlay used to be dispatched before anything looked
    // at the modifiers - so the key people press to leave dropped every pending call instead
    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await;

    assert!(
        harness.app.quit,
        "ctrl+d means leave, wherever it is pressed"
    );
    assert_eq!(
        harness.app.kernel.pending_permissions().len(),
        1,
        "and it does not answer the question on the way out"
    );
}

/// The two keys mean different things depending on whether there is a turn to stop, and each of
/// them keeps its own meaning.
///
/// note: `ctrl+c` stops a turn and `ctrl+d` leaves, but with nothing running there is no turn to
/// stop, so `ctrl+c` leaves as well - otherwise a session at rest could not be ended from the
/// keyboard at all. The other side is the same: with a turn running, `ctrl+d` still means leave
/// and must not be read as a request to stop the turn, which is what taking the first key's
/// meaning for every key here would do.
#[tokio::test]
async fn each_of_the_two_keys_means_the_same_thing_whatever_is_running() {
    let mut idle = Harness::new([]);
    idle.app
        .on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(
        idle.app.quit,
        "with no turn to stop, `ctrl+c` leaves: it is the way a session at rest ends"
    );

    let mut running = Harness::new([]);
    running.app.busy = true;
    running
        .app
        .on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await;
    assert!(
        running.app.quit,
        "`ctrl+d` means leave whether or not there is a turn to stop"
    );
    assert!(
        !running.app.kernel.is_interrupted(),
        "and it did not stop the turn on its way out"
    );
}

#[tokio::test]
async fn dropping_the_calls_hands_the_turn_back_to_the_model() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "danger", json!({}))]),
        ModelResponse::text("all right, something else then"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    harness.answer(KeyCode::Char('d')).await;

    // answering `n` to each resumes the turn; dropping them all used to leave the kernel idle
    // with the refusals recorded and nobody driving, until somebody noticed and typed /continue
    harness.settle().await;
    let screen = harness.screen();
    assert!(
        screen.contains("all right, something else then"),
        "{screen}"
    );
}

#[tokio::test]
async fn saying_always_answers_for_the_calls_already_waiting() {
    let mut harness = Harness::new([
        // one answer, three calls: the model asked for all of them before anybody was asked
        // about any of them, and all three questions exist before the first is drawn
        ModelResponse::tool_calls(vec![
            call("c1", "dig", json!({ "where": "one" })),
            call("c2", "dig", json!({ "where": "two" })),
            call("c3", "dig", json!({ "where": "three" })),
        ]),
        ModelResponse::text("all three"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig three times").await;
    harness.settle().await;
    assert_eq!(
        harness.app.kernel.pending_permissions().len(),
        3,
        "one question each"
    );

    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    // "always" said what happens from now on, and the other two were already in the queue when it
    // was said. Asking about them again would be the prompt going back on it one keystroke later
    assert!(
        harness.app.kernel.pending_permissions().is_empty(),
        "the rest of the batch was answered by the same `always`"
    );
    assert!(harness.app.overlay.is_none());
    let results = harness
        .app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.label == "dig")
        .count();
    assert_eq!(results, 3, "and all three of them ran");
}

#[tokio::test]
async fn saying_always_leaves_a_waiting_call_that_needs_something_else_a_question() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![
            call("c1", "dig", json!({ "path": "src/main.rs" })),
            call("c2", "dig", json!({ "path": ".env" })),
        ]),
        ModelResponse::text("both"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig twice").await;
    harness.settle().await;

    // no `settle` after this one: there is still a question, so no turn was started to wait for
    harness.answer(KeyCode::Char('a')).await;

    // `shell` is allowed from now on and the second call is still a question, because the rule
    // about `.env` is not what anybody just answered
    let waiting = harness.app.kernel.pending_permissions();
    assert_eq!(waiting.len(), 1, "the one with a rule of its own");
    assert_eq!(waiting[0].args["path"], ".env");
    assert!(harness.app.asked().is_some());
}

#[tokio::test]
async fn a_question_that_arrives_under_somebody_s_fingers_is_not_answered_by_them() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "the question is up");

    // the question has the prompt's place, so there is nowhere for this to go - and `a` is the
    // third letter of it. It used to grant `shell` for the rest of the session, which is what a
    // live run did
    for c in "what is the capital of Peru".chars() {
        harness.press(KeyCode::Char(c)).await;
    }

    assert!(
        harness.app.asked().is_some(),
        "the question is still waiting to be read"
    );
    assert_eq!(
        harness.app.focus,
        Focus::Input,
        "and it never took the keys to begin with"
    );
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::exec("run"))),
        Verdict::Ask,
        "nothing was granted by somebody typing a sentence"
    );
    // ... and none of it reached the prompt either, because the prompt is not on the screen. This
    // used to be a race the program could only mostly win: the question took every key and handed
    // back the ones that were not answers, so a 300ms timer decided which. The first letter of a
    // sentence arrives after a pause, so the timer had already expired by the time it landed, and
    // it was `a` a few keys later that did the damage. Nothing is timed now, and nothing is
    // swallowed by a box somebody cannot see
    assert_eq!(harness.app.input.lines(), [""]);
    assert!(
        !harness.screen().contains("┌ you "),
        "the prompt gave up its place to the question: {}",
        harness.screen()
    );

    // `enter` is guarded by the same gate, which is most of why it is a gate: unswallowed it would
    // send whatever the question interrupted and start a turn on the way to answering
    harness.press(KeyCode::Enter).await;
    assert!(
        harness.app.asked().is_some(),
        "and the question is still the question"
    );
    assert_eq!(
        harness.app.kernel.state().name(),
        "deciding",
        "nothing was sent, so nothing was started"
    );

    // and `tab` puts the keys on it, where one key answers it
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.asked().is_none(), "answered");
    assert_eq!(
        harness.app.focus,
        Focus::Input,
        "and the keys come back to the prompt with nothing left to ask"
    );
}

/// A tool's question is answered by the keys, whatever else is standing in the prompt's place.
///
/// note: a pass's list is not a reason to answer some other question with its keys. A compaction
/// is refused while a tool is waiting, so the two are never up at once -
/// and when they somehow are, the keys belong to the tool, which is the one holding a turn still.
/// The panel drew the tool's question, because `question_parts` gives it first.
#[tokio::test]
async fn a_tool_s_question_is_answered_even_while_a_compaction_is_standing_there() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "a tool is waiting");

    // a pass cannot be asked for while a question waits - `compact` refuses it - so this is the
    // one state the guard is here to survive: both standing there, and only one of them drawn
    harness.app.proposed = Some(kamchatka::app::Proposed {
        rows: vec!["[1] something the pass would take".to_owned()],
        count: 1,
        holding: 5,
    });

    harness.press(KeyCode::Tab).await;
    let screen = harness.screen();
    assert!(screen.contains("a tool wants to run"), "{screen}");

    // `y` is the tool's answer. Read as a pass's, it took the pass instead and left the call
    // waiting - a question on the screen that one key could not answer
    harness.press(KeyCode::Char('y')).await;
    assert!(
        harness.app.asked().is_none(),
        "`y` answered the tool rather than the compaction"
    );
    assert!(
        harness.app.proposed.is_some(),
        "and left the pass alone: nothing was compacted"
    );
}

/// The answers are separated from what the tool was asked to do, and the blank goes first.
///
/// note: `path: /etc/hosts` and `[y] once` on consecutive rows read as one list of things rather
/// than as a question and the ways of answering it - and the header was already separated from the
/// arguments this way, so the answers were the odd ones out. It is a row of the layout rather than
/// a line of the answers, which is what makes it the first thing to give way: in the answers it
/// would be the top line of the one region that gets its rows before anything else, so a panel
/// with a single row to spare would have spent it on a blank and pushed `[y] once` off the bottom.
#[tokio::test]
async fn a_question_separates_its_answers_from_what_it_is_about() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "read",
        json!({ "path": "/etc/hosts" }),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness.send("go").await;
    harness.settle().await;
    // the keys on it, so the answers are the first line of the region rather than the `[tab]` one
    harness.press(KeyCode::Tab).await;

    let roomy = harness.sized(76, 24);
    let rows: Vec<&str> = roomy.lines().collect();
    let answers = rows
        .iter()
        .position(|line| line.contains("[y] once"))
        .unwrap_or_else(|| panic!("the answers are on the screen: {roomy}"));
    assert!(
        rows[answers - 1].trim_matches(['│', ' ']).is_empty(),
        "a blank row between the two: {roomy}"
    );
    assert!(
        rows[answers - 2].contains("/etc/hosts"),
        "and the argument above that: {roomy}"
    );

    // and with no room for it, what goes is the blank rather than an answer
    let short = harness.sized(76, 10);
    for line in ["[y] once", "[i] the exact JSON"] {
        assert!(
            short.contains(line),
            "{line:?} should survive a short screen: {short}"
        );
    }

    // the one size where there is nothing spare at all: the header, one row of arguments and the
    // answers come to exactly the rows there are, so the blank has nothing left to take and the
    // row that would have been it belongs to the argument instead. Spending it on a blank here is
    // what the note above is against, and what is left without it is a question naming a tool
    // and asking about nothing
    let exact = harness.sized(100, 11);
    let rows: Vec<&str> = exact.lines().collect();
    let answers = rows
        .iter()
        .position(|line| line.contains("[y] once"))
        .unwrap_or_else(|| panic!("the answers are on the screen: {exact}"));
    assert!(
        rows[answers - 1].contains("/etc/hosts"),
        "the row the blank would have been is the argument: {exact}"
    );
}

/// A half-written message is still there after the question that interrupted it.
///
/// note: the question takes the prompt's place rather than stacking above it, so the box holding
/// the keys is always the box on the screen - stacked, the two disagreed below about fifteen rows,
/// where the question needs the room and the prompt gave way while keeping the keys and the text.
/// What that costs is that the draft goes off the screen for as long as the question is up, so the
/// thing worth pinning is that it comes back, and comes back with the keys on it.
#[tokio::test]
async fn the_question_gives_the_prompt_its_place_back_with_what_was_in_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    // a draft typed while the turn runs, before there is any question to interrupt it
    harness.send("dig").await;
    for c in "and what about".chars() {
        harness.press(KeyCode::Char(c)).await;
    }
    assert_eq!(harness.app.input.lines(), ["and what about"]);

    harness.settle().await;
    assert!(harness.app.asked().is_some(), "the turn stopped to ask");
    // the prompt is off the screen and the question is where it was, saying what to press
    let screen = harness.flat();
    assert!(!screen.contains("┌ you "), "{screen}");
    assert!(screen.contains("a tool wants to run · tab"), "{screen}");
    assert!(
        screen.contains("[tab] puts the keys here"),
        "the answers do not work yet, so the panel says which key does: {screen}"
    );

    // answering hands the place back, with the draft in it and the keys on it - no second `tab`
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert_eq!(harness.app.focus, Focus::Input);
    assert_eq!(
        harness.app.input.lines(),
        ["and what about"],
        "the draft the question interrupted"
    );
    let screen = harness.flat();
    assert!(screen.contains("and what about"), "{screen}");
    assert!(
        !screen.contains("[tab] puts the keys here"),
        "and the gate is gone with the question: {screen}"
    );
}

#[tokio::test]
async fn a_message_sent_into_a_turn_that_stops_to_ask_waits_for_the_answer_too() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("dug, and Lima"),
        ModelResponse::text("Lima"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    // typed while that turn is running, and the turn then stops to ask about the call
    harness.send("what is the capital of Peru").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "the turn stopped to ask");

    // it must not have gone in yet: the call it stopped at has a result still to come, and a user
    // message between an assistant's call and that call's result is a shape a request cannot have
    let waiting: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(
        !waiting.iter().any(|text| text.contains("capital of Peru")),
        "{waiting:?}"
    );

    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;

    let after: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    let question = after
        .iter()
        .position(|text| text.contains("capital of Peru"))
        .expect("it went in once the turn was done");
    let result = after
        .iter()
        .position(|text| text.contains("a bone"))
        .expect("the call's result");
    assert!(result < question, "{after:?}");
}

/// The whole of why a question is pinned rather than laid over the screen: it is a question about
/// something, and the something is on another tab.
///
/// note: this is the case `App::about` was written for and could only paper over. A question about
/// eliding item 22 was asked by a box that covered the list saying what 22 was, so the labels had
/// to be read into the question itself - and anything the question did not think to copy was
/// unreachable until it had been answered. Now the answer is to go and look.
#[tokio::test]
async fn a_waiting_question_can_be_left_and_come_back_to() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "context", json!({ "ids": [1] }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("context", "elided")
            .with_capabilities([kamchatka::tools::domains::context("elide")]),
    ));
    harness
        .app
        .kernel
        .push(ContextItem::file("secrets.txt", "a password, probably"));

    harness.send("tidy the context").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "a question is waiting");
    assert!(
        harness.screen().contains("a tool wants to run"),
        "pinned on the chat tab: {}",
        harness.screen()
    );

    // away to the context tab, which the question used to make unreachable. `tab` is not the way:
    // on the chat tab it moves the keys onto the question, which is a different gesture
    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.tab, Tab::Chat, "`tab` is not a tab switch here");
    harness.chord(KeyCode::Char('t')).await;
    assert_eq!(harness.app.tab, Tab::Context);
    let context = harness.screen();
    assert!(context.contains("secrets.txt"), "{context}");
    assert!(
        !context.contains("a tool wants to run"),
        "and the question is not following: {context}"
    );
    assert!(
        harness.app.asked().is_some(),
        "but it is still waiting to be answered"
    );

    // the strip is what says so from over here
    assert_eq!(
        harness.tab_colour("chat"),
        Color::Red,
        "the chat tab is where the answer has to go"
    );

    // reading an item is what somebody came here to do, and it decides nothing
    harness.press(KeyCode::Enter).await;
    assert!(
        harness.screen().contains("a password"),
        "{}",
        harness.screen()
    );
    harness.press(KeyCode::Esc).await;
    assert!(
        harness.app.asked().is_some(),
        "and none of that answered anything"
    );

    // and back, where the keys are already on the question because that is what the trip was for
    harness.app.show(Tab::Chat);
    assert_eq!(harness.app.focus, Focus::Body);
    // and the box the keys are on is the one drawn in the accent. Red and the accent are two
    // statements about the same panel - *a tool is waiting on somebody* and *the keys are here* -
    // and they are read together, because a person who set the frame red would otherwise have
    // configured the second distinction away
    assert_eq!(
        harness.corners(),
        vec![harness.app.accent, harness.app.accent],
        "the window, and the question that has the keys"
    );
    harness.press(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.asked().is_none(), "one key, having looked");
    assert_ne!(
        harness.tab_colour("chat"),
        Color::Red,
        "and the strip stops saying otherwise"
    );
}

/// A question about a call that names its items twice describes no items at all.
///
/// note: the overlay reads the arguments the tool will read, and a call giving `ids` and `select`
/// is one the tool refuses - there is no set of items it is about. Naming the selector's matches
/// anyway would put a list of forty rows in front of somebody, over a call that is going to move
/// none of them whichever way they answer.
#[tokio::test]
async fn a_question_naming_its_items_twice_is_not_described_as_a_move() {
    async fn asked_about(args: serde_json::Value) -> Vec<String> {
        let mut harness = Harness::new([
            ModelResponse::tool_calls(vec![call("c1", "context", args)]),
            ModelResponse::text("done"),
        ]);
        harness.app.kernel.add_tool(Arc::new(
            ConstTool::new("context", "elided")
                .with_capabilities([kamchatka::tools::domains::context("elide")]),
        ));
        harness
            .app
            .kernel
            .push(ContextItem::file("secrets.txt", "a password, probably"));

        harness.send("tidy the context").await;
        harness.settle().await;
        let asked = harness.app.asked().expect("a question is waiting");

        harness.app.about(&asked)
    }

    // a class on its own is the argument most worth expanding, because nobody can count what
    // `files` comes to off the screen this question is covering
    let one_way = asked_about(json!({
        "action": "elide", "select": "files", "reason": "tidying",
    }))
    .await;
    assert!(
        one_way.iter().any(|line| line.contains("secrets.txt")),
        "{one_way:?}"
    );

    // and with the numbers beside it there is no set of items to name, because the call moves
    // nothing whichever way it is answered
    let both = asked_about(json!({
        "action": "elide", "ids": [1], "select": "files", "reason": "tidying",
    }))
    .await;
    assert!(both.is_empty(), "{both:?}");
}

/// A question naming more items than it can list says how many it left out - and only when it did.
///
/// note: eight is what the panel has room for, and a call naming nine items reads as one about the
/// first eight of them. So the ninth is counted, and a call naming exactly eight has nothing left
/// over to say: a line reading "… and 0 more" beside a list of every item it named would be the
/// one this guards against.
#[tokio::test]
async fn a_question_about_more_items_than_it_can_list_says_how_many_it_left_out() {
    async fn asked_about(items: usize) -> Vec<String> {
        let mut harness = Harness::new([
            ModelResponse::tool_calls(vec![call(
                "c1",
                "context",
                json!({ "action": "elide", "ids": (1..=items).collect::<Vec<_>>() }),
            )]),
            ModelResponse::text("done"),
        ]);
        harness.app.kernel.add_tool(Arc::new(
            ConstTool::new("context", "elided").with_capabilities([Capability::fs("glob")]),
        ));
        // pushed before the question, so they hold the identifiers 1 to `items` that the
        // arguments name; the message that asks for it is one more, and is not named
        for n in 1..=items {
            harness
                .app
                .kernel
                .push(ContextItem::file(format!("f{n}.txt"), "a little"));
        }

        harness.send("tidy the context").await;
        harness.settle().await;
        let asked = harness.app.asked().expect("a question is waiting");

        harness.app.about(&asked)
    }

    let nine = asked_about(9).await;
    assert_eq!(nine.len(), 9, "eight rows and the count: {nine:?}");
    assert!(
        nine[8].contains("and 1 more"),
        "the one it left off the list is counted rather than dropped: {nine:?}"
    );

    let ten = asked_about(10).await;
    assert!(
        ten[8].contains("and 2 more"),
        "and the count is of what is actually left: {ten:?}"
    );
}

#[tokio::test]
async fn a_question_about_a_long_argument_can_be_read_and_still_be_answered() {
    // a `revise` that rewrites a tool result carries the replacement in its arguments, and the
    // replacement is as long as the result was. Sized as one block, the box grew past the bottom
    // of the screen and `centred` cut what was last in it - the answers
    let long: String = (0..80)
        .map(|i| format!("line {i} of a very long replacement"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "revise", "content": long }),
        )]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("context", "revised")
            .with_capabilities([kamchatka::tools::domains::context("revise")]),
    ));

    harness.send("fix it").await;
    harness.settle().await;

    // the question, and every way of answering it, on a screen the arguments cannot fit on
    let screen = harness.screen();
    assert!(screen.contains("context wants: context:revise"), "{screen}");
    assert!(screen.contains("[y] once"), "{screen}");
    assert!(screen.contains("[i] the exact JSON"), "{screen}");
    // and it says how to see the part that did not fit
    assert!(screen.contains("pgup / pgdn for the rest"), "{screen}");
    assert!(screen.contains("line 0 of"), "{screen}");
    assert!(!screen.contains("line 79 of"), "{screen}");

    // and on a screen tall enough for the whole of a question, it says nothing of the sort: the
    // `pgup / pgdn` is the text for the boundary where the arguments have been cut, and a panel
    // that claims there is more to read when the last line is already on the screen sends
    // somebody paging down through rows that are there
    //
    // note: a second question, because an argument of eighty lines does not fit on any screen and
    // the boundary is where it *does*. Eight lines is the smallest that is still cut on one row
    // less than it is whole on.
    let fits: String = (0..8)
        .map(|i| format!("line {i} of a very long replacement"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut whole = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "revise", "content": fits }),
        )]),
        ModelResponse::text("done"),
    ]);
    whole.app.kernel.add_tool(Arc::new(
        ConstTool::new("context", "revised")
            .with_capabilities([kamchatka::tools::domains::context("revise")]),
    ));
    whole.send("fix it").await;
    whole.settle().await;

    let exact = whole.sized(100, 25);
    assert!(
        exact.contains("line 7 of a very long replacement"),
        "{exact}"
    );
    assert!(
        !exact.contains("pgup / pgdn"),
        "nothing was cut, so nothing says there is more: {exact}"
    );
    // and one row less, they are cut and it says so
    assert!(
        whole.sized(100, 24).contains("pgup / pgdn for the rest"),
        "one row less and the arguments are cut"
    );

    // the question is pinned rather than laid over the screen, so `pgdn` is the conversation's
    // until somebody moves the keys onto the question - and then it is the arguments'
    //
    // note: asserted on a line the panel is the only thing that can be showing. The conversation
    // behind it echoes the call, which puts the first two lines of the argument on the screen
    // whatever the panel is scrolled to - and used to be covered over by the box
    harness.press(KeyCode::PageDown).await;
    assert!(
        !harness.screen().contains("line 17 of"),
        "the arguments did not move: {}",
        harness.screen()
    );

    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.focus, Focus::Body);
    harness.press(KeyCode::PageDown).await;
    let screen = harness.screen();
    assert!(screen.contains("line 17 of"), "{screen}");
    // the answers do not move with them
    assert!(screen.contains("[y] once"), "{screen}");

    // `down` and `up` move the arguments a row each rather than only a page. Reading a rewritten
    // tool result line by line is what the arrows are for everywhere else on this program, and
    // somebody paging through eighty lines twenty at a time to find the one that changed is doing
    // it the hard way - so `up` has to come back up one row too, not just `down` going further
    let paged = harness.screen();
    harness.press(KeyCode::Down).await;
    let rowed = harness.screen();
    assert_ne!(rowed, paged, "`down` moved off the page: {paged}");
    harness.press(KeyCode::Up).await;
    assert_eq!(
        harness.screen(),
        paged,
        "`up` is a row as well, and puts it back"
    );

    // it stops at the end rather than counting presses: without the clamp, four pages down past
    // the bottom is four pages back up before anything moves
    for _ in 0..8 {
        harness.press(KeyCode::PageDown).await;
    }
    let bottom = harness.screen();
    assert!(bottom.contains("line 79 of"), "{bottom}");
    harness.press(KeyCode::PageUp).await;
    let screen = harness.screen();
    assert_ne!(screen, bottom, "one page up moves");

    // a box too small for the question is still a box somebody has to answer: the arguments go
    // first, then the header, and the answers are the last thing standing
    let tiny = harness.sized(30, 6);
    assert!(tiny.contains("[y] once"), "{tiny}");

    // and the question is still answerable after all that reading
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.kernel.pending_permissions().is_empty());
}

#[tokio::test]
async fn a_refused_model_is_told_which_kind_of_refusal_it_was() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "cat /etc/shadow" }),
        )]),
        ModelResponse::text("understood"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::exec("run")]),
    ));
    // a standing rule rather than a moment's hesitation
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

    harness.send("read the shadow file").await;
    harness.settle().await;

    // the result the model reads names the rule that did it, and says the rule is standing - so
    // it can stop asking rather than rephrasing the same call. Before this the whole of what
    // reached the model was `the call was not permitted`, and the reason was on the screen only
    let result = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .expect("a refusal is a result like any other");
    let said = result.content.to_text().into_owned();
    assert!(
        said.contains("`exec:run`"),
        "it names what refused it: {said}"
    );
    assert!(
        said.contains("a standing rule rather than an answer to this one call"),
        "{said}"
    );

    // and the person is told too: handing the reason out once meant whichever asked first got it
    let screen = harness.screen();
    assert!(screen.contains("refused by"), "{screen}");
}

#[tokio::test]
async fn a_call_refused_once_at_the_prompt_says_so_rather_than_naming_a_rule() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("understood"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    // nothing standing was decided, so the model is told the opposite thing: this call was
    // refused, and a different approach may well be allowed
    let result = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .expect("a refusal is a result like any other");
    let said = result.content.to_text().into_owned();
    assert!(
        said.contains("an answer to this call rather than a standing rule"),
        "{said}"
    );
}

/// The question about a change to the context names the items it would change.
///
/// note: `ids: [2]` is a true account of the arguments and a useless one to be asked about. The
/// tool rewrites and hides pieces of the context, the box asking covers the list those numbers
/// refer to, and the answer is one key - so somebody asked whether item 2 may be elided had to
/// already know what item 2 was, from a screen they could no longer see.
#[tokio::test]
async fn the_question_about_a_change_says_which_items_it_would_change() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "context",
        json!({ "action": "elide", "ids": [2], "reason": "it has served its purpose" }),
    )])]);
    let _offered = kamchatka::introspect::install(
        &harness.app.kernel,
        harness.app.policy.clone(),
        Limits::default(),
    );
    harness
        .app
        .kernel
        .push(ContextItem::user("what is in the log?"));
    harness.app.kernel.push(ContextItem::file(
        "server.log",
        "a wall of output".repeat(40),
    ));

    harness.send("tidy up").await;
    harness.settle().await;

    let screen = harness.flat();
    assert!(screen.contains("a tool wants to run"), "{screen}");
    assert!(
        screen.contains("ids: [2]"),
        "the arguments as written: {screen}"
    );
    // and what that number is, which is the half the question was missing
    assert!(
        screen.contains("[2] server.log") && screen.contains("reference"),
        "the question does not say what item 2 is: {screen}"
    );
    assert!(
        screen.contains("active"),
        "nor what state it is in: {screen}"
    );
}

/// A selector is the argument most worth expanding, because nobody can count it off the screen.
#[tokio::test]
async fn the_question_expands_a_selector_into_the_items_it_matches() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "context",
        json!({
            "action": "elide",
            "select": "all:files",
            "reason": "the reads are done with",
        }),
    )])]);
    let _offered = kamchatka::introspect::install(
        &harness.app.kernel,
        harness.app.policy.clone(),
        Limits::default(),
    );
    harness.app.kernel.push(ContextItem::user("read them"));
    harness
        .app
        .kernel
        .push(ContextItem::file("one.rs", "fn one() {}"));
    harness
        .app
        .kernel
        .push(ContextItem::file("two.rs", "fn two() {}"));

    harness.send("tidy up").await;
    harness.settle().await;

    let screen = harness.flat();
    assert!(screen.contains("select: all:files"), "{screen}");
    assert!(
        screen.contains("[2] one.rs") && screen.contains("[3] two.rs"),
        "a selector has to be expanded or the question cannot be answered: {screen}"
    );
}

/// A call waiting on a decision is not reported as something the model is not being shown.
///
/// note: the projector leaves a turn out while one of its calls has no result - it has to, since
/// a call with no answer is a request most providers reject - so `sends_content` says no for the
/// whole of the time the permission prompt is open. The chat read that as "left out" and drew
/// `[2] an assistant turn with no content and no answered calls` directly above the call the
/// person was being asked to authorise, describing it as a fault. It fired on every prompt, which
/// is the most common interactive path there is.
#[tokio::test]
async fn a_call_waiting_on_a_decision_is_not_drawn_as_withheld() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "dig",
        json!({ "where": "here" }),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("what is here?").await;
    harness.settle().await;

    let screen = harness.flat();
    assert!(
        screen.contains("a tool wants to run"),
        "the prompt should be open, or this is testing nothing: {screen}"
    );
    assert!(
        !screen.contains("no content and no answered calls"),
        "a call being asked about is mid-flight, not withheld: {screen}"
    );
    // the call itself is still on the chat, as a call
    assert!(screen.contains("dig("), "{screen}");

    // and once it is refused, the turn really is left out - and then the line belongs there
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;
    let screen = harness.flat();
    assert!(
        screen.contains("dig("),
        "the call it made is still what happened: {screen}"
    );
}

/// A command's own joints are picked out of it, so that the stages can be told apart.
///
/// note: what this replaced was breaking the line at each joint, one stage to a row. That read
/// well and cost a row per stage on a panel whose rows are its scarcest thing - and the broken
/// form was not a spelling `sh` would take back, so the panel was showing something nobody could
/// act on. The colour says the same thing in the command as the model wrote it.
#[tokio::test]
async fn a_commands_joints_are_picked_out_of_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({
                "action": "run",
                "cmd": "rg -n 'fn draw' src/ | sort -u && cargo build --release 2>&1",
            }),
        )]),
        ModelResponse::text("ran it"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("look for it").await;
    harness.settle().await;

    // the command as it was written, on one line, under the rule a fenced block gets
    let screen = harness.screen();
    assert!(
        screen.contains("│ rg -n 'fn draw' src/ | sort -u && cargo build --release 2>&1"),
        "{screen}"
    );

    // and the joints in a colour of their own, which the flags and paths around them are not in
    assert_eq!(harness.style_of_last("&&").0, Color::Cyan);
    assert_ne!(harness.style_of_last("cargo").0, Color::Cyan);
}

/// A separator that is part of an argument is not coloured as a joint.
///
/// note: the case that decides whether this is worth doing at all. A `|` inside a string drawn in
/// the joint's colour is the panel telling somebody a quoted character is a pipe, on the screen
/// where they decide whether to run it - so the reading is quote-aware, and gives up rather than
/// guessing.
#[tokio::test]
async fn a_separator_inside_a_quote_is_not_coloured_as_a_joint() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "action": "run", "cmd": "echo 'a | b'" }),
        )]),
        ModelResponse::text("said it"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("say it").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("│ echo 'a | b'"), "{screen}");
    // the `|` is inside the string, so it is drawn as the string's own colour and not as a joint
    assert_ne!(harness.style_of_last("| b'").0, Color::Cyan);
}

/// A build with nothing rating commands underlines nothing in one.
///
/// note: the other half of the joint tests above, and it holds in a build without the advisor in
/// it, where there is no `worst` stage to point at. Underlining a run of a command that was never
/// taken apart says there is a worse part of it to find, on the one screen where somebody is
/// deciding whether to run it.
///
/// note: `ls -l /tmp` rather than a chain, because the underline is a run *between* joints and the
/// run a wrong range picks out of a chain is a whole stage - which would make a failure here
/// indistinguishable from the advisor's own underline being drawn.
#[tokio::test]
async fn a_command_nothing_rated_is_not_underlined() {
    use ratatui::style::Modifier;

    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "action": "run", "cmd": "ls -l /tmp" }),
        )]),
        ModelResponse::text("listed"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("list it").await;
    harness.settle().await;

    // the command is drawn as it was written, and not one character of it is pointed at
    assert!(harness.screen().contains("│ ls -l /tmp"));
    for needle in ["ls -l", "tmp"] {
        assert!(
            !harness
                .style_of_last(needle)
                .1
                .contains(Modifier::UNDERLINED),
            "nothing was rated, so nothing is pointed at: {needle}"
        );
    }
}

/// A field named `cmd` is a command because the tool taking one is `shell`, and nothing else.
///
/// note: the rule is drawn by the tool's name as well as the field's, the way `App::about` picks
/// its two out - a `cmd` is a shell command *here* because `shell` is the tool that takes one,
/// and somebody else's tool with a field of that name has not said it is drawing a command line.
/// Drawing one anyway puts a rule and the joints in a colour on the panel, on an argument that is
/// a string, which reads as `sh` having been asked about something it was never shown.
#[tokio::test]
async fn a_cmd_on_a_tool_that_is_not_shell_is_not_drawn_as_a_command() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "cmd": "echo hi && ls" }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));

    harness.send("look").await;
    harness.settle().await;

    // the field as the model wrote it: a name and a string, sharing a row, and nothing on the
    // screen saying a shell was involved
    let screen = harness.screen();
    assert!(screen.contains("cmd: echo hi && ls"), "{screen}");
    assert!(
        !screen.contains("│ echo hi"),
        "a rule down the left is what a command is drawn with: {screen}"
    );
}

/// What the rule records say, as `(subject, verdict, answering, once)`.
fn ruled(harness: &Harness) -> Vec<(String, Verdict, Option<nachalnik::PermissionId>, bool)> {
    harness
        .app
        .kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            nachalnik::Event::PolicyRuled {
                subject,
                verdict,
                answering,
                once,
            } => Some((subject, verdict, answering, once)),
            _ => None,
        })
        .collect()
}

/// The row the keys are on is a row of the tab.
///
/// note: `down` walks down the list and stops on its last one, and `end` puts the cursor on that
/// last one rather than a row past it. The picked row is `App::chosen`, which every caller reads
/// as the capability the next key is about, so a value off the end of the list names nothing and
/// indexing the list with it is the panic. The frame clamps it before drawing, so the screen
/// looks right either way; the field does not, and the field is what the keys and the tests on
/// either side of them work from.
#[tokio::test]
async fn the_picked_row_is_a_row_of_the_tab() {
    let mut harness = Harness::new(Vec::new());
    for capability in [
        Capability::fs("read"),
        Capability::fs("write"),
        Capability::exec("run"),
    ] {
        harness
            .app
            .policy
            .set(&Subject::Capability(capability), Verdict::Allow);
    }
    harness.tab(Tab::Permissions);

    let rows = harness.app.permissions().len();
    assert!(rows >= 3, "not enough decisions to walk: {rows}");

    harness.press(KeyCode::Down).await;
    assert_eq!(harness.app.chosen, 1, "down is the next row");

    harness.press(KeyCode::End).await;
    assert_eq!(harness.app.chosen, rows - 1, "end is the last row");

    // and at the end of the list, the keys that walk it are keys that do nothing
    for key in [
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::End,
        KeyCode::Char('G'),
    ] {
        harness.press(key).await;
        assert_eq!(
            harness.app.chosen,
            rows - 1,
            "{key:?} went past the last of {rows} row(s)"
        );
    }

    // the row it names is one the tab has, and the keys that walk back up still work
    assert!(
        harness.app.permissions().get(harness.app.chosen).is_some(),
        "row {} of {rows}",
        harness.app.chosen
    );
    harness.press(KeyCode::Up).await;
    assert_eq!(harness.app.chosen, rows - 2, "up is the row before it");
    harness.press(KeyCode::Home).await;
    assert_eq!(harness.app.chosen, 0, "and home is the first of them");
}

/// A rule changed on the permissions tab is in the record.
#[tokio::test]
async fn a_rule_changed_on_the_tab_is_in_the_record() {
    let mut harness = Harness::new(Vec::new());
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    harness.app.policy.set(
        &Subject::Capability(Capability::exec("run")),
        Verdict::Allow,
    );
    harness.tab(Tab::Permissions);

    harness.press(KeyCode::Char('n')).await;
    assert_eq!(
        ruled(&harness),
        [("exec:run".to_owned(), Verdict::Deny, None, false)]
    );
}

/// A yes to a command that reaches the network is in the record as a grant for that call alone.
///
/// note: the decision said the call was allowed, and nothing said the network was opened for it,
/// which is the half the sandbox acts on.
#[tokio::test]
async fn a_network_grant_for_one_call_is_in_the_record() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "curl https://example.com" }),
        )]),
        ModelResponse::text("fetched"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("fetch it").await;
    harness.settle().await;
    let asked = harness.app.asked().expect("a question").id;
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;

    assert!(
        ruled(&harness).contains(&("net:reach".to_owned(), Verdict::Allow, Some(asked), true)),
        "{:?}",
        ruled(&harness)
    );
}

/// A chord is not its letter: `ctrl+a` at a question does not answer *always*.
///
/// note: the answers are bare letters, and a key with `ctrl` or `alt` held reached them as its
/// letter - so readline's `ctrl+a`, pressed out of habit with a question focused, allowed the call
/// and wrote the standing rule that answers every one like it, and `ctrl+y` allowed it once.
#[tokio::test]
async fn a_chord_at_a_question_is_not_the_letter_it_carries() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
        ModelResponse::text("as you wish"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    harness.send("tidy up").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some());
    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.focus, Focus::Body);

    for (code, held) in [
        ('a', KeyModifiers::CONTROL),
        ('a', KeyModifiers::ALT),
        ('y', KeyModifiers::CONTROL),
    ] {
        harness
            .app
            .on_key(KeyEvent::new(KeyCode::Char(code), held))
            .await;
        assert!(
            harness.app.asked().is_some(),
            "{held:?}+{code} answered the question"
        );
    }
    assert_ne!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::exec("run"))),
        Verdict::Allow,
        "and wrote no rule"
    );

    // the letter on its own still answers
    harness.press(KeyCode::Char('a')).await;
    assert!(harness.app.asked().is_none());
}

/// Asks what the gate would ask about a running command, as a task that waits for the answer.
fn reaching(harness: &Harness, call: &str, cmd: &str) -> tokio::task::JoinHandle<Option<bool>> {
    let policy = harness.app.policy.clone();
    let (call, cmd) = (nachalnik::ToolCallId::from(call), cmd.to_owned());

    tokio::spawn(async move { policy.reaching().ask(call, cmd).await })
}

/// Waits for the questions to be up, since they are asked from a task of their own.
async fn until_reached(harness: &Harness, count: usize) {
    for _ in 0..200 {
        if harness.app.policy.reaching().waiting().len() == count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{count} question(s) never arrived");
}

/// A running command that reached for the network is asked about in the prompt's place, and the
/// answer reaches it and is in the record.
///
/// note: the question is not the kernel's - it arrives while the call runs, from the gate holding
/// the command's first internet socket - so what is checked here is that the screen treats it as
/// the same kind of thing: pinned where the prompt was, the chat tab red, the keys only once `tab`
/// has put them there, and a `policy.ruled` saying who let it through.
#[tokio::test]
async fn a_command_reaching_for_the_network_is_asked_about_where_the_prompt_was() {
    let mut harness = Harness::new(Vec::new());
    let asked = reaching(&harness, "c1", "python3 fetch.py");
    until_reached(&harness, 1).await;

    let screen = harness.screen();
    assert!(screen.contains("a command wants the network"), "{screen}");
    assert!(screen.contains("python3 fetch.py"), "{screen}");
    assert!(screen.contains("[a] always, for net:reach"), "{screen}");
    assert!(
        !screen.contains("┌ you "),
        "the prompt gave up its place: {screen}"
    );
    assert_eq!(harness.tab_colour("chat"), Color::Red);

    // a letter at the prompt is not an answer, for the reason the kernel's questions give
    harness.press(KeyCode::Char('y')).await;
    assert_eq!(harness.app.policy.reaching().waiting().len(), 1);

    harness.answer(KeyCode::Char('y')).await;
    assert_eq!(asked.await.expect("it ran"), Some(true));
    assert!(harness.app.reached().is_none());
    assert_eq!(harness.app.focus, Focus::Input, "the keys are back");
    assert_eq!(
        ruled(&harness),
        [("net:reach".to_owned(), Verdict::Allow, None, true)],
        "once, and in the record"
    );
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::net("reach"))),
        Verdict::Ask,
        "and nothing standing was granted by a once"
    );
}

/// A question that has exactly its minimum and no more gets the minimum, rather than the screen.
///
/// note: the minimum is a floor the conversation argues against - a question may take the last of a
/// short window when it cannot be answered otherwise, and not one row before that. The case here
/// is the boundary between the two, which is the only place they can be told apart: a window that
/// leaves the question its seven rows exactly, and a question wanting eleven. Taking the
/// conversation's row as well would leave a window answering a question about a command with
/// nothing of the command on it, and the question is pinned under the conversation rather than
/// over it for exactly that reason.
#[tokio::test]
async fn a_question_taking_exactly_its_minimum_leaves_the_conversation_its_row() {
    let mut harness = Harness::new(Vec::new());
    // a command long enough that the question wants eleven rows of a hundred-column window, and a
    // second one behind it so the answers take a row of their own
    let _first = reaching(
        &harness,
        "c1",
        "curl -sSL https://example.com/a/b/c | tar xz -C /srv && systemctl restart thing && echo done",
    );
    let _second = reaching(
        &harness,
        "c2",
        "wget https://example.org/some/quite/long/path/here",
    );
    until_reached(&harness, 2).await;

    // thirteen rows: a row of conversation and the status line are what a question has to leave
    // behind, which leaves it seven - and seven is exactly the fewest a question is drawn in
    let screen = harness.sized(100, 13);
    let question = screen
        .lines()
        .skip_while(|line| !line.contains("wants the network"))
        .position(|line| line.contains('└'))
        .expect("the question is boxed")
        + 1;
    assert_eq!(question, 7, "the minimum, and no more: {screen}");
    assert!(
        !screen.contains("│ curl"),
        "and the command itself waits for a window with room for it: {screen}"
    );

    // a row taller and the question grows into it, which is the bargain the minimum is the other
    // side of: the two are one rule rather than two, and the seven is a floor and not a cap
    let taller = harness.sized(100, 14);
    assert_eq!(
        taller
            .lines()
            .skip_while(|line| !line.contains("wants the network"))
            .position(|line| line.contains('└'))
            .expect("the question is boxed")
            + 1,
        8,
        "one row more is one row more question: {taller}"
    );
}

/// `always` allows the network from here on, and lets through every command already waiting on
/// the same question; `n` refuses one command and no more.
#[tokio::test]
async fn always_is_the_network_from_here_on_and_no_is_this_command() {
    let mut harness = Harness::new(Vec::new());

    let refused = reaching(&harness, "c1", "nc example.com 80");
    until_reached(&harness, 1).await;
    harness.answer(KeyCode::Char('n')).await;
    assert_eq!(refused.await.expect("it ran"), Some(false));
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::net("reach"))),
        Verdict::Ask
    );

    let first = reaching(&harness, "c2", "curl a");
    until_reached(&harness, 1).await;
    let second = reaching(&harness, "c3", "curl b");
    until_reached(&harness, 2).await;
    assert!(harness.screen().contains("1 more after this one"));

    harness.answer(KeyCode::Char('a')).await;
    assert_eq!(first.await.expect("it ran"), Some(true));
    assert_eq!(second.await.expect("it ran"), Some(true), "swept");
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::net("reach"))),
        Verdict::Allow
    );
    assert_eq!(
        ruled(&harness),
        [
            ("net:reach".to_owned(), Verdict::Deny, None, true),
            ("net:reach".to_owned(), Verdict::Allow, None, false),
        ]
    );
}

/// A row that leaves the tab under the cursor leaves the keys on the row that has taken its place.
///
/// note: the tab lists decisions, so taking one back - `ask` is what the policy does when nobody
/// has made one - drops the row rather than greying it. The cursor was on the last row, and the
/// next key has to answer for a row that is there: without this, the tab acts on a row one past
/// the end of its own list. Nothing here draws between the two keys, which is the whole of it -
/// a frame happens to clamp this too, and a caller with a loop of its own has no frame to do it.
#[tokio::test]
async fn a_decision_taken_back_does_not_leave_the_keys_on_a_row_that_is_not_there() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::exec("run")]),
    ));
    let read = Subject::Capability(Capability::fs("read"));
    let shell = Subject::Capability(Capability::exec("run"));
    harness.app.policy.set(&read, Verdict::Deny);
    harness.app.policy.set(&shell, Verdict::Allow);
    harness.tab(Tab::Permissions);

    let rows = harness.app.permissions();
    assert_eq!(rows.len(), 2, "{} rows to choose between", rows.len());
    // the last row of the list, and the row above it: whatever is answered next must be that one
    let last = rows
        .iter()
        .position(|row| row.subject == shell)
        .expect("the shell is listed");
    assert_eq!(last, rows.len() - 1, "the shell is on the last of two rows");
    let other = rows[last - 1].subject.clone();

    pick(&mut harness, &shell).await;
    harness.press(KeyCode::Char('r')).await;
    assert_eq!(
        harness
            .app
            .permissions()
            .iter()
            .filter(|row| row.subject == shell)
            .count(),
        0,
        "`ask` is not a decision, so it is not a row"
    );

    // and the next key answers for the row that has taken its place
    harness.press(KeyCode::Char('a')).await;
    assert_eq!(
        harness.app.policy.stance(&other),
        Verdict::Allow,
        "the row that is still on the tab is the one that was answered"
    );
    let said = harness
        .app
        .loose
        .last()
        .map(|entry| entry.text.clone())
        .unwrap_or_default();
    assert!(
        said.contains(&other.to_string()),
        "and the answer says which subject it was about: {said:?}"
    );
}

/// A question in the prompt's place takes the prompt's keys and leaves the conversation's.
///
/// note: the guard is not a freeze. Somebody answering a tool call is often looking at what the
/// call is about, and the panel is pinned rather than laid over the screen for exactly that - so
/// the four keys that move the conversation are the ones still working, while the answers wait
/// for `tab`. Read on `app.scroll`, which is what the transcript is drawn from and what the keys
/// work against.
#[tokio::test]
async fn the_conversation_still_scrolls_while_a_question_waits() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("dug"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    // enough conversation that the transcript has somewhere to go
    for n in 0..80 {
        harness
            .app
            .kernel
            .push(ContextItem::assistant(format!("line {n}"), vec![]));
    }

    harness.send("dig").await;
    harness.settle().await;
    assert!(harness.app.asked().is_some(), "a question is waiting");
    // the keys are still on the prompt, which is not on the screen - that is what makes this the
    // guard rather than the question
    assert_eq!(harness.app.focus, Focus::Input);

    // a frame first, since what it measured is what the paging keys work against
    harness.screen();
    let bottom = harness.app.rendered.saturating_sub(harness.app.viewport);
    assert!(bottom > 0, "there is a conversation to move through");
    assert_eq!(harness.app.scroll, bottom, "and it starts at the bottom");

    // each of the four in turn, from wherever the last one left it: up and down a line, and the
    // paging keys half a screen, which is what `input_key` does with the same keys. Two pages up
    // before one down, so that a `pgdn` going too far is not hidden by the bottom it stops at
    let half = (harness.app.viewport / 2) as isize;
    for (key, step) in [
        (KeyCode::Up, -1),
        (KeyCode::Down, 1),
        (KeyCode::PageUp, -half),
        (KeyCode::PageUp, -half),
        (KeyCode::PageDown, half),
    ] {
        let was = harness.app.scroll;
        harness.press(key).await;
        let now = harness.app.scroll;
        assert_eq!(
            now,
            was.saturating_add_signed(step).min(bottom),
            "{key:?} did not move the conversation where it should: {was} -> {now}"
        );
    }
}

/// The picked row is the row the keys have, and it says so by being reversed rather than
/// underlined.
///
/// note: the two modifiers are the whole of what distinguishes "the keys are here" from "the keys
/// are elsewhere" on this tab, and both list tabs use the same pair. A pane that drew the
/// underline while the keys were on its body would be claiming the keys had gone somewhere they
/// had not, on the one screen where a letter changes what a call is allowed to do.
#[tokio::test]
async fn the_picked_row_is_reversed_while_the_keys_are_on_the_tab() {
    use ratatui::style::Modifier;

    let mut harness = Harness::new(Vec::new());
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::fs("read")]),
    ));
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    harness.tab(Tab::Permissions);

    assert_eq!(
        harness.app.focus,
        Focus::Body,
        "the keys are on the tab's body, which is the only place they can be"
    );
    let (_, modifier) = harness.style_of("allow");
    assert!(
        modifier.contains(Modifier::REVERSED),
        "the picked row is reversed while the keys are on it, not underlined"
    );
    assert!(
        !modifier.contains(Modifier::UNDERLINED),
        "an underline would say the keys are somewhere else"
    );
}

/// A row somebody cycled back to `ask` leaves the picked row naming a row that is no longer there.
///
/// note: the frame is what keeps `App::chosen` a row of the tab, and it is the only thing that
/// does - the key handler clamps on its way in, so a key that took a row off the list leaves the
/// index pointing one past the end. The screen looks right either way, because the list clamps
/// its own selection, but `chosen` is the field every caller and every key works from, and a
/// value off the end of the list names no subject at all.
#[tokio::test]
async fn cycling_the_picked_row_away_leaves_it_on_a_row_of_the_tab() {
    let mut harness = Harness::new(Vec::new());
    for capability in [
        Capability::fs("read"),
        Capability::fs("write"),
        Capability::exec("run"),
    ] {
        harness
            .app
            .policy
            .set(&Subject::Capability(capability), Verdict::Allow);
    }
    harness.tab(Tab::Permissions);
    let rows = harness.app.permissions().len();
    assert_eq!(rows, 3, "three decisions to walk: {rows}");

    // the last row, and cycled all the way round: allow to deny keeps it listed, deny to ask
    // does not, because `ask` is what the policy does about everything nobody has answered
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Char(' ')).await;
    assert_eq!(harness.app.chosen, 2, "still the last of three");
    harness.press(KeyCode::Char(' ')).await;
    assert_eq!(
        harness.app.permissions().len(),
        rows - 1,
        "an undecided subject is not a row"
    );
    assert_eq!(
        harness.app.chosen,
        rows - 1,
        "and the key that took it off the list leaves the index where it was"
    );

    // the frame brings it back onto a row, which is the whole of what it is there for
    harness.screen();
    assert_eq!(
        harness.app.chosen,
        rows - 2,
        "the picked row is a row of the tab after the frame has drawn"
    );
    assert!(
        harness.app.permissions().get(harness.app.chosen).is_some(),
        "row {} of {}",
        harness.app.chosen,
        harness.app.permissions().len()
    );
}
