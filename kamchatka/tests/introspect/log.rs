//! `log`: the record kept beside the context, what reading it costs, and the questions only an
//! absence in it can answer.

use crate::{agent, answered, answers_from, branch, offers, one_turn};
use kamchatka::{introspect, tools::Careful, tools::Limits, tools::Subject};
use nachalnik::{
    Config, ContextItem, ContextKind, Kernel, ModelResponse, Verdict, test::ScriptedProvider,
    test::call,
};
use serde_json::json;
use std::sync::Arc;

/// A bare call prices the whole log and hands back nothing else.
///
/// note: the estimate is the load-bearing half. "412 records" does not tell a model whether it
/// can afford them, and a summary that named a count and left the cost to be found out by asking
/// would be the thing `budget` exists to stop. So this checks the quote against what the records
/// really cost once they are in the context - the kernel's own count of the item they landed as -
/// rather than only that a figure is there.
#[tokio::test]
async fn a_bare_log_is_a_summary_and_a_price_rather_than_the_records() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "action": "read" })),
        call("c2", "log", json!({ "action": "read", "since": 0 })),
    ]));

    // enough of them that the answer is mostly records rather than mostly header, which is what
    // makes the two figures comparable at all
    for n in 0..40 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel
        .replace(item, "the parser is in src/parse.rs")
        .unwrap();
    kernel.push(ContextItem::user("what have you been doing?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let summary = &said[0];

    // the kinds, counted, and none of the records
    assert!(summary.contains("context.added"), "{summary}");
    assert!(summary.contains("context.replaced"), "{summary}");
    assert!(
        !summary.contains("src/parser.rs"),
        "a bare call should cost almost nothing, and a replaced item's text is not nothing: \
         {summary}"
    );
    assert!(
        !summary.lines().any(|line| line.starts_with("    1  ")),
        "a bare call hands back no records at all: {summary}"
    );

    // what the summary said the whole log would cost
    let quoted: usize = summary
        .split('~')
        .nth(1)
        .and_then(|rest| rest.split(" tokens").next())
        .map(|n| n.replace(',', "").parse().unwrap())
        .expect("the summary quotes a token figure");

    // and what it really cost: the kernel's count of the item the second answer landed as. It is
    // the later of the two calls, so its log is a few records longer than the one that was priced
    // and it carries a header the quote does not - both push the real figure up, which is the
    // safe direction for an estimate to be wrong in
    let charged = kernel
        .items()
        .iter()
        .filter(|item| matches!(&item.kind, ContextKind::ToolResult { tool, .. } if tool == "log"))
        .nth(1)
        .map(|item| item.tokens)
        .expect("the second answer is in the context");
    assert!(
        quoted <= charged && charged - quoted < charged / 8,
        "the summary quoted {quoted} and taking them all cost {charged}"
    );
}

/// Every answer opens with what exists, not with what matched.
///
/// note: this is the rule that makes the tool safe rather than a convenience. A filtered answer
/// that reported only its own count is indistinguishable from a session in which almost nothing
/// happened, and the difference matters most in exactly the case somebody filters for - looking
/// for an overwrite and finding none.
#[tokio::test]
async fn a_filtered_log_opens_with_the_whole_total_and_not_the_filtered_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "action": "read", "kinds": ["context.replaced"] }),
        ),
        call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["model.payload"] }),
        ),
    ]));

    kernel.push(ContextItem::user("carry on"));
    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel
        .replace(item, "the parser is in src/parse.rs")
        .unwrap();
    let total = kernel.history().len();

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said {
        let opens: usize = answer
            .split(' ')
            .next()
            .map(|n| n.replace(',', "").parse().unwrap())
            .expect("every answer opens with a count");
        assert!(
            opens >= total,
            "the answer opened with {opens} and there were {total} records: {answer}"
        );
    }

    assert!(said[0].contains("1 match"), "{}", said[0]);
    // a real zero, arriving beside a total that is not one, which is what stops it reading as an
    // empty log. `model.payload` is a kind nothing emits unless `record_payloads` is on
    assert!(said[1].contains("0 match"), "{}", said[1]);
    assert!(
        said[1].contains("Nothing matched"),
        "a zero says so in words as well as in a figure: {}",
        said[1]
    );
    assert!(
        said[1].contains("context.added"),
        "and lists the kinds there are, because a filter that matched nothing is usually one \
         spelled for another session: {}",
        said[1]
    );
}

/// What an item used to say survives in the log, and `ids` is how it is found again.
///
/// note: measured rather than assumed, and the measurement moved what this test is for. Taking
/// the content out of `ContextReplaced` - the "fix" the stale sentence in `session.rs` used to
/// invite - is already caught by five tests across two crates, `undo::a_replacement_is_the_one_/// thing_that_would_otherwise_be_lost` among them, so this is not the guard on that and saying it
/// was would have been a false sense of a well-watched seam. What nothing else catches is the
/// pair of things this tool adds: finding the record by the *item* number rather than by kind,
/// and `whole` - drop either and only this fails.
/// `take` counts records, which is what it says it counts, and a whole one is not one line.
///
/// note: it counted rendered *lines*, and `whole` prints a replaced item's old text entire - so
/// `take: 1` against a record holding three lines of old text handed over the last of those lines
/// with no sequence number and no event name in front of it, and the header called that one
/// record. The two arguments are tested apart from each other everywhere else.
#[tokio::test]
async fn take_counts_records_even_where_one_of_them_is_many_lines() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({
                "action": "revise",
                "ids": [1],
                "content": "one line now",
                "reason": "it was three",
            }),
        ),
        call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["context.replaced"], "whole": true, "take": 1 }),
        ),
    ]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs\nthe lexer is in src/lex.rs\nthe kernel is next door",
    ));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let whole = said.last().unwrap();

    assert!(
        whole.contains("context.replaced"),
        "the one record asked for arrived without its name: {whole}"
    );
    assert!(
        whole.contains("src/parser.rs") && whole.contains("the kernel is next door"),
        "a whole record is the whole of it: {whole}"
    );
    assert!(
        whole.contains("Showing 1.") && !whole.contains("not here"),
        "one record matched and one was asked for: {whole}"
    );
}

#[tokio::test]
async fn a_revised_item_can_be_read_back_out_of_the_log_by_its_number() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({
                "action": "revise",
                "ids": [1],
                "content": "the parser is in src/parse.rs",
                "reason": "I wrote down the wrong path",
            }),
        ),
        call("c2", "log", json!({ "action": "read", "ids": [1] })),
        call(
            "c3",
            "log",
            json!({ "action": "read", "ids": [1], "whole": true }),
        ),
    ]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs\nand the lexer is in src/lex.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (glimpsed, whole) = (&said[0], &said[1]);

    assert!(glimpsed.contains("context.replaced"), "{glimpsed}");
    assert!(glimpsed.contains("src/parser.rs"), "{glimpsed}");
    // the first line only, and the line admits to being one
    assert!(
        !glimpsed.contains("src/lex.rs"),
        "a log line is a line; the rest is asked for: {glimpsed}"
    );
    assert!(glimpsed.contains("`whole`"), "{glimpsed}");

    // and asked for, it is all there - which is the assertion that would fail if anybody ever
    // "fixed" the one event that carries content
    assert!(whole.contains("src/parser.rs"), "{whole}");
    assert!(whole.contains("src/lex.rs"), "{whole}");

    // nothing about reading the log put anything back into the context
    assert_eq!(
        kernel
            .item(nachalnik::ContextId(1))
            .unwrap()
            .content
            .to_text(),
        "the parser is in src/parse.rs"
    );
}

/// A shortened answer says how much of it is missing.
#[tokio::test]
async fn take_says_how_many_records_are_beyond_what_it_showed() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "take": 2 }),
    )]));

    for n in 0..6 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("Showing the 2 most recent"), "{}", said[0]);
    assert!(
        said[0].contains("21 older are not here"),
        "a shortened log has to say how much of it is not here, and in a word that is true of \
         them - nothing narrowed what counts here, so they are older rather than unmatched: {}",
        said[0]
    );
    // the most recent, and still in the order they happened
    let numbered: Vec<&str> = said[0]
        .lines()
        .filter(|line| line.starts_with("  ") && line.trim().starts_with(char::is_numeric))
        .collect();
    assert_eq!(numbered.len(), 2, "{}", said[0]);
    let seq = |line: &str| -> u64 { line.trim().split(' ').next().unwrap().parse().unwrap() };
    assert!(
        seq(numbered[0]) < seq(numbered[1]),
        "records come back in the order they happened: {numbered:?}"
    );
}

/// A filter nobody can read is a mistake to report, not a log with nothing in it.
///
/// note: the one wrong answer this tool can give is *nothing happened*, and an empty result for a
/// malformed argument is exactly that answer. The schema is descriptive and the kernel validates
/// nothing against it, so the tool has to.
#[tokio::test]
async fn a_filter_that_is_not_a_number_is_an_error_rather_than_an_empty_log() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "action": "read", "since": "yesterday" }),
        ),
        call("c2", "log", json!({ "action": "read", "take": "lots" })),
        // a number written as a word is a mistake; a number written as a string is not, and
        // taking it costs nothing
        call("c3", "log", json!({ "action": "read", "since": "1" })),
    ]));

    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said[..2] {
        assert!(answer.contains("whole number"), "{answer}");
        assert!(
            answer.contains("empty log"),
            "the refusal says why it is not an empty answer: {answer}"
        );
    }
    assert!(said[2].contains("records"), "{}", said[2]);
    assert!(said[2].contains("match since:1"), "{}", said[2]);
}

/// The log is one order, and it is the order the changes were applied in.
///
/// note: the runtime writes the record and the broadcast under one lock so the two agree; what
/// this checks is that reading it back through a tool does not resort it. Run with calls in
/// parallel, because that is the configuration where a second order could appear.
#[tokio::test]
async fn the_records_come_back_in_one_order_however_the_calls_were_run() {
    let kernel = Kernel::new(Config {
        parallel_tool_calls: true,
        ..Config::default()
    });
    let provider = Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "context", json!({ "action": "look" })),
        call("c2", "context", json!({ "action": "budget" })),
        call("c3", "log", json!({ "action": "read", "since": 0 })),
    ])));
    kernel.set_provider(provider);
    let policy = Arc::new(Careful::new());
    for domain in ["context", "log", "setup", "fork"] {
        policy.set(&Subject::parse(domain), Verdict::Allow);
    }
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());

    kernel.push(ContextItem::user("all three at once"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let seqs: Vec<u64> = said[0]
        .lines()
        .filter(|line| line.starts_with("  ") && line.trim().starts_with(char::is_numeric))
        .map(|line| line.trim().split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(seqs.len() > 3, "{}", said[0]);
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "sequence numbers are the order, and they came back out of it: {seqs:?}"
    );
}

/// The log is read-only, and there is no argument that says otherwise.
#[tokio::test]
async fn log_declares_its_own_capability_and_no_way_to_write() {
    let (kernel, _provider, _anchor) = agent(Vec::new());

    let spec = kernel.tool("log").expect("it is installed").spec();
    assert_eq!(
        spec.capabilities,
        vec![kamchatka::tools::domains::log("read")],
        "it has to be separately grantable, and separately revocable"
    );
    // it takes an `action` like every other tool here, and `read` is the only one there is:
    // uniform beats terse, because the tool that is the exception is the one a model gets wrong
    assert_eq!(
        offers(&spec.schema),
        ["read"],
        "every tool here takes an action, and this one reads"
    );
    assert_eq!(
        branch(&spec.schema, "read")["required"],
        json!(["action"]),
        "`read` takes filters and requires none of them"
    );
}

/// A fork's own events are not in this session's log, and the log says so by counting.
///
/// note: one of the questions the design that brought `log` here left to be resolved while
/// implementing, and the answer falls out of what a fork already is: a whole second kernel with a
/// session of its own that goes when it does. So the parent's log holds what the parent did - it
/// asked for a tool, the tool ran, the tool answered - and the fork's own request is not in it,
/// even though the fork made one against the same provider. What the copy *said* is in the tool
/// result, like any other tool's output, and that is the whole of what crosses.
#[tokio::test]
async fn a_forks_own_events_are_not_in_this_sessions_log() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "fork", json!({ "action": "draft" }))]),
        ModelResponse::text("the copy's answer"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["model.requested"] }),
        )]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what would you say?"));
    kernel.turn().await.expect("the turn failed");

    // four requests reached the provider: the one that asked for `draft`, the fork's own, the one
    // that asked for `log`, and the one that finished the turn
    assert_eq!(provider.requests().len(), 4);

    // and the log this session can read holds two at the moment `log` runs - its own first and
    // third. The fork's is in neither figure, because it belonged to a session that no longer
    // exists, and the fourth had not happened yet
    let said = answers_from(&kernel, &["log"]).remove(0);
    assert!(
        said.contains("2 match kinds:[\"model.requested\"]"),
        "a fork's request is not this session's: {said}"
    );
    // what the copy said did cross, as the tool's output, the way any tool's output does
    assert!(
        answers_from(&kernel, &["fork"])
            .iter()
            .any(|answer| answer.contains("the copy's answer")),
        "the answer is the one thing a fork hands back"
    );
}

/// `take` shortens an answer; it does not narrow what the answer is of, and the header says so.
///
/// note: found by reading what the tool actually prints rather than by a test, which is why it is
/// worth one now. A call carrying only `take` reported "15 records ... total. 15 match , ~205
/// tokens" - a match count that was really the total, a filter description that was empty because
/// there was no filter, and a dangling comma where it should have been. A header whose whole job
/// is to be believed cannot be the part that reads like a bug.
#[tokio::test]
async fn take_on_its_own_does_not_claim_to_have_matched_anything() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "action": "read", "take": 2 })),
        call(
            "c2",
            "log",
            json!({ "action": "read", "take": 2, "kinds": ["context.added"] }),
        ),
    ]));

    for n in 0..5 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (bare_take, narrowed) = (&said[0], &said[1]);

    assert!(bare_take.contains("tokens in all."), "{bare_take}");
    assert!(
        !bare_take.contains("match"),
        "nothing narrowed what counts, so nothing matched anything: {bare_take}"
    );
    assert!(
        bare_take.contains("older are not here"),
        "what is missing is older, not unmatched: {bare_take}"
    );
    // and where something *did* narrow it, the match count and the filter are both there
    assert!(narrowed.contains("match kinds:"), "{narrowed}");
    assert!(
        narrowed.contains("more match and are not here"),
        "{narrowed}"
    );
}

/// An argument this tool does not take is a mistake to report, not one to ignore.
///
/// note: found live. A session called `log {action: "look"}` - the sibling tools all took an
/// `action` and this one did not, so it was the obvious mistake - got the summary back, and read
/// it as the answer to a question it had not asked. It then cited it. An ignored argument is the
/// same failure as a filter nobody can parse, one step earlier: the reply is a real answer, so
/// nothing in it says that what was asked for did not happen.
///
/// note: that particular call cannot go wrong any more, and what closed it was making `log` take
/// an `action` like everything else here. `action: "look"` is now an operation this tool does not
/// have and is refused by name, which is the same answer `fs` and `context` give - three tools,
/// one sentence. What this still checks is the other half, which no amount of uniformity fixes: a
/// misspelled *filter*, where the tool would otherwise answer a question nobody asked.
#[tokio::test]
async fn an_argument_log_does_not_take_is_refused_rather_than_ignored() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "action": "look" })),
        call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["context.added"], "limit": 3 }),
        ),
    ]));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    // an operation it does not have, refused the way every tool here refuses one
    assert!(said[0].contains("there is no `look`"), "{}", said[0]);
    assert!(
        said[0].contains("this tool does read"),
        "and says what it does instead: {}",
        said[0]
    );
    // a real filter beside an unreadable one is still refused, rather than half-honoured
    assert!(said[1].contains("does not take `limit`"), "{}", said[1]);
    assert!(said[1].contains("nothing was done"), "{}", said[1]);
    assert!(!said[1].contains("match"), "{}", said[1]);
}

/// An item with no beginning in this log is said to have none, which is what `ids` really asks.
///
/// note: the sharpest thing the live runs turned up. A session resumed under a second model was
/// asked whether it had written an inherited turn. It asked the log about that item and got five
/// `model.requested` rows naming it - every one true, because the item had been in every request
/// since - and read them as proof it had written the item itself. The record that settled the
/// question was the one that was not there, and an absence is the one answer a list of matching
/// records cannot give. So the tool says it.
#[tokio::test]
async fn an_inherited_item_is_reported_as_having_no_beginning_here() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("which index?"));
    first.push(ContextItem::assistant("I chose a B-tree.", vec![]));

    let kernel = Kernel::resume(Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "log", json!({ "action": "read", "ids": [2] })),
        call("c2", "log", json!({ "action": "read", "ids": [2, 3] })),
        call("c3", "log", json!({ "action": "read", "ids": [3] })),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(kamchatka::tools::domains::log("read")),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (inherited, mixed, native) = (&said[0], &said[1], &said[2]);

    // the misleading evidence is here, exactly as it was live: `model.requested` names item 2,
    // because item 2 has been in every request since. It is true and it is not provenance
    assert!(inherited.contains("model.requested"), "{inherited}");
    // and the record that settles it is the one that is absent, so the absence is stated
    assert!(
        inherited.contains("[2] has no `context.added` here"),
        "{inherited}"
    );
    assert!(inherited.contains("before this log begins"), "{inherited}");
    // and it names the tool that settles it rather than leaving the model to infer a snapshot
    assert!(inherited.contains("`setup` with `model`"), "{inherited}");

    // the same where some of the named items do have a beginning and some do not
    assert!(mixed.contains("[2] has no"), "{mixed}");
    assert!(mixed.contains("context.added"), "{mixed}");
    // and silence where every named item was created here, because then there is nothing to say
    assert!(
        !native.contains("has no `context.added` here"),
        "an item this log saw created needs no note about snapshots: {native}"
    );
}

/// `since` is exclusive, and the schema says which number means everything.
///
/// note: also found live, and it cost the answer. A session reaching for "all of it" wrote the
/// first record's number, which is *after* that record - and in a resumed session the first record
/// is `session.resumed`, the one that would have told it the context was not its own. It read
/// everything except the thing it was looking for. A resumed log numbers on from the session it
/// carries on from, so that first number is not `1`.
#[tokio::test]
async fn since_the_first_record_is_not_since_the_beginning_and_the_schema_says_so() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("earlier"));
    let kernel = Kernel::resume(Config::default(), first.snapshot());
    let resumed = kernel.history()[0].seq;
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "log", json!({ "action": "read", "since": resumed })),
        call("c2", "log", json!({ "action": "read", "since": 0 })),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(kamchatka::tools::domains::log("read")),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(
        !said[0].contains("session.resumed"),
        "`since: {resumed}` is after record {resumed}, and that is the one that matters: {}",
        said[0]
    );
    assert!(
        said[1].contains("session.resumed"),
        "`since: 0` is the one that means all of them: {}",
        said[1]
    );

    let spec = kernel.tool("log").expect("installed").spec();
    let since = branch(&spec.schema, "read")["properties"]["since"]["description"]
        .as_str()
        .expect("it says what it is for");
    assert!(
        since.contains("`0` is all of them"),
        "the off-by-one is not guessable, so it is written down: {since}"
    );
}

/// An empty filter is no filter; a filter full of the wrong thing is a mistake.
///
/// note: found live. A model spelling "every argument the schema lists, none of them constraining
/// anything" wrote `{ids: [], kinds: [], since: 0, take: 20}` and was refused, which cost it a
/// turn and taught it nothing - an empty list constrains nothing, and reading it as "no filter" is
/// the only thing it can mean. A list with entries that are not item numbers is a different thing
/// and still worth reporting - one bad entry among good ones included, since dropping it would
/// answer a filter on the rest as though that were what was asked.
#[tokio::test]
async fn an_empty_filter_list_is_no_filter_rather_than_a_refusal() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "action": "read", "ids": [], "kinds": [], "since": 0, "take": 2, "whole": false }),
        ),
        call("c2", "log", json!({ "action": "read", "ids": ["two"] })),
        call("c3", "log", json!({ "action": "read", "ids": [12, -1] })),
    ]));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("match since:0"), "{}", said[0]);
    assert!(
        !said[0].contains("ids:["),
        "an empty list is not reported as a filter that was applied: {}",
        said[0]
    );
    assert!(said[0].contains("Showing the 2 most recent"), "{}", said[0]);
    // and a list of the wrong thing still says so, naming what it was given
    assert!(said[1].contains("holds `\"two\"`"), "{}", said[1]);
    assert!(
        said[2].contains("holds `-1`"),
        "an entry that is not an item number was dropped: {}",
        said[2]
    );
}

/// A compaction pass says why it ran, because the record it came from says why.
///
/// note: found live, watching a session read four passes back. The line said `1 out, 4 elided,
/// 8863 → 725 tokens` and stopped - what moved, and nothing about what moved it. The report
/// carries the compactor's own sentence, naming the threshold it crossed and by how much, and
/// that is the half which says whether a pass was the system working or the limit being wrong.
#[tokio::test]
async fn a_compaction_record_says_what_moved_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "kinds": ["context.compacted"] }),
    )]));

    let big = kernel.push(ContextItem::file("big.txt", "a".repeat(4_000)));
    kernel.push(ContextItem::user("carry on"));
    kernel.apply_compaction(nachalnik::CompactionPlan {
        elide: vec![big],
        remove: Vec::new(),
        summary: None,
        reason: "compacted to make room; the context had reached 122% of the limit".into(),
    });

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("context.compacted"), "{said}");
    assert!(said.contains("1 elided"), "what moved: {said}");
    assert!(
        said.contains("reached 122%"),
        "and what moved it, which the record has carried all along: {said}"
    );
}

/// A drained log says it was drained, rather than that nothing happened.
///
/// note: the sentence this replaces said "this session's log is empty, which is not the same as a
/// log you have not been shown" - in a session whose log had been taken away and written
/// elsewhere, which is exactly a log you are not being shown. `Kernel::drain_history` is the
/// supported way to stop a long session growing forever and it leaves the sequence counter alone,
/// so the two cases are told apart by arithmetic that was already there. Reporting the second as
/// the first is the one mistake this tool exists not to make, and the empty case was making it in
/// so many words.
///
/// note: the one test in this file that calls the tool rather than driving the loop. An empty log
/// cannot survive a turn - the turn records itself into it - so there is no script that reaches
/// this state through a request, and what is being checked is a sentence rather than anything the
/// loop does.
#[tokio::test]
async fn a_drained_log_says_so_instead_of_saying_nothing_happened() {
    let (kernel, _provider, _anchor) = agent(Vec::new());
    kernel.push(ContextItem::user("carry on"));
    let through = kernel.last_seq();
    let taken = kernel.drain_history(through);
    assert!(!taken.is_empty(), "there was something to drain");

    let said = kernel
        .tool("log")
        .expect("installed")
        .invoke(
            &nachalnik::ToolCall::new("c1", "log", json!({ "action": "read" })),
            nachalnik::OutputSink::disconnected(),
        )
        .await
        .expect("it answered")
        .content
        .to_text()
        .into_owned();
    assert!(said.contains("drained"), "{said}");
    assert!(
        said.contains(&format!("{through} record(s) have been through it")),
        "it says how many went, which is the part a count of zero cannot: {said}"
    );
    assert!(
        !said.contains("nothing has happened"),
        "an emptied log is not an empty one: {said}"
    );

    // note: and on a kernel there is no other way to be empty. A fresh one has already recorded
    // `session.started`, so an empty log with a counter above zero is always a drained one - which
    // is what makes the old sentence's confident "nothing has been recorded yet" wrong in every
    // case it could actually be printed in, rather than merely wrong sometimes.
    let quiet = Kernel::new(Config::default());
    assert_eq!(quiet.last_seq(), 1, "a kernel records its own beginning");
    assert_eq!(quiet.history().len(), 1);
}

/// An item with no beginning here names the right reason for it having none.
#[tokio::test]
async fn a_drained_log_blames_the_drain_rather_than_a_snapshot() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "ids": [1] }),
    )]));
    let item = kernel.push(ContextItem::user("carry on"));
    assert_eq!(item, nachalnik::ContextId(1));
    // everything up to and including this item's own `context.added` goes
    kernel.drain_history(kernel.last_seq());
    kernel.push(ContextItem::user("and on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]).remove(0);
    assert!(said.contains("[1] has no `context.added` here"), "{said}");
    assert!(
        said.contains("were drained"),
        "a log that starts late says the records went, not that the context was inherited: {said}"
    );
    assert!(
        !said.contains("resumed from a snapshot"),
        "and does not offer the wrong cause: {said}"
    );
}
