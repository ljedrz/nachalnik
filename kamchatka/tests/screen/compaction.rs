//! Compaction, watched from the screen.
//!
//! note: a compactor is the one thing allowed to change a request after it has been previewed, so
//! it has to report exactly what it did. These check the report against the rows: a pin is
//! honoured, a marker is not swapped in for something smaller than itself, and a compactor with
//! nothing left to elide is asked once and then left alone.

use std::sync::Arc;

use kamchatka::tools::Trim;
use nachalnik::{ContextItem, ContextState, test::call};
use serde_json::json;

use crate::harness::Harness;

/// A compaction pass leaves the conversation coherent: the calls the model made are still on the
/// record, each with an answer saying its result was compacted.
///
/// note: the reason this program elides rather than removes. Removing a tool result forces the
/// projector to drop the call that asked for it - a call with no result is a request most
/// providers reject - so the model was left reading a history in which it never asked for any of
/// this, immediately above a summary saying the results had been dropped.
#[tokio::test]
async fn compaction_shortens_a_result_without_unasking_the_question() {
    use kamchatka::tools::Trim;
    use nachalnik::{Compactor, Content, ToolCall};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    let call = ToolCall::new(
        "call-1",
        "read",
        std::sync::Arc::new(json!({"path": "big.rs"})),
    );
    kernel.push(ContextItem::user("what is in big.rs?"));
    kernel.push(ContextItem::assistant(
        Content::text("let me look"),
        vec![call.clone()],
    ));
    let result = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        Content::text("x".repeat(40_000)),
        false,
    ));

    let trim = Trim {
        threshold: 0.0,
        target: 0.0,
    };
    let plan = trim
        .plan(&kernel.items(), &kernel.budget())
        .await
        .expect("something to compact");
    assert_eq!(plan.elide, vec![result], "it elides rather than removes");
    assert!(plan.remove.is_empty());

    let report = kernel.apply_compaction(plan);
    assert_eq!(report.elided.len(), 1);
    assert!(
        report.tokens_after < report.tokens_before / 10,
        "{} -> {}",
        report.tokens_before,
        report.tokens_after
    );

    let request = kernel.preview_request().expect("a request");
    let sent = format!("{:?}", request.messages);
    assert!(!sent.contains("xxxx"), "the content is gone");
    assert!(
        sent.contains("compacted to make room"),
        "and says so where it was: {sent}"
    );
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|m| !m.tool_calls.is_empty())
            .count(),
        1,
        "the call it made is still on the record: {sent}"
    );
    assert!(
        kernel.project().repairs.is_empty(),
        "so nothing had to be repaired"
    );

    // and it is still on the tab, marked, restorable, with what it holds counted as held back
    assert_eq!(kernel.item(result).unwrap().state, ContextState::Elided);
    assert_eq!(
        kernel.with_context(|c| c.tokens_withheld()),
        kernel.item(result).unwrap().tokens
    );
}

/// A second pass with nothing left to elide is not a pass. `Trim` runs before every request, so a
/// pass that answers with a plan whatever the state of the context adds a summary and burns an
/// undo on every one of them - and the context it is meant to shrink grows for the rest of the
/// session.
#[tokio::test]
async fn a_compactor_with_nothing_left_to_elide_stops_asking() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // a pinned file bigger than the target is all it takes: the pass can never get under it,
    // however much it elides, so it is asked again on the next request and the one after
    kernel.push(ContextItem::file("big.rs", "x".repeat(4_000)).pinned());
    // the call as well as its result, because a result answering no call is repaired out of the
    // request, and the kernel will not elide something the request is not carrying
    let call = nachalnik::ToolCall::new("c1", "shell", std::sync::Arc::new(json!({})));
    kernel.push(ContextItem::assistant(
        nachalnik::Content::text("let me look"),
        vec![call.clone()],
    ));
    kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "shell",
        "y".repeat(400),
        false,
    ));

    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("the context is over the threshold");
    assert_eq!(plan.elide.len(), 1);
    assert_eq!(kernel.apply_compaction(plan).elided.len(), 1);

    let (items, undo) = (kernel.items().len(), kernel.with_context(|c| c.undo_len()));
    assert!(
        trim.should_compact(&budget()),
        "still over the threshold, which is the case this is about"
    );
    for _ in 0..5 {
        if let Some(plan) = trim.plan(&kernel.items(), &budget()).await {
            kernel.apply_compaction(plan);
        }
    }
    assert_eq!(
        kernel.items().len(),
        items,
        "five more passes and the context grew"
    );
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        undo,
        "and spent the person's undo doing it"
    );
}

/// A pinned tool result is one `Trim` may not have, and naming it anyway is not free: the kernel
/// refuses it, the plan is a plan all the same, and a plan carries a summary. One pinned result
/// bigger than the target keeps the context over the threshold for the rest of the session, so
/// against a live endpoint this added a summary and burned an undo before every request, growing
/// the request 53 tokens a turn - the thing the pass exists to stop.
#[tokio::test]
async fn compaction_does_not_ask_for_a_result_that_is_pinned() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    let call = nachalnik::ToolCall::new("c1", "shell", std::sync::Arc::new(json!({})));
    kernel.push(ContextItem::assistant(
        nachalnik::Content::text("let me look"),
        vec![call.clone()],
    ));
    let result = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "shell",
        "y".repeat(4_000),
        false,
    ));
    kernel.set_state(
        [result],
        ContextState::Pinned,
        Some("this has to last".into()),
    );

    let trim = Trim {
        threshold: 0.2,
        target: 0.1,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };
    assert!(
        trim.should_compact(&budget()),
        "well over the threshold, which is the case this is about"
    );

    let (items, undo) = (kernel.items().len(), kernel.with_context(|c| c.undo_len()));
    for _ in 0..5 {
        assert!(
            trim.plan(&kernel.items(), &budget()).await.is_none(),
            "there is nothing here it may take, so there is no plan to make"
        );
    }
    assert_eq!(kernel.items().len(), items, "and the context did not grow");
    assert_eq!(kernel.with_context(|c| c.undo_len()), undo);
    assert_eq!(
        kernel.item(result).unwrap().state,
        ContextState::Pinned,
        "and the pin held"
    );
}

/// A marker is not free, and a pass that credits itself with the whole of what it elided is
/// counting on it being. `Trim` subtracted each item's tokens and put a line of its own reason
/// where the content had been, so on a context full of small results the arithmetic said it had
/// recovered hundreds of tokens while the request it was making got bigger.
#[tokio::test]
async fn compaction_does_not_elide_a_result_smaller_than_the_marker_replacing_it() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor, Content};

    let mut harness = Harness::new([]);
    let kernel = harness.app.kernel.clone();

    // the bulk is something the pass cannot touch, so the loop runs to the end of its candidates
    // and never reaches the target however many of them it takes
    kernel.push(ContextItem::file("big.rs", "x".repeat(2_600)).pinned());
    // twenty results the size a `write` confirmation really is: "wrote 412 bytes to /w/foo.rs".
    // Each one answers a call that is really in the context, or the projector repairs it away
    // and there is nothing here to elide
    for i in 0..20 {
        let asked = call(&format!("c{i}"), "write", json!({}));
        kernel.push(ContextItem::assistant(
            Content::text(""),
            vec![asked.clone()],
        ));
        kernel.push(ContextItem::tool_result(
            asked.id.clone(),
            "write",
            format!("wrote 412 bytes to /w/f{i}.rs"),
            false,
        ));
    }

    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    let before = budget().context_tokens;
    assert!(trim.should_compact(&budget()), "over the threshold");
    assert!(
        trim.plan(&kernel.items(), &budget()).await.is_none(),
        "there is nothing here worth eliding, so there is no plan"
    );
    assert_eq!(
        budget().context_tokens,
        before,
        "and nothing moved: the request is the one it was"
    );

    // the floor is a floor and not a refusal to work: one result worth eliding, in among the
    // twenty that are not, and the pass takes that one and leaves the rest alone
    let asked = call("big", "shell", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![asked.clone()],
    ));
    let big = kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "shell",
        "y".repeat(1_200),
        false,
    ));

    let with_big = budget().context_tokens;
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("one result is worth it");
    assert_eq!(plan.elide, vec![big], "only the one that pays for itself");
    kernel.apply_compaction(plan);
    assert!(
        budget().context_tokens < with_big,
        "and the request really did get smaller: {with_big} -> {}",
        budget().context_tokens
    );

    // and the person is told what really happened. This compactor never removes anything, so the
    // line naming only removals announced every pass it ever made as `0 items out`
    harness.drain();
    let screen = harness.flat();
    assert!(
        screen.contains("compacted: 1 elided"),
        "the announcement does not say what moved: {screen}"
    );
    assert!(
        !screen.contains("0 items out"),
        "and does not say nothing moved: {screen}"
    );
}

/// A blob goes first, ahead of results older than it, and the arithmetic gets no say. Every
/// counter in this workspace puts a `Content::Blob` at `0` tokens, so the two rules the rest of
/// this pass runs on - oldest first, and nothing smaller than its own marker - between them made
/// the largest thing in the context the one thing `Trim` could never take: ranked last by age,
/// then skipped for recovering nothing.
#[tokio::test]
async fn blobs_are_taken_before_anything_else() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // the older of the two, and the one the pass would otherwise reach first
    let read = call("c1", "read", json!({"path": "big.rs"}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![read.clone()],
    ));
    let text = kernel.push(ContextItem::tool_result(
        read.id.clone(),
        "read",
        "x".repeat(4_000),
        false,
    ));

    // the newest item in the context, and by a distance the biggest: 600 KB of base64 that the
    // budget is about to report as free
    let shot = call("c2", "screenshot", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![shot.clone()],
    ));
    let blob = kernel.push(ContextItem::tool_result(
        shot.id.clone(),
        "screenshot",
        Content::blob("image/png", "A".repeat(600_000)),
        false,
    ));

    assert_eq!(
        kernel.item(blob).unwrap().tokens,
        0,
        "the trap this is about: the counter abstains, so every size test here reads a \
         600 KB picture as free"
    );

    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("over the threshold on the text alone");
    assert_eq!(
        plan.elide,
        vec![blob, text],
        "the blob goes first, though it is the newest item and the one counted at nothing"
    );
    assert!(
        plan.summary
            .as_ref()
            .is_some_and(|summary| summary.content.to_text().contains("1 of them carrying an")),
        "and the model is told a picture was among them, since the marker it reads will only \
         mention a token limit: {:?}",
        plan.summary.as_ref().map(|s| s.content.to_text())
    );

    let report = kernel.apply_compaction(plan);
    assert_eq!(report.elided.len(), 2);
    let request = kernel.preview_request().expect("a request");
    let sent = format!("{:?}", request.messages);
    assert!(
        !sent.contains("AAAA"),
        "and the base64 is out of the request: {}",
        &sent[..sent.len().min(400)]
    );
}

/// The same, with the budget already where the pass wants it. A blob is not taken because taking
/// it helps the arithmetic - the arithmetic cannot see it - but because nothing here can say what
/// it costs, and an unbounded payload nobody can measure is the wrong thing to be carrying on a
/// guess.
#[tokio::test]
async fn a_blob_goes_even_when_the_count_says_there_is_room() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    let shot = call("c1", "screenshot", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![shot.clone()],
    ));
    let blob = kernel.push(ContextItem::tool_result(
        shot.id.clone(),
        "screenshot",
        Content::blob("image/png", "A".repeat(600_000)),
        false,
    ));

    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    // a limit the counted content is nowhere near, which is exactly the state a context full of
    // pictures reports: `used` is a handful of tokens and the request is megabytes
    let budget = || Budget {
        limit: Some(1_000_000),
        ..kernel.budget()
    };
    assert!(
        budget().used() <= (budget().limit.unwrap() as f64 * trim.target) as usize,
        "the target is already met on the counted tokens, so the ordinary loop stops at once"
    );
    assert!(
        budget().fraction_used().unwrap() < trim.threshold,
        "and the threshold is nowhere near, which is what a context of pictures always reports"
    );
    // so the pass is only reached at all because the budget says something in it has no price
    // on it. Without that, `should_compact` sleeps through the one state it is most needed in
    assert_eq!(budget().uncounted, 1);
    assert!(trim.should_compact(&budget()));

    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("a plan all the same");
    assert_eq!(plan.elide, vec![blob], "the picture, and nothing else");

    // and once it is a marker there is nothing unpriced left, so the pass stops being asked -
    // an abstention that outlived the content would ask for a plan before every request for the
    // rest of the session
    kernel.apply_compaction(plan);
    assert_eq!(budget().uncounted, 0);
    assert!(!trim.should_compact(&budget()));
}

/// A picture the pass may not touch does not turn into a plan. `should_compact` now answers yes
/// to anything unpriced, and a pinned one stays unpriced forever - so the pass is asked before
/// every request for the rest of the session, and each of those asks has to come back empty. A
/// plan is not nothing: it is a summary in the context and an undo spent.
#[tokio::test]
async fn an_unpriced_item_the_pass_may_not_take_produces_no_plan() {
    use kamchatka::tools::Trim;
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // a user's own attachment: not a tool result, so not a candidate at all, and this pass has
    // no business eliding what the person themselves put there
    kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "A".repeat(600_000),
    )));

    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000_000),
        ..kernel.budget()
    };
    assert!(trim.should_compact(&budget()), "something here is unpriced");

    let (items, undo) = (kernel.items().len(), kernel.with_context(|c| c.undo_len()));
    for _ in 0..5 {
        assert!(
            trim.plan(&kernel.items(), &budget()).await.is_none(),
            "there is nothing here it may take, so there is no plan to make"
        );
    }
    assert_eq!(kernel.items().len(), items, "and the context did not grow");
    assert_eq!(kernel.with_context(|c| c.undo_len()), undo);
}

/// The pass leaves one standing summary, not one per pass.
///
/// note: found live. Against a real endpoint at a 6,000-token limit, a tool loop left **twenty-one
/// identical summaries of 67 tokens each** - 1,407 tokens, a quarter of the budget, all of it the
/// same sentence, in a context the compactor had been called on to make room in. Every pass wrote
/// one and nothing ever took one back out: a summary is a `Reference`, and this pass only ever
/// considers a tool result.
#[tokio::test]
async fn compaction_summaries_do_not_pile_up() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let trim = Trim {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    // four passes with something new to take each time, which is what a tool loop looks like
    for n in 0..4 {
        let call = call(&format!("c{n}"), "peek", json!({}));
        kernel.push(ContextItem::assistant("looking", vec![call.clone()]));
        kernel.push(ContextItem::tool_result(
            call.id.clone(),
            "peek",
            "a line of routine diagnostic output. ".repeat(60),
            false,
        ));

        let plan = trim
            .plan(&kernel.items(), &budget())
            .await
            .expect("something to take");
        kernel.apply_compaction(plan);
    }

    let items = kernel.items();
    let standing: Vec<_> = items
        .iter()
        .filter(|item| item.source == "compaction" && item.state.sends_content())
        .collect();

    assert_eq!(
        standing.len(),
        1,
        "one summary belongs in the request, not one per pass: {:?}",
        standing
            .iter()
            .map(|item| item.content.to_text().into_owned())
            .collect::<Vec<_>>()
    );

    // and it speaks for every pass, since it is the only one left saying anything
    let said = standing[0].content.to_text();
    let elided = items.iter().filter(|item| item.state.is_elided()).count();
    assert!(
        said.starts_with(&format!("{elided} ")),
        "the standing sentence should count all {elided} of them: {said}"
    );

    // superseded rather than destroyed, like everything else here: the earlier ones are still
    // in the context to look at, and `u` puts one back
    assert!(
        items
            .iter()
            .any(|item| item.source == "compaction" && !item.state.sends_content()),
        "the earlier passes' summaries are kept, not dropped"
    );
}

/// Past the limit, the corner says the compactor stands between that figure and the request.
///
/// note: found live. A tool loop leaves the context fat, because a pass runs when a request is
/// *built* rather than when a turn ends - so the corner sat at `~16,254 tokens, 270.9% (6.0k)` in
/// red while the request that followed cost 1,100. The number was true of the context and false
/// of what was about to be sent, and nothing on the screen said which.
#[tokio::test]
async fn over_the_limit_the_corner_says_the_compactor_goes_first() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Trim {
        threshold: 0.8,
        target: 0.5,
    })));
    // a context that is genuinely over whatever the endpoint published
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(ContextItem::file(
        "big.txt",
        "a line of routine diagnostic output. ".repeat(limit),
    ));
    harness.drain();

    let screen = harness.flat();
    assert!(
        screen.contains("the compactor runs first"),
        "the corner should name what stands between the figure and the request: {screen}"
    );

    // and `/budget` has room for the half the corner cannot fit
    harness.send("/budget").await;
    let panel = harness.flat();
    assert!(
        panel.contains("runs before the next request is sent"),
        "{panel}"
    );
    assert!(
        panel.contains("everything it would take is pinned"),
        "it says what it cannot promise, too: {panel}"
    );
}

/// With no compactor there is nothing between the figure and the request, and it does not claim
/// otherwise.
#[tokio::test]
async fn over_the_limit_with_no_compactor_promises_nothing() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(None);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(ContextItem::file(
        "big.txt",
        "a line of routine diagnostic output. ".repeat(limit),
    ));
    harness.drain();

    let screen = harness.flat();
    assert!(
        !screen.contains("the compactor runs first"),
        "there is no compactor, so the request really is this big: {screen}"
    );
}
