//! The question that stops a turn, and the tab that shows every answer already given.
//!
//! note: two views of one thing. A permission is asked about once, at the moment it is least
//! convenient to think about it, and read back afterwards on a tab where it can be changed - so
//! these check both ends: that the question names what it is about and cannot be answered by
//! somebody's next keystroke, and that the tab reports the same policy the calls actually meet.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{
    app::{Focus, Tab},
    sandbox::Confinement,
    tools::{Limits, Subject},
};
use nachalnik::{
    Capability, Config, ContextItem, ModelResponse, Verdict,
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig somewhere").await;
    harness.settle().await;

    // the question, with the arguments it would run with
    let screen = harness.screen();
    assert!(screen.contains("a tool wants to run"), "{screen}");
    assert!(screen.contains("dig wants: shell"), "{screen}");
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
    assert!(back.contains("dig wants: shell"), "{back}");
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

#[tokio::test]
async fn saying_always_stops_the_question_being_asked_again() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::tool_calls(vec![call("c2", "dig", json!({}))]),
        ModelResponse::text("twice"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
            .stance(&Subject::Capability(Capability::Shell)),
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
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![
            call("c1", "danger", json!({})),
            call("c2", "danger", json!({})),
        ])],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
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
        ConstTool::new("grep", "found it").with_capabilities([Capability::Read]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
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
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let shell = screen
        .lines()
        .find(|line| line.contains("shell"))
        .expect("a decided capability is listed");
    assert!(shell.contains("deny"), "{shell}");
    assert!(
        shell.contains("rm"),
        "the row names what it covers: {shell}"
    );

    // no tool declares `network` - but the shell is judged against it anyway, on what the command
    // says, so its row names the tool the answer actually reaches rather than claiming that
    // nothing needs it
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Network), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let network = screen
        .lines()
        .find(|line| line.contains("network"))
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
        .set(&Subject::Capability(Capability::Write), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let write = screen
        .lines()
        .find(|line| line.contains("write"))
        .expect("what the policy has been told about is listed");
    assert!(write.contains("nothing registered needs it"), "{write}");
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
        ConstTool::new("shell", "output").with_capabilities([Capability::Shell]),
    ));
    // the default is a question, not a refusal: reaching the network is a thing somebody may
    // perfectly well want, and the sandbox is what makes either answer mean something
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::Network)),
        Verdict::Ask
    );
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Network), Verdict::Deny);

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
        screen.contains("network"),
        "a refusal nobody can account for is the thing this program is not for: {screen}"
    );
}

#[tokio::test]
async fn the_permissions_tab_admits_what_a_shell_can_do() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::Read]),
    ));
    harness.tab(Tab::Permissions);

    // nothing here runs commands, so the five verdicts are the whole story
    let screen = harness.sized(120, 30);
    assert!(!screen.contains("shell:"), "{screen}");

    // ... and once something does, the tab has to account for it: `shell` subsumes every other
    // row unless something is confining it, and nothing is here
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("sh", "output").with_capabilities([Capability::Shell]),
    ));
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: a command can do any of these"),
        "{screen}"
    );

    // ... and when something is, it says what a command can reach instead of what it cannot
    harness.app.confinement = Confinement::Full;
    let screen = harness.sized(120, 30);
    assert!(screen.contains("shell: confined"), "{screen}");
    assert!(
        !screen.contains("can do any of these"),
        "a confined shell cannot: {screen}"
    );

    // refusing it outright puts the other rows back in charge either way
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);
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
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
    ));

    // answered for the capability and nothing else, which is what `always` on an ordinary read
    // does. The ordinary file is no longer a question and `.env` still is, because the strictest
    // of what is consulted wins and a rule about the *file* is finer than a verdict about the tool
    // that opened it. One turn, both reads, one question
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Read), Verdict::Allow);
    harness.send("read both").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        screen.contains("src/main.rs") && screen.contains("read: 2 tokens"),
        "the ordinary one ran without being asked about: {screen}"
    );
    let asked = screen
        .lines()
        .find(|line| line.contains("wants:"))
        .expect("this one is a question");
    assert!(asked.contains(".env*"), "the rule is named: {asked}");
    assert!(asked.contains("read"), "and so is the capability: {asked}");
}

#[tokio::test]
async fn saying_always_answers_for_everything_the_question_named() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "path": ".env" }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
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
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
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
        .set(&Subject::Capability(Capability::Read), Verdict::Allow);
    let filled = harness.flat();
    assert!(filled.contains("Careful"), "{filled}");
    assert!(filled.contains("read allow read"), "{filled}");

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

#[tokio::test]
async fn the_permissions_tab_draws_the_path_rules_too() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
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
        ConstTool::new("shell", "output").with_capabilities([Capability::Shell]),
    ));
    // this time it is the tool's own capability that is refused, not something the command reached
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);

    harness.send("clean up").await;
    harness.settle().await;

    let screen = harness.screen();
    let note = screen
        .lines()
        .find(|line| line.contains("refused by"))
        .expect("the refusal is accounted for");
    assert!(note.contains("shell"), "{note}");
    assert!(
        !note.contains("network"),
        "the command never reached for it: {note}"
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
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
            ModelResponse::text("as you wish"),
            ModelResponse::tool_calls(vec![call("c2", "rm", json!({}))]),
            ModelResponse::text("done"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
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
            .any(|row| row.subject == Subject::Capability(Capability::Shell)),
        "a refusal of one call is not a decision about the capability"
    );

    // deciding it *is* what puts it there, and changes what the same call does next time
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Deny,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;
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
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
            ModelResponse::text("fine, I will not"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));

    // the tab lists decisions, so there is one to make first: `a` at a question is what puts a
    // subject on it, and `n` there is what changes its mind
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;
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
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;

    use nachalnik::Verdict::{Allow, Ask, Deny};
    let shell = Subject::Capability(Capability::Shell);
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

#[tokio::test]
async fn ctrl_d_leaves_even_when_a_tool_is_waiting_to_run() {
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![call(
            "c1",
            "danger",
            json!({}),
        )])],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
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

#[tokio::test]
async fn dropping_the_calls_hands_the_turn_back_to_the_model() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "danger", json!({}))]),
            ModelResponse::text("all right, something else then"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
            .stance(&Subject::Capability(Capability::Shell)),
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
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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
        ModelResponse::tool_calls(vec![call("c1", "amend", json!({ "ids": [1] }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("amend", "elided").with_capabilities([Capability::Custom("amend".into())]),
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
    harness.press(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.asked().is_none(), "one key, having looked");
    assert_ne!(
        harness.tab_colour("chat"),
        Color::Red,
        "and the strip stops saying otherwise"
    );
}

#[tokio::test]
async fn a_question_about_a_long_argument_can_be_read_and_still_be_answered() {
    // an `amend` that rewrites a tool result carries the replacement in its arguments, and the
    // replacement is as long as the result was. Sized as one block, the box grew past the bottom
    // of the screen and `centred` cut what was last in it - the answers
    let long: String = (0..80)
        .map(|i| format!("line {i} of a very long replacement"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "amend",
            json!({ "action": "revise", "content": long }),
        )]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("amend", "revised").with_capabilities([Capability::Custom("amend".into())]),
    ));

    harness.send("fix it").await;
    harness.settle().await;

    // the question, and every way of answering it, on a screen the arguments cannot fit on
    let screen = harness.screen();
    assert!(screen.contains("amend wants: amend"), "{screen}");
    assert!(screen.contains("[y] once"), "{screen}");
    assert!(screen.contains("[i] the exact JSON"), "{screen}");
    // and it says how to see the part that did not fit
    assert!(screen.contains("pgup / pgdn for the rest"), "{screen}");
    assert!(screen.contains("line 0 of"), "{screen}");
    assert!(!screen.contains("line 79 of"), "{screen}");

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
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    // a standing rule rather than a moment's hesitation
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);

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
    assert!(said.contains("`shell`"), "it names what refused it: {said}");
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
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
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

/// The question about an `amend` names the items it would change.
///
/// note: `ids: [2]` is a true account of the arguments and a useless one to be asked about. The
/// tool rewrites and hides pieces of the context, the box asking covers the list those numbers
/// refer to, and the answer is one key - so somebody asked whether item 2 may be elided had to
/// already know what item 2 was, from a screen they could no longer see.
#[tokio::test]
async fn the_question_about_an_amend_says_which_items_it_would_change() {
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![call(
            "c1",
            "amend",
            json!({ "action": "elide", "ids": [2], "reason": "it has served its purpose" }),
        )])],
        Config::default(),
    );
    let _offered = kamchatka::introspect::install(&harness.app.kernel, Limits::default());
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
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![call(
            "c1",
            "amend",
            json!({
                "action": "elide",
                "select": "all:files",
                "reason": "the reads are done with",
            }),
        )])],
        Config::default(),
    );
    let _offered = kamchatka::introspect::install(&harness.app.kernel, Limits::default());
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
