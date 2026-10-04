//! `log`: the record kept beside the context, what reading it costs, and the questions only an
//! absence in it can answer.

use crate::{agent, answered, answers_from, branch, offers, one_turn};
use kamchatka::{introspect, tools::Careful, tools::Limits, tools::Subject};
use nachalnik::{
    Config, ContextItem, ContextKind, ContextState, Kernel, ModelResponse, Verdict,
    test::ScriptedProvider, test::call,
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

/// The summary counts each kind, and the count is the number of records of it.
///
/// note: the histogram is the only thing a bare call says about what happened, so a count that
/// was really "this kind happened" answers the question a model reaches for the log with -
/// how many tool calls, how many edits - and answers it with one every time.
#[tokio::test]
async fn the_summary_counts_each_kind_rather_than_saying_that_it_happened() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read" }),
    )]));

    for n in 0..7 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]).remove(0);
    // the total the answer opens with, and the rows under it, which have to account for it
    let total: usize = said
        .split(' ')
        .next()
        .expect("every answer opens with a count")
        .replace(',', "")
        .parse()
        .expect("a figure");
    let rows: Vec<(&str, usize)> = said
        .lines()
        .filter_map(|line| {
            let (name, count) = line.trim_end().rsplit_once(' ')?;
            let (name, _) = name.rsplit_once(' ')?;
            Some((name, count.replace(',', "").parse().ok()?))
        })
        .filter(|(name, _)| !name.ends_with(','))
        .collect();
    assert!(
        rows.len() > 1,
        "a session that did several kinds of thing has a row for each: {said}"
    );
    assert!(
        rows.iter().any(|(_, count)| *count > 1),
        "and a kind that happened more than once is counted more than once: {said}"
    );
    assert_eq!(
        rows.iter().map(|(_, count)| count).sum::<usize>(),
        total,
        "the counts and the total are one pass over one log: {said}"
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
/// invite - is already caught elsewhere,
/// `undo::a_replacement_is_the_one_thing_that_would_otherwise_be_lost` among them, so this is not
/// the guard on that and saying it was would have been a false sense of a well-watched seam. What nothing else catches is the
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

/// A `take` above the ceiling is taken as the ceiling, and the answer says which and why.
///
/// note: the same clamp as `search` gets, and the same reason, one step worse. A session log is
/// append-only and grows for as long as the run does, so unlike a search - whose matches depend
/// on what the model has in front of it - it is guaranteed to cross any fixed bound eventually. A
/// live `log read` with `take: 999999` returned 9,637 tokens into a context already at 104% of its
/// limit, the compactor elided it on arrival, and the record of the read was itself in the log
/// that had just been elided: a session that read its own log in bulk could not read it again to
/// find out why.
#[tokio::test]
async fn a_take_wider_than_the_ceiling_is_clamped_and_says_so() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "take": 100_000 }),
    )]));

    // more records than the ceiling, so the clamp and not the size of the log is what stops it
    for n in 0..200 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"])[0].clone();
    // the total is the true one, whatever the `take` was
    assert!(said.contains(" records, ~"), "{said}");
    // and only the ceiling of them is here
    assert!(said.contains("Showing the 64 most recent"), "{said}");
    assert!(
        said.contains("`take` is at most 64"),
        "a clamped answer that does not say it was clamped reads as the whole of the log: {said}"
    );
    assert!(
        said.contains("`since` past the last record shown"),
        "and says how to reach the rest, which `since` can do: {said}"
    );
    let numbered: Vec<&str> = said
        .lines()
        .filter(|line| line.starts_with("  ") && line.trim().starts_with(char::is_numeric))
        .collect();
    assert_eq!(numbered.len(), 64, "the ceiling of them: {said}");
}

/// A `take` under the ceiling is left alone, and the sentence about the ceiling is not in it.
///
/// note: the ceiling is itself a case. A `take` of exactly the ceiling stopped the answer short
/// just as one above it did, and nothing was clamped off it - every record asked for arrived - so
/// saying so was a claim about a clamp that never happened.
#[tokio::test]
async fn a_take_under_the_ceiling_is_not_announced_as_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "action": "read", "take": 3 })),
        // the ceiling itself, over a log longer than it, so the answer is short either way
        call("c2", "log", json!({ "action": "read", "take": 64 })),
    ]));

    // more records than the ceiling, so a `take` of it really does leave some behind
    for n in 0..70 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("Showing the 3 most recent"), "{}", said[0]);
    assert!(
        !said[0].contains("`take` is at most"),
        "the ceiling is said where it stopped the answer short, and nowhere else: {}",
        said[0]
    );

    assert!(
        said[1].contains("Showing the 64 most recent"),
        "{}",
        said[1]
    );
    assert!(
        !said[1].contains("`take` is at most"),
        "a `take` of exactly the ceiling asked for no more than the ceiling holds, so nothing was \
         taken off it and saying otherwise is a clamp that did not happen: {}",
        said[1]
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
        // an action that is not a word, and a kind that is not a name beside one that is: the
        // first read as a bare `read`, the second as a narrower filter than the one asked for
        call("c3", "log", json!({ "action": 7 })),
        call(
            "c4",
            "log",
            json!({ "action": "read", "kinds": ["context.added", 3] }),
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
    assert!(said[2].contains("there is no `7`"), "{}", said[2]);
    assert!(
        said[3].contains("`kinds` is a list of names"),
        "{}",
        said[3]
    );
    assert!(!said[3].contains("match"), "{}", said[3]);
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

/// A resumed log says it was resumed, wherever an answer turns on the records it does not hold.
///
/// note: the rule the tool states in its own module - a short answer is never absence - was false
/// in a resumed session, and a resumed session is the one this tool is for. A kernel continues
/// numbering from the snapshot's last record, so a resumed log always starts above 1, and the
/// branch that read the number alone called that a drain: false, and on the paths where the number
/// is never printed, not said at all. A session resumed over a `context.replaced` asked for that
/// kind and got "0 match, nothing matched" beside a total, with nothing saying the record is in
/// the log written beside the snapshot rather than here. So all three of the answers that turn on
/// records this log has not got say where the log begins, and say *resumed* rather than *drained*
/// when that is what happened to it.
#[tokio::test]
async fn a_resumed_log_says_it_was_resumed_rather_than_drained_where_it_matters() {
    let first = Kernel::new(Config::default());
    for n in 0..40 {
        first.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    let item = first.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    first
        .replace(item, "the parser is in src/parse.rs")
        .unwrap();
    let kernel = Kernel::resume(Config::default(), first.snapshot());
    let resumed = kernel.history()[0].seq;
    // the last record numbered before this log begins, which is one `since` can name and this log
    // cannot show - and in a resumed session it is the session it was resumed from that holds it
    let before = resumed - 1;
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        // a kind the session it was resumed from held and this one has not
        call(
            "c1",
            "log",
            json!({ "action": "read", "kinds": ["context.replaced"] }),
        ),
        // a `since` below the first record here, so the record it names is not in this log
        call(
            "c2",
            "log",
            json!({ "action": "read", "since": before - 1 }),
        ),
        // an item from before the resume, which has no beginning here
        call("c3", "log", json!({ "action": "read", "ids": [2] })),
        // and the `since` on the other side of the same line: `since: {before}` is the record
        // right before this log begins, so every record it asks for after that one is here
        call("c4", "log", json!({ "action": "read", "since": before })),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(kamchatka::tools::domains::log("read")),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("what did I do before you started?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said {
        assert!(
            !answer.contains("drained"),
            "nothing was drained - the log begins at a resume, and saying otherwise is false: \
             {answer}"
        );
        assert!(
            answer.contains(&format!("begins at record {resumed}")),
            "a resumed log says where it begins, since a model reading a zero beside a total \
             would otherwise take it for the whole of what happened: {answer}"
        );
    }

    // and each of the three is told in the words that are true of it, rather than left to the
    // shared sentence above them
    assert!(
        said[0].contains("0 match") && said[0].contains("Nothing matched"),
        "a kind this session has not emitted matches nothing, and says so: {}",
        said[0]
    );
    assert!(
        said[0].contains("resumed"),
        "and says why the kind is missing from it: {}",
        said[0]
    );
    assert!(
        said[1].contains(&format!("Nothing numbered {before} to {before} is here")),
        "`since: {}` named a record this log has never had, and an answer that did not say so \
         read as there being none: {}",
        before - 1,
        said[1]
    );
    assert!(
        said[2].contains("[2] has no `context.added` here"),
        "an item from the session this was resumed from has no beginning here: {}",
        said[2]
    );
    // and the other side of the same line: `since: {before}` asks for the records after the last
    // one this log has not got, so nothing was skipped. Off by one either way and it either
    // misses a record it does not have or reports a range of records it does.
    assert!(
        !said[3].contains("is here, so `since"),
        "`since: {before}` names the record immediately before this log, and every record it \
         reaches is here: {}",
        said[3]
    );
    assert!(
        said[3].contains(&format!("match since:{before}")),
        "and it answered with the records it was asked for: {}",
        said[3]
    );
}

/// And so do the answers that found something, because a count of them reads as the whole session.
///
/// note: found live. A resumed session asked how many tool calls there had been, read the summary
/// and a `kinds` filter, and was told neither that the log begins at the resume - both counted
/// only what had happened since - so it had nothing to say but a guess.
#[tokio::test]
async fn a_resumed_log_says_so_in_its_summary_and_in_what_it_found() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::memory("scratch", "a note from before"));
    let kernel = Kernel::resume(Config::default(), first.snapshot());
    let resumed = kernel.history()[0].seq;
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "log", json!({ "action": "read" })),
        call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["session.resumed"] }),
        ),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(kamchatka::tools::domains::log("read")),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("how many tool calls have there been?"));

    kernel.turn().await.expect("the turn failed");

    for answer in answers_from(&kernel, &["log"]) {
        assert!(
            answer.contains(&format!("begins at record {resumed}")),
            "{answer}"
        );
    }
}

/// The two causes stay two, and the drained one still says it.
///
/// note: the other half of the test above, because a fix that made every log starting above 1 say
/// it was resumed would be wrong in the other direction: `Kernel::drain_history` leaves the
/// counter alone and a drained log is exactly a log whose first record is not 1, with nothing
/// having been resumed. A model told a drained log was resumed would go looking for a snapshot
/// that does not exist.
#[tokio::test]
async fn a_drained_log_still_says_it_was_drained_where_a_resumed_one_says_it_was_resumed() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "action": "read", "kinds": ["context.replaced"] }),
        ),
        call("c2", "log", json!({ "action": "read", "since": 0 })),
    ]));
    for n in 0..5 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    let through = kernel.last_seq();
    let taken = kernel.drain_history(through);
    assert!(!taken.is_empty(), "there was something to drain");
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said {
        assert!(
            answer.contains("drained"),
            "records were carried away and kept where this session cannot read them, which is not \
             a resume: {answer}"
        );
        assert!(
            !answer.contains("resumed"),
            "nothing was resumed here, and a model told so would go looking for a snapshot: \
             {answer}"
        );
    }
    assert!(
        said[1].contains(&format!("Nothing numbered 1 to {through} is here")),
        "`since: 0` named the drained records, which is a range with nothing in it here: {}",
        said[1]
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

/// `whole` written in quotes is read as the word it is.
#[tokio::test]
async fn a_whole_record_can_be_asked_for_in_quotes() {
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
        call(
            "c2",
            "log",
            json!({ "action": "read", "ids": [1], "whole": "true" }),
        ),
    ]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs\nand the lexer is in src/lex.rs",
    ));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("src/lex.rs"), "{}", said[0]);
}

/// An item that never existed is said not to, rather than to have been inherited.
#[tokio::test]
async fn an_item_that_never_existed_is_not_reported_as_inherited() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "ids": [999] }),
    )]));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = &answers_from(&kernel, &["log"])[0];
    assert!(!said.contains("already in the context"), "{said}");
    assert!(said.contains("[999]"), "{said}");
}

/// Every kind of record that names an item is found by its number, and one that does not is not.
///
/// note: `ids` is the whole of what makes a log worth having beside the context - "what happened
/// to 12?" is a question the context cannot answer, because the context only holds what 12 says
/// now. So the filter has to reach every record that names an item, and *only* those: an `ids`
/// filter that matched every record would answer every such question with the whole session, and
/// one that missed a kind would answer "nothing happened" about a change that did.
///
/// note: the two halves are checked apart, because they fail in opposite directions and each kind
/// is a separate arm of the match. The negatives are what catch a filter that matches everything -
/// which is what a `names` that answers `true` to everything amounts to - and the kind filter is
/// beside every one of them so that a wrong match is a wrong match *about that kind*.
#[tokio::test]
async fn an_ids_filter_finds_a_record_by_its_number_whatever_kind_of_record_it_is() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        // something whose answer is recorded as a context item of its own, which is the item a
        // `tool.finished` names
        call("c1", "context", json!({ "action": "look", "ids": [1] })),
        call(
            "c2",
            "log",
            json!({ "action": "read", "kinds": ["context.undone"], "ids": [3] }),
        ),
        call(
            "c3",
            "log",
            json!({ "action": "read", "kinds": ["context.redone"], "ids": [3] }),
        ),
        call(
            "c4",
            "log",
            json!({ "action": "read", "kinds": ["context.compacted"], "ids": [3] }),
        ),
        call(
            "c5",
            "log",
            json!({ "action": "read", "kinds": ["model.requested"], "ids": [3] }),
        ),
        call(
            "c6",
            "log",
            json!({ "action": "read", "kinds": ["tool.finished"], "ids": [6] }),
        ),
        // and the same kinds asked about an item none of them is about
        call(
            "c7",
            "log",
            json!({ "action": "read", "kinds": ["context.undone"], "ids": [1] }),
        ),
        call(
            "c8",
            "log",
            json!({ "action": "read", "kinds": ["context.redone"], "ids": [1] }),
        ),
        call(
            "c9",
            "log",
            json!({ "action": "read", "kinds": ["context.compacted"], "ids": [1] }),
        ),
        call(
            "c10",
            "log",
            json!({ "action": "read", "kinds": ["tool.finished"], "ids": [1] }),
        ),
    ]));
    kernel.push(ContextItem::memory("scratch", "a note"));
    kernel.push(ContextItem::memory("scratch", "another note"));
    let third = kernel.push(ContextItem::file("big.txt", "x".repeat(400)));
    // one undo and one redo of a push, so the two records name an item by taking it away and by
    // putting it back rather than by a state it was moved to
    assert!(kernel.undo().expect("there was something to undo"));
    assert!(kernel.redo().expect("there was something to redo"));
    kernel.apply_compaction(nachalnik::CompactionPlan {
        elide: vec![third],
        remove: Vec::new(),
        summary: None,
        reason: "it is most of the context".into(),
    });
    kernel.push(ContextItem::user("what happened to 3?"));

    kernel.turn().await.expect("the turn failed");

    // note: the numbering the calls were written against, said out loud so that a change to it is
    // a failure here rather than a test quietly checking nothing. Four items were in the context,
    // so the turn's own assistant message is the fifth and the first answer the sixth - and it is
    // the answer a `tool.finished` names, which is what `c6` is asking about.
    assert!(
        matches!(
            &kernel.item(nachalnik::ContextId(6)).expect("it is in the context").kind,
            ContextKind::ToolResult { tool, .. } if tool == "context"
        ),
        "the first answer is item 6, and the record naming it is `tool.finished`"
    );

    let said = answers_from(&kernel, &["log"]);
    // each kind of record that names item 3, found by its number
    assert!(said[0].contains("1 match"), "{}", said[0]);
    assert!(said[1].contains("1 match"), "{}", said[1]);
    assert!(said[2].contains("1 match"), "{}", said[2]);
    // and the request this turn is making carries it, so it is about it too
    assert!(said[3].contains("1 match"), "{}", said[3]);
    assert!(said[4].contains("1 match"), "{}", said[4]);

    // and none of them is about item 1, which none of those five kinds names - a filter that
    // matched every record would answer every one of these with the record it must not
    for answer in &said[5..] {
        assert!(
            answer.contains("0 match"),
            "an `ids` filter matches the records that name the item, not every record: {answer}"
        );
        assert!(
            answer.contains("Nothing matched"),
            "and a filter that found nothing says so in words: {answer}"
        );
    }
}

/// The line for a replaced item admits to being one line only when there is more of the text.
///
/// note: `ContextReplaced` is the one record carrying content, and a short text is all on its line
/// already - a marker sending the model to `whole` for it would send it for nothing.
#[tokio::test]
async fn a_replaced_items_short_text_is_not_said_to_have_more_of_it() {
    let (kernel, _provider, _anchor) = agent(Vec::new());
    let item = kernel.push(ContextItem::memory("scratch", "x"));
    kernel.replace(item, "y").expect("the item is there");

    let said = kernel
        .tool("log")
        .expect("it is installed")
        .invoke(
            &nachalnik::ToolCall::new("c1", "log", json!({ "action": "read", "ids": [1] })),
            nachalnik::OutputSink::disconnected(),
        )
        .await
        .expect("it answered")
        .content
        .to_text()
        .into_owned();
    assert!(said.contains("context.replaced"), "{said}");
    assert!(!said.contains("bytes in all"), "{said}");
}

/// `ids` reaches a compaction by every list in its report, the summary beside the others.
#[tokio::test]
async fn a_compaction_is_found_by_the_summary_and_by_what_it_took() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "action": "read", "ids": [1], "kinds": ["context.compacted"] }),
        ),
        call(
            "c2",
            "log",
            json!({ "action": "read", "ids": [3], "kinds": ["context.compacted"] }),
        ),
    ]));

    let file = kernel.push(ContextItem::file("big.txt", "a".repeat(4_000)));
    kernel.push(ContextItem::user("carry on"));
    kernel.apply_compaction(nachalnik::CompactionPlan {
        elide: vec![file],
        remove: Vec::new(),
        summary: Some(ContextItem::memory(
            "scratch",
            "the parser is in src/parser.rs",
        )),
        reason: "compacted to make room; the context had reached 122% of the limit".into(),
    });
    let summary = kernel.items().last().expect("the summary is in").id;
    assert_eq!(
        summary,
        nachalnik::ContextId(3),
        "the summary is after what was elided"
    );

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (by_taken, by_summary) = (&said[0], &said[1]);
    assert!(
        by_taken.contains("1 match") && by_taken.contains("context.compacted"),
        "the record of a pass that elided item 1 is found by its number: {by_taken}"
    );
    assert!(
        by_summary.contains("1 match") && by_summary.contains("context.compacted"),
        "and the same record is found by the summary the pass put in its place: {by_summary}"
    );
}

/// `ids` reaches a request record by an item the projector left out of it.
///
/// note: `model.requested` names two lists, and an excluded item is in only the second - "what
/// was not sent, and why" is the whole reason that list exists, and this is what a model asking
/// why a file was not in the request reads the log to find. A filter that matched only what went
/// out would answer that with silence for every withheld item there is.
#[tokio::test]
async fn a_request_is_found_by_an_item_the_projector_left_out_of_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "ids": [1], "kinds": ["model.requested"] }),
    )]));

    let file = kernel.push(ContextItem::file("a.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("carry on"));
    kernel.set_state([file], ContextState::Excluded, Some("done with it".into()));

    kernel.turn().await.expect("the turn failed");

    let said = &answers_from(&kernel, &["log"])[0];
    assert!(
        !said.contains("Nothing matched"),
        "a request that left item 1 out is a record about it, and it is found by its number: {said}"
    );
    assert!(
        said.contains("model.requested") && said.contains("match"),
        "and it is the request record itself: {said}"
    );
}

/// The limit table is this tool's limit, and the answer the model is shown obeys it.
///
/// note: `Tool::limit` is the one method here that decides how much of what a call returns the
/// model gets to see, so a tool that declared nothing would hand back a log of whatever length
/// the session happened to reach - on the very answer that reads as the cheapest thing an agent
/// can ask for. A limit a table nobody consults is not a limit, so this asks for the answer a
/// small row produces, and the row is moved with the same handle `/limit` moves.
#[tokio::test]
async fn the_log_says_no_more_than_the_limits_table_allows_it_to() {
    let limits = Limits::new();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "log",
        json!({ "action": "read", "kinds": ["context.added"] }),
    )]))));
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::parse("log"), Verdict::Allow);
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, limits.clone());

    for n in 0..150 {
        kernel.push(ContextItem::memory("scratch", format!("note number {n}")));
    }
    limits.set("log:read", 1_000).expect("a row for it");
    kernel.push(ContextItem::user("what has happened?"));

    kernel.turn().await.expect("the turn failed");

    // the copy the model was shown, which is the last one recorded: with `keep_truncated_output`
    // the whole is put in beside it, excluded, and it is the longer of the pair
    let said = answered(&kernel);
    assert!(
        said.contains("truncated by an output limit"),
        "the log declared no limit, so a log of a long session is handed back whole however \
         large it is: {said}"
    );
}
