//! Compaction, watched from the screen.
//!
//! note: a compactor is the one thing allowed to change a request after it has been previewed, so
//! it has to report exactly what it did. These check the report against the rows: a pin is
//! honoured, a marker is not swapped in for something smaller than itself, and a compactor with
//! nothing left to elide is asked once and then left alone.

use std::sync::Arc;

use crossterm::event::KeyCode;
use kamchatka::tools::Shedder;
use nachalnik::{ContextItem, ContextState, test::call};
use serde_json::json;

use crate::harness::{Harness, grouped};

/// A compaction pass leaves the conversation coherent: the calls the model made are still on the
/// record, each with an answer saying its result was compacted.
///
/// note: the reason this program elides rather than removes. Removing a tool result forces the
/// projector to drop the call that asked for it - a call with no result is a request most
/// providers reject - so the model was left reading a history in which it never asked for any of
/// this, immediately above a summary saying the results had been dropped.
#[tokio::test]
async fn compaction_shortens_a_result_without_unasking_the_question() {
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
    // read, so the pass may take it: one the model has not been shown is kept
    kernel.push(ContextItem::assistant(
        Content::text("it is forty thousand x"),
        vec![],
    ));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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
    // and says the model had read it, which is the half a live run needed: a model reading a
    // marker that said only "compacted" decided it had never seen the files it had just
    // summarised, called its summaries fabricated and withdrew them
    assert!(
        sent.contains("compacted after you had read it"),
        "and says so where it was: {sent}"
    );
    // and closes the retry, which is the other half: a model read a 10,000-token file into a
    // 9,000-token context three times, with a marker in front of it each time
    assert!(
        sent.contains("ask for the part you need rather than the whole"),
        "a marker that only says what happened is read as an invitation to try again: {sent}"
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

/// A second pass with nothing left to elide is not a pass. `Shedder` runs before every request, so a
/// pass that answers with a plan whatever the state of the context adds a summary and burns an
/// undo on every one of them - and the context it is meant to shrink grows for the rest of the
/// session.
#[tokio::test]
async fn a_compactor_with_nothing_left_to_elide_stops_asking() {
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
    // a result big enough to be worth taking, which is what the first pass needs: one that
    // recovers barely more than the summary it leaves behind is not a pass at all, and this is
    // about the passes *after* the first
    kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "shell",
        "y".repeat(4_000),
        false,
    ));
    // and read: what the model has not been shown is kept
    kernel.push(ContextItem::assistant("it is all y", vec![]));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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

/// A pinned tool result is one `Shedder` may not have, and naming it anyway is not free: the kernel
/// refuses it, the plan is a plan all the same, and a plan carries a summary. One pinned result
/// bigger than the target keeps the context over the threshold for the rest of the session, so
/// against a live endpoint this added a summary and burned an undo before every request, growing
/// the request 53 tokens a turn - the thing the pass exists to stop.
#[tokio::test]
async fn compaction_does_not_ask_for_a_result_that_is_pinned() {
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

    let trim = Shedder {
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

/// A pass that reaches for a pin is refused by the kernel, and the refusal is on the screen as well
/// as in the report.
///
/// note: the line naming what moved reads the same either way, so without it a person has no way
/// of knowing that what they pinned is still holding the context up.
#[tokio::test]
async fn a_pass_that_reaches_for_a_pin_says_it_was_refused() {
    use nachalnik::{Content, ToolCall};

    let mut harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let call = ToolCall::new("call-1", "read", Arc::new(json!({"path": "big.rs"})));
    kernel.push(ContextItem::user("what is in big.rs?"));
    kernel.push(ContextItem::assistant(
        Content::text("let me look"),
        vec![call.clone()],
    ));
    let pinned = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        Content::text("x".repeat(40_000)),
        false,
    ));
    kernel.push(ContextItem::assistant(Content::text("it is all x"), vec![]));
    kernel.set_state([pinned], ContextState::Pinned, None);

    // a plan that names it anyway, which is what a compactor working from a stale list does
    let report = kernel.apply_compaction(nachalnik::CompactionPlan {
        elide: vec![pinned],
        reason: "making room".into(),
        ..Default::default()
    });
    assert_eq!(report.refused.len(), 1, "the pin held");

    harness.drain();
    let said = harness
        .app
        .loose
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("1 refused, because pinned"), "{said}");
}

/// A marker is not free, and a pass that credits itself with the whole of what it elided is
/// counting on it being. `Shedder` subtracted each item's tokens and put a line of its own reason
/// where the content had been, so on a context full of small results the arithmetic said it had
/// recovered hundreds of tokens while the request it was making got bigger.
#[tokio::test]
async fn compaction_does_not_elide_a_result_smaller_than_the_marker_replacing_it() {
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

    let trim = Shedder {
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
    kernel.push(ContextItem::assistant("read it", vec![]));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
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

/// A pass that would free less than its markers and its summary cost takes nothing, counted on
/// the scale the counter is really using.
///
/// note: a counter that has corrected itself upwards prices a short result below what a marker
/// priced at four bytes a token really costs, and the pass then reports tokens recovered while the
/// request grows.
#[tokio::test]
async fn a_pass_that_would_grow_the_request_takes_nothing() {
    use nachalnik::{Budget, BytesPerToken, Calibrating, Calibration, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // what the counter has learnt by the time this pass runs. A session that has had a few
    // responses reads a third above the plain estimate, and every figure in the context is on
    // that scale - the projected ones and the ones on the items alike
    kernel.set_counter(Arc::new(Calibrating::new(BytesPerToken::default())));
    kernel.recalibrate(Calibration {
        scale: 1.4,
        observations: 3,
        estimated: 1_000,
        reported: 1_400,
    });

    // the bulk is something the pass cannot touch, so its loops run to the end of their
    // candidates and the one result below is the only thing there is to take
    kernel.push(ContextItem::file("big.txt", "x".repeat(2_600)).pinned());
    // 300 bytes: about 105 tokens on the corrected scale, short of what this pass has to free
    // before its own summary pays for itself. Priced at four bytes a token the same result read
    // as 52 tokens clear of a marker, and the pass took it
    let asked = call("c1", "context:pin", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![asked.clone()],
    ));
    let pin = kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "context:pin",
        "y".repeat(300),
        false,
    ));
    // read, so that what decides it is the arithmetic and not that it is new
    kernel.push(ContextItem::assistant("pinned", vec![]));

    let trim = Shedder {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    let tokens = kernel.item(pin).unwrap().tokens;
    assert!(
        tokens > 50 && tokens < 184,
        "a result between what a marker costs on the raw scale and what it costs on this one: \
         {tokens}"
    );

    let (items, undo) = (kernel.items().len(), kernel.with_context(|c| c.undo_len()));
    assert!(
        trim.should_compact(&budget()),
        "over the threshold, which is the case this is about"
    );
    assert!(
        trim.plan(&kernel.items(), &budget()).await.is_none(),
        "eliding that one would put a longer sentence where it was, so there is no pass to make"
    );
    assert_eq!(kernel.items().len(), items, "and the context did not grow");
    assert_eq!(kernel.with_context(|c| c.undo_len()), undo, "nor the undo");
    assert_eq!(
        kernel.item(pin).unwrap().state,
        ContextState::Active,
        "and the result is still there to be read"
    );
}

/// The floor on a marker is not a reason to take nothing. A result that pays for its own marker
/// and its pass's summary is taken whatever the scale is read at, and the request really does get
/// smaller - which is the whole of what the compactor is for.
#[tokio::test]
async fn a_result_worth_more_than_its_marker_and_the_summary_is_still_taken() {
    use nachalnik::{Budget, BytesPerToken, Calibrating, Calibration, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    kernel.set_counter(Arc::new(Calibrating::new(BytesPerToken::default())));
    kernel.recalibrate(Calibration {
        scale: 1.4,
        observations: 3,
        estimated: 1_000,
        reported: 1_400,
    });

    // the bulk, again, so the loops run past the one result that is worth looking at
    kernel.push(ContextItem::file("big.txt", "x".repeat(2_600)).pinned());
    // 2,000 bytes: 700 tokens on the corrected scale, and 626 of them back after the marker
    // that takes its place - a pass worth running whatever the scale is read at
    let asked = call("c1", "shell", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![asked.clone()],
    ));
    let shell = kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "shell",
        "y".repeat(2_000),
        false,
    ));
    kernel.push(ContextItem::assistant("read it", vec![]));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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
        .expect("a result that pays for its own marker is still taken");
    assert_eq!(
        plan.elide,
        vec![shell],
        "so the pass is not a blanket refusal"
    );

    let report = kernel.apply_compaction(plan);
    assert!(
        report.tokens_after < report.tokens_before,
        "and the request really did get smaller: {} -> {}",
        report.tokens_before,
        report.tokens_after
    );
}

/// The summary a pass leaves counts only the exchanges a compactor dropped, not one the person took
/// out by hand.
///
/// note: a message excluded by hand is out of the request too, and counting it would tell the
/// model more had been dropped to make room than any pass had dropped.
#[tokio::test]
async fn a_summary_counts_only_what_this_compactor_dropped() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let by_hand = exchange(kernel, 0, &"an old question. ".repeat(100), "ok");
    exchange(kernel, 1, &"another old question. ".repeat(100), "ok");
    exchange(kernel, 2, &"the newest question. ".repeat(100), "ok");
    kernel.set_state(by_hand, ContextState::Excluded, Some("not this".to_owned()));

    let trim = Shedder {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };
    let plan = trim
        .plan(&kernel.items(), &budget)
        .await
        .expect("over the threshold");
    let said = plan
        .summary
        .as_ref()
        .map(|summary| summary.content.to_text().into_owned());
    assert!(
        said.as_deref()
            .is_some_and(|said| said.starts_with("The 1 earliest exchange(s)")),
        "the person took one out themselves, and this pass is not what took it: {said:?}"
    );
}

/// A blob goes as soon as it has been shown, ahead of results older than it, and the arithmetic
/// gets no say. Every counter in this workspace puts a `Content::Blob` at `0` tokens, so the two
/// rules the rest of this pass runs on - oldest first, and nothing smaller than its own marker -
/// between them made the largest thing in the context the one thing the compactor could never
/// take: ranked last by age, then skipped for recovering nothing.
#[tokio::test]
async fn blobs_are_taken_before_anything_else() {
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
    // and the model has seen it: a picture it has not is kept for its first showing
    kernel.push(ContextItem::assistant(
        Content::text("a login screen"),
        vec![],
    ));

    assert_eq!(
        kernel.item(blob).unwrap().tokens,
        0,
        "the trap this is about: the counter abstains, so every size test here reads a \
         600 KB picture as free"
    );

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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
    assert!(
        plan.elide.contains(&blob) && plan.elide.contains(&text),
        "the blob goes with the text, though it is the newest item and the one counted at \
         nothing: {:?}",
        plan.elide
    );
    assert!(
        plan.reason.starts_with("compacted after you had read it"),
        "and the marker says it had been shown, which is what lets it go: {}",
        plan.reason
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

/// A picture the model has not been shown yet is kept for its first showing, where one it has seen
/// would go first; the older text goes in its place.
///
/// note: the rule the fresh text results were already under. A screenshot elided on its way in is
/// a picture the model asked for and never saw, and it asks for it again - so it goes on the next
/// pass, once it has been read.
#[tokio::test]
async fn a_picture_not_yet_shown_is_kept_for_its_first_showing() {
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

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

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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
        vec![text],
        "the picture is the model's to see first"
    );

    // and once the model has answered with it in front of it, it is the first thing to go
    kernel.apply_compaction(plan);
    kernel.push(ContextItem::assistant(
        Content::text("a login screen"),
        vec![],
    ));
    let later = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("a picture that has been seen is taken");
    assert_eq!(later.elide, vec![blob]);
}

/// A result whose call is no longer sent is not named, so the summary counts only what goes.
///
/// note: the projector leaves such a result out and the kernel will not elide what is not sent,
/// so naming it moved nothing - and the summary, counted from the plan, told the model one more
/// result had been elided than had.
#[tokio::test]
async fn a_result_whose_call_is_not_sent_is_not_counted_as_elided() {
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    let mut results = Vec::new();
    for n in 0..2 {
        let read = call(&format!("c{n}"), "read", json!({"path": "big.rs"}));
        let turn = kernel.push(ContextItem::assistant(
            Content::text(""),
            vec![read.clone()],
        ));
        let result = kernel.push(ContextItem::tool_result(
            read.id.clone(),
            "read",
            "x".repeat(4_000),
            false,
        ));
        results.push((turn, result));
    }
    kernel.push(ContextItem::assistant(Content::text("read both"), vec![]));
    // the first turn excluded, which takes its result out of the request with it
    kernel.set_state([results[0].0], ContextState::Excluded, None);

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
        threshold: 0.1,
        target: 0.05,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("the second result is over the threshold");
    assert_eq!(plan.elide, vec![results[1].1], "only what is sent");
    assert!(
        plan.summary.is_none(),
        "and each marker says what it is, so there is no summary to miscount"
    );
    let report = kernel.apply_compaction(plan);
    assert_eq!(report.elided.len(), 1);
}

/// The same, with the budget already where the pass wants it. A blob is not taken because taking
/// it helps the arithmetic - the arithmetic cannot see it - but because nothing here can say what
/// it costs, and an unbounded payload nobody can measure is the wrong thing to be carrying on a
/// guess.
#[tokio::test]
async fn a_blob_goes_even_when_the_count_says_there_is_room() {
    use nachalnik::{Budget, Compactor, Content};

    let mut harness = Harness::new([]);
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
    // and the model has seen it: a picture it has not is kept for its first showing
    kernel.push(ContextItem::assistant(
        Content::text("a login screen"),
        vec![],
    ));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder {
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

    // and once it is a marker there is nothing unpriced left, and nothing more to plan - an
    // abstention that outlived the content would make a plan before every request for the rest
    // of the session
    kernel.apply_compaction(plan);
    assert_eq!(budget().uncounted, 0);
    assert!(trim.plan(&kernel.items(), &budget()).await.is_none());

    // and the line announcing the pass says what its two totals could not price, since without
    // it they read as a request that shrank by the difference
    harness.drain();
    let screen = harness.flat();
    assert!(
        screen.contains("1 → 0 piece(s) unpriced"),
        "the announcement reads as though everything was priced: {screen}"
    );
}

/// A picture the pass may not touch does not turn into a plan. `should_compact` now answers yes
/// to anything unpriced, and a pinned one stays unpriced forever - so the pass is asked before
/// every request for the rest of the session, and each of those asks has to come back empty. A
/// plan is not nothing: it is a summary in the context and an undo spent.
#[tokio::test]
async fn an_unpriced_item_the_pass_may_not_take_produces_no_plan() {
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // a user's own attachment: not a tool result, so not a candidate at all, and this pass has
    // no business eliding what the person themselves put there
    kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "A".repeat(600_000),
    )));

    let trim = Shedder {
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

/// A share of the limit that rounds to nothing is said as under one percent, not as `0%`.
///
/// note: found live, with `--compact 0.01` against a million tokens: the marker the model read in
/// place of what it had been shown said the context had reached 0% of the limit, which reads as a
/// pass that ran on an empty context.
#[tokio::test]
async fn a_share_that_rounds_to_nothing_is_said_as_under_one_percent() {
    use nachalnik::{Budget, Compactor, Content, ToolCall};

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
    kernel.push(ContextItem::tool_result(
        call.id,
        "read",
        "x".repeat(8_000),
        false,
    ));
    kernel.push(ContextItem::assistant("it is all x", Vec::new()));

    // and the person has asked something since, so the turn that read it is over: a pass leaves
    // the turn in progress alone
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder::under(0.001);
    let budget = Budget {
        limit: Some(1_000_000),
        ..kernel.budget()
    };
    assert!(budget.context_tokens > 0 && budget.context_tokens < 5_000);
    let plan = trim
        .plan(&kernel.items(), &budget)
        .await
        .expect("the result is over the threshold and has been read");
    assert!(
        plan.reason
            .contains("the context had reached under 1% of the 1000000-token limit"),
        "{}",
        plan.reason
    );
}

/// The pass leaves one standing summary, not one per pass.
///
/// note: found live. Against a real endpoint at a 6,000-token limit, a tool loop left **twenty-one
/// identical summaries of 67 tokens each** - 1,407 tokens, a quarter of the budget, all of it the
/// same sentence, in a context the compactor had been called on to make room in. Every pass wrote
/// one and nothing ever took one back out: a summary is a `Reference`, and the pass then only ever
/// considered a tool result.
#[tokio::test]
async fn compaction_summaries_do_not_pile_up() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let trim = Shedder {
        threshold: 0.8,
        target: 0.5,
    };
    let budget = || Budget {
        limit: Some(1_000),
        ..kernel.budget()
    };

    // five exchanges of words, each of which takes the context past the threshold once the second
    // is in, so every pass after that drops the oldest left
    let mut passes = 0;
    for n in 0..5 {
        exchange(kernel, n, &"a question of some length. ".repeat(60), "ok");
        if let Some(plan) = trim.plan(&kernel.items(), &budget()).await {
            passes += plan.summary.is_some() as usize;
            kernel.apply_compaction(plan);
        }
    }
    assert!(passes >= 3, "only {passes} passes dropped anything");

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
    let dropped = items
        .iter()
        .filter(|item| {
            item.kind == nachalnik::ContextKind::UserMessage && item.state == ContextState::Excluded
        })
        .count();
    assert!(
        said.starts_with(&format!("The {dropped} earliest exchange(s)")),
        "the standing sentence should count all {dropped} of them: {said}"
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
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.8,
        target: 0.5,
    })));
    harness.app.compact_target = Some(0.5);
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

/// Under the limit, nothing promises a pass that is not coming.
///
/// note: a sentence naming a compaction that will not happen is worse than a missing one. A
/// context at two per cent of the limit reads as one about to be compacted, which sends somebody
/// to `/compact` - or to pin what the pass would have taken, for a pass that never fires.
#[tokio::test]
async fn the_budget_promises_no_compaction_the_context_does_not_need() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.8,
        target: 0.5,
    })));
    harness
        .app
        .kernel
        .push(ContextItem::user("a short question"));
    harness.drain();

    harness.send("/budget").await;
    let panel = harness.flat();
    assert!(
        !panel.contains("the compactor runs before the next request is sent"),
        "a context at a few per cent of the limit is not about to be compacted: {panel}"
    );
}

/// Everything the conversation has said, a line an entry.
fn said(harness: &Harness) -> String {
    harness
        .app
        .loose
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A request refused for its length says what to take out, and where compaction could not free
/// enough it says so and names the model's own turns, which is what fills the context then.
///
/// note: the sentence pointed at `/compact` whatever the context held, and the compactor takes
/// tool results and nothing else - so a session that had mostly talked was pointed at a command
/// that would find nothing, and a headless run ended on the last turn having failed.
#[tokio::test]
async fn a_request_too_long_says_what_compaction_cannot_take() {
    use nachalnik::Content;

    let mut talked = Harness::new([]);
    talked.app.kernel.set_compactor(None);
    // the projection a session talking to the OpenAI dialect gets, which does not send a turn's
    // thinking back - so what an item holds and what the request carries come apart
    talked
        .app
        .kernel
        .set_projector(Arc::new(nachalnik::LinearProjector {
            send_reasoning: false,
            ..nachalnik::LinearProjector::default()
        }));
    let limit = talked.app.kernel.budget().limit.expect("a limit");
    for _ in 0..4 {
        // with a great deal of thinking behind each, which the item counts and the request
        // does not: a live run said the turns were five times the request they were part of
        talked.app.kernel.push(
            ContextItem::assistant(
                Content::text("a long answer, and another sentence of it. ".repeat(limit / 20)),
                vec![],
            )
            .with_reasoning(Some(Content::text("thinking it over. ".repeat(limit)))),
        );
    }
    talked.send("and then?").await;
    talked.settle().await;
    let told = said(&talked);
    assert!(
        told.contains("tokens have to go before it is sent"),
        "{told}"
    );
    assert!(told.contains("the model's own turns"), "{told}");
    assert!(
        !told.contains("`/compact` says"),
        "it points at what would find nothing: {told}"
    );
    let figure = |before: &str, after: &str| -> usize {
        let at = told.find(after).expect("the figure is there");
        let start = told[..at].rfind(before).expect("its start") + before.len();
        told[start..at]
            .trim()
            .replace(',', "")
            .parse()
            .expect("a number")
    };
    let turns = figure("~", " of what goes out is the model's own turns");
    let request = figure(
        "Nothing has read that request:",
        " is this counter's own estimate",
    );
    assert!(
        turns <= request,
        "the turns are {turns} of a {request}-token request: {told}"
    );

    // and where a tool result is what fills it, `/compact` is the way out still
    let mut searched = Harness::new([]);
    searched.app.kernel.set_compactor(None);
    let read = call("c1", "read", json!({ "path": "big.txt" }));
    searched.app.kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![read.clone()],
    ));
    searched.app.kernel.push(ContextItem::tool_result(
        read.id.clone(),
        "read",
        "a line of routine diagnostic output. ".repeat(limit / 4),
        false,
    ));
    searched.send("and then?").await;
    searched.settle().await;
    let told = said(&searched);
    assert!(told.contains("`/compact` says"), "{told}");
    assert!(!told.contains("the model's own turns"), "{told}");
}

/// A refusal for length names what is holding the room, and does not offer a remedy worth less
/// than the overrun.
///
/// note: found live. A pinned attachment no pass may take, and two short messages after it, came
/// back with "compaction can free ~0 at most ... and ~38 of what goes out is the model's own
/// turns. `/exclude` the oldest of those" - advice worth 38 tokens against an overrun of 3,526,
/// naming neither the attachment nor the command that unpins it.
#[tokio::test]
async fn a_request_too_long_names_what_is_pinned() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(None);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    // what `-f` puts in, and pinned: a promise no pass may break
    let attachment = harness.app.kernel.push(
        ContextItem::file("big.txt", "a line of routine output. ".repeat(limit / 2)).pinned(),
    );
    for _ in 0..2 {
        harness.app.kernel.push(ContextItem::user("and then what?"));
    }
    harness.send("and then?").await;
    harness.settle().await;

    let told = said(&harness);
    assert!(
        told.contains("tokens have to go before it is sent"),
        "{told}"
    );
    assert!(
        told.contains(&format!("[{attachment}] big.txt")),
        "it names the attachment and its number: {told}"
    );
    assert!(
        told.contains("`/restore N` unpins one"),
        "and says how to unpin it: {told}"
    );
    // a pinned item is in the request already, so `/restore` does not put it back there
    assert!(!told.contains("puts one back in the request"), "{told}");
    assert!(
        !told.contains("`/exclude` the oldest of those"),
        "38 tokens of the model's turns cannot cover an overrun of thousands: {told}"
    );
}

/// A request the tool definitions alone put over the limit says so and names `/tools toggle`,
/// rather than asking for exclusions that cannot cover it.
///
/// note: found against an endpoint whose model takes a thousand tokens. The request was four
/// thousand, nearly all of it the tools, and the sentence said to `/exclude` by number until the
/// corner was under the limit - which no exclusion could ever make it.
#[tokio::test]
async fn a_request_the_tools_put_over_the_limit_names_them() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(None);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.add_tool(Arc::new(
        nachalnik::test::ConstTool::new("vast", "done").with_schema(json!({
            "type": "object",
            "description": "a tool described at length. ".repeat(limit / 4),
        })),
    ));
    harness.send("hello").await;
    harness.settle().await;

    let told = said(&harness);
    assert!(
        told.contains("tokens have to go before it is sent"),
        "{told}"
    );
    assert!(told.contains("the tool definitions to"), "{told}");
    assert!(told.contains("`/tools toggle ID`"), "{told}");
    assert!(!told.contains("`/exclude` by number"), "{told}");
}

/// And the line saying the context is full, which comes first, does not ask for the exclusions
/// the next line says cannot cover it.
///
/// note: found against an endpoint whose model takes a thousand tokens, with the compactor in
/// place: `/exclude` what is no longer needed, then that no exclusion covers it.
#[tokio::test]
async fn a_context_the_tools_fill_is_not_told_to_exclude() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.5,
        target: 0.3,
    })));
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.add_tool(Arc::new(
        nachalnik::test::ConstTool::new("vast", "done").with_schema(json!({
            "type": "object",
            "description": "a tool described at length. ".repeat(limit / 4),
        })),
    ));
    harness.send("hello").await;
    harness.settle().await;

    let told = said(&harness);
    assert!(told.contains("the context is full"), "{told}");
    assert!(
        !told.contains("`/exclude` what is no longer needed"),
        "{told}"
    );
    assert!(
        told.contains("the tool definitions alone come to"),
        "{told}"
    );
}

/// `/compact` in a context that has not reached the compactor's own target says the context is
/// under it, rather than that nothing in it is eligible.
///
/// note: a pass is only asked for above the threshold, and above the threshold there is always a
/// target below the total - so a context already under the target is the one case where "no
/// eligible item" and "nothing to do" are the same plan, and the first sentence claimed the
/// first.
#[tokio::test]
async fn compact_under_the_target_says_the_context_is_under_it() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.5,
        target: 0.3,
    })));
    harness.app.compact_target = Some(0.3);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(ContextItem::user(
        "a line of routine output. ".repeat(limit / 50),
    ));
    harness.drain();

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;

    let screen = harness.flat();
    assert!(
        screen.contains("has nothing to do") && screen.contains("under the"),
        "{screen}"
    );
    // and it is named as somebody reads it, not by its module path in the middle of a sentence
    assert!(screen.contains("Shedder has nothing to do"), "{screen}");
    assert!(!screen.contains("tools::shedder"), "{screen}");
    assert!(
        !screen.contains("found nothing it may take"),
        "nothing is ineligible here; the context is simply not full enough: {screen}"
    );
}

/// `/compact` above the target, where a pass really does find nothing eligible, still says so.
#[tokio::test]
async fn compact_above_the_target_says_nothing_is_eligible() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.5,
        target: 0.3,
    })));
    harness.app.compact_target = Some(0.3);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness
        .app
        .kernel
        .push(ContextItem::file("big.txt", "a line of routine output. ".repeat(limit)).pinned());
    harness.drain();

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;

    let screen = harness.flat();
    assert!(
        screen.contains("found nothing it may take"),
        "the context is over the target and only a pin stands in the way: {screen}"
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

// ------------------------------------------------------------------------------- asking for one

/// A context with one compactable result in it, and a compactor that will take it.
async fn ready_to_compact() -> (Harness, nachalnik::ContextId) {
    use nachalnik::{Content, ToolCall};

    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.0,
        target: 0.0,
    })));
    harness.app.compact_target = Some(0.0);

    let call = ToolCall::new("call-1", "read", Arc::new(json!({"path": "big.rs"})));
    harness
        .app
        .kernel
        .push(ContextItem::user("what is in big.rs?"));
    harness.app.kernel.push(ContextItem::assistant(
        Content::text("let me look"),
        vec![call.clone()],
    ));
    let result = harness.app.kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        Content::text("x".repeat(40_000)),
        false,
    ));
    // read, so the pass may take it: one the model has not been shown is kept
    harness.app.kernel.push(ContextItem::assistant(
        Content::text("it is forty thousand x"),
        vec![],
    ));
    // and asked something since, so that turn is over: a pass leaves the turn in progress alone
    harness.app.kernel.push(ContextItem::user("and now?"));
    harness.drain();

    (harness, result)
}

/// `/compact` says what a pass would take, takes none of it, and waits for an answer.
///
/// note: the whole point of the command, and the half an automatic pass cannot have. A pass that
/// announces itself afterwards leaves somebody reading what they have lost; this is the same
/// information one step earlier, with the identifiers to pin from, while it is still a decision.
#[tokio::test]
async fn compact_lists_what_would_go_and_waits() {
    let (mut harness, result) = ready_to_compact().await;

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;

    let packed = harness.packed();
    assert!(packed.contains(&format!("[{result}]")), "{packed}");
    // and the row is that item's own: the label and the kind it has in the context, not the
    // first thing at the other end of the list. The identifier is what somebody pins against,
    // so a row naming something else beside it is a pin against the wrong thing
    assert!(
        packed.contains("elide,leavingamarkerinitsplace·read·tool_result·"),
        "the row describes the item the identifier names: {packed}"
    );
    assert!(
        packed.contains("elide"),
        "and what would happen to it: {packed}"
    );
    assert!(packed.contains("[y]takeit"), "and how to answer: {packed}");
    // and the total is what the rows add up to: the tokens the pass would take out of the request,
    // which for a plan of one is that item's own. A figure read off some other item is a total
    // nobody can check the question against
    let holding = harness
        .app
        .kernel
        .item(result)
        .expect("the result is still there")
        .tokens;
    assert!(
        packed.contains(&format!("holding{}tokens", grouped(holding))),
        "the total is the items named, at what they hold: {packed}"
    );

    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Active,
        "and nothing has happened"
    );

    // the question has the prompt's place rather than standing above it. Stacked, the two
    // disagree about the room on any short window: the question needs it, so the prompt gives way
    // and goes on holding the keys from off the screen
    assert!(
        !harness.flat().contains("ask for something"),
        "the prompt is not on the screen beside it: {}",
        harness.flat()
    );
    assert!(!harness.app.prompted());
}

/// The question is pinned rather than modal, so the thing it is about stays reachable: the
/// context tab, and `p` on the row somebody wants kept.
///
/// note: this is what the question being in the prompt's place buys, and the reason `y` works the
/// pass out again instead of applying the list it showed. Applying the old plan would take
/// exactly what was just protected - caught by the kernel, which refuses a pinned item, but
/// caught afterwards and reported, which is the shape the question exists to get away from.
#[tokio::test]
async fn a_pin_made_while_the_question_waits_is_honoured() {
    let (mut harness, result) = ready_to_compact().await;

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;
    // over to the list, onto the row the question named, and keep it - all of it while the
    // question waits, which is what a question in the prompt's place is for
    harness.alt(KeyCode::Char('2')).await;
    harness
        .press(KeyCode::Char(
            char::from_digit(result.0 as u32, 10).unwrap(),
        ))
        .await;
    harness.press(KeyCode::Char('G')).await;
    harness.press(KeyCode::Char('p')).await;
    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Pinned,
        "the context tab still takes keys with a question open"
    );

    // and back to answer the question that was waiting the whole time
    harness.alt(KeyCode::Char('1')).await;
    harness.press(KeyCode::Tab).await;
    harness.press(KeyCode::Char('y')).await;
    harness.settle().await;
    harness.drain();

    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Pinned,
        "the pin stands"
    );
    let screen = harness.flat();
    assert!(
        screen.contains("nothing left to take"),
        "and the pass says so rather than reporting a refusal: {screen}"
    );
}

/// `y` takes it, reported by the same event any other pass is reported by.
#[tokio::test]
async fn y_takes_it_and_says_what_it_took() {
    let (mut harness, result) = ready_to_compact().await;

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;
    harness.press(KeyCode::Tab).await;
    harness.press(KeyCode::Char('y')).await;
    harness.settle().await;
    harness.drain();

    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Elided
    );
    let screen = harness.flat();
    assert!(screen.contains("compacted:"), "{screen}");
}

/// `n` leaves it, and says so rather than going quiet.
#[tokio::test]
async fn n_leaves_it_alone() {
    let (mut harness, result) = ready_to_compact().await;

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;
    harness.press(KeyCode::Tab).await;
    harness.press(KeyCode::Char('n')).await;

    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Active
    );
    let screen = harness.flat();
    assert!(screen.contains("left alone"), "{screen}");
    assert!(
        !screen.contains("[y]takeit"),
        "and the question is gone: {screen}"
    );
}

/// Which row of the compaction's list is the first one on the screen.
///
/// note: the rows are numbered `[1]`, `[2]` and so on, so the number on the top row is the row
/// somebody is reading. Read from the panel rather than from the argument list, which echoes the
/// first of them whatever the list is scrolled to.
fn first_row_shown(harness: &mut Harness) -> Option<u32> {
    harness.screen().lines().find_map(|row| {
        let at = row.find('[')?;
        let digits: String = row[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// The list of a compaction that has more items in it than the panel has rows scrolls with all
/// four keys, and one row at a time is one row.
///
/// note: the panel is drawn in the prompt's place rather than over the conversation, so there are
/// only a handful of rows to scroll - a list of a hundred is asked about through them one press
/// at a time, and only `pgup` and `pgdn` used to move. `up` and `down` are the keys everything
/// else scrolls with, and a list nobody can read at its own pace is a decision somebody makes
/// having read a seventh of it.
#[tokio::test]
async fn the_list_of_what_a_compaction_would_take_scrolls_a_row_at_a_time() {
    use nachalnik::{Content, ToolCall};

    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.0,
        target: 0.0,
    })));
    harness.app.compact_target = Some(0.0);
    // forty exchanges is a hundred and twenty rows, which is more than any window this program
    // draws in, so the list is longer than the panel whichever way it is sized
    for round in 0..40 {
        let call = ToolCall::new(
            format!("call-{round}"),
            "read",
            Arc::new(json!({ "path": format!("f{round}.rs") })),
        );
        harness
            .app
            .kernel
            .push(ContextItem::user(format!("what is in f{round}.rs?")));
        harness.app.kernel.push(ContextItem::assistant(
            Content::text("let me look"),
            vec![call.clone()],
        ));
        harness.app.kernel.push(ContextItem::tool_result(
            call.id.clone(),
            "read",
            Content::text("x".repeat(4000)),
            false,
        ));
    }
    // and a turn of the model's own, so the last exchange is over and a pass may reach it
    harness.app.kernel.push(ContextItem::user("and now?"));
    harness.app.kernel.push(ContextItem::assistant(
        Content::text("that is all of them"),
        vec![],
    ));
    harness.drain();

    harness.send("/compact").await;
    // the pass is worked out off the loop, and the question asked when it comes back
    harness.settle().await;
    harness.press(KeyCode::Tab).await;

    assert_eq!(
        first_row_shown(&mut harness),
        Some(1),
        "the list starts at the top"
    );

    // `down` is one row, not a page: the point of it is to read the row that is not on the screen
    harness.press(KeyCode::Down).await;
    assert_eq!(first_row_shown(&mut harness), Some(2));
    // and `up` puts it back rather than carrying on down, or leaving it where it was
    harness.press(KeyCode::Up).await;
    assert_eq!(first_row_shown(&mut harness), Some(1));

    // at the top there is nowhere above the top to go
    harness.press(KeyCode::Up).await;
    assert_eq!(first_row_shown(&mut harness), Some(1));

    // a page is still a page
    // a page is more than a row, whatever its size, and `up` and `pgup` come back from it
    harness.press(KeyCode::PageDown).await;
    let paged = first_row_shown(&mut harness).expect("a row is shown");
    assert!(paged > 2, "pgdn moved {paged}");
    harness.press(KeyCode::Up).await;
    assert_eq!(first_row_shown(&mut harness), Some(paged - 1));
    harness.press(KeyCode::PageUp).await;
    assert_eq!(first_row_shown(&mut harness), Some(1));
}

/// `/compact` asked for while a turn is running is refused, and says why.
///
/// note: the guard is one question at a time, and a turn under way is the other half of it - so
/// this is `/compact` typed while the model is working. Refused rather than queued: a queued pass
/// would work its plan out against a context that is still being added to, which is the one thing
/// the note on the command says a list must not be read against.
#[tokio::test]
async fn compact_during_a_running_turn_is_refused() {
    let (mut harness, result) = ready_to_compact().await;

    // a turn under way, and nothing else waiting to be answered
    harness.app.busy = true;
    assert!(!harness.app.asking(), "the case this is about");
    harness.send("/compact").await;

    let screen = harness.flat();
    assert!(
        screen.contains("already waiting for an answer"),
        "a pass worked out beside a running turn would list a context still being added to: \
         {screen}"
    );
    // and nothing is proposed, so there is no question on the screen either
    assert!(harness.app.proposed.is_none(), "{screen}");
    assert_eq!(
        harness.app.kernel.item(result).unwrap().state,
        ContextState::Active,
        "and nothing was taken"
    );
}

/// With no compactor installed there is nothing to ask, and it says so rather than saying
/// nothing.
#[tokio::test]
async fn compact_without_a_compactor_says_there_is_none() {
    let (mut harness, _) = ready_to_compact().await;
    harness.app.kernel.set_compactor(None);

    harness.send("/compact").await;

    let screen = harness.flat();
    assert!(screen.contains("no compactor is installed"), "{screen}");
}

/// A compactor aims below the fullness it starts at, whatever fullness that is.
///
/// note: the two fields do not say they are ordered, and for two years one spelling of the pair
/// put them the wrong way round below a third. `--compact 0.15` meant "start at fifteen percent
/// of the limit" and aimed at ten, so a context between the two was already under the target: the
/// pass fired before every request, found nothing it could take that would help, said so, and
/// left the context exactly where it was. Nothing was broken and nothing happened, which is the
/// worst way for a setting to be wrong.
#[test]
fn a_compactor_aims_lower_than_the_point_it_starts_at() {
    for threshold in [0.99, 0.8, 0.5, 0.3, 0.2, 0.15, 0.05, 0.01] {
        let trim = Shedder::under(threshold);
        assert!(
            trim.target < trim.threshold,
            "aims at {} from {threshold}, which is not down",
            trim.target,
        );
        assert!(
            trim.target > 0.0,
            "aims at {} from {threshold}, which is at nothing left",
            trim.target,
        );
    }

    // and where there is room for it, twenty points is still twenty points: the default pass
    // starts at four fifths of the limit and takes the context to three fifths of it
    let default = Shedder::under(0.8);
    assert!(
        (default.target - 0.6).abs() < 0.001,
        "the default aims at {}",
        default.target,
    );
}

/// A result the model has not been shown yet is kept, whether or not the request fits the limit.
///
/// note: found in a live session with a 12,000-token limit. Pass after pass at 84 to 90% of it
/// elided the `grep` the model had just run - before the request that would have been the first
/// to carry it - so the model read a marker and ran the search again, for a result that fitted.
/// And where it did not fit, the pass that took it anyway left the model a marker for a file it
/// had asked for and never seen.
#[tokio::test]
async fn a_result_not_yet_shown_is_kept_whether_or_not_it_fits() {
    use nachalnik::{Budget, Compactor, Content};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // read, and answered, in an exchange that is over
    let seen = exchange(kernel, 0, "what is in old.rs?", &"x".repeat(4_000))[2];
    // read, and not yet answered: the request about to go is the first to carry it
    kernel.push(ContextItem::user("and new.rs?"));
    let new = call("c2", "read", json!({"path": "new.rs"}));
    kernel.push(ContextItem::assistant(Content::text(""), vec![new.clone()]));
    let fresh = kernel.push(ContextItem::tool_result(
        new.id.clone(),
        "read",
        "y".repeat(4_000),
        false,
    ));

    let trim = Shedder {
        threshold: 0.5,
        target: 0.3,
    };
    let within = |limit| Budget {
        limit: Some(limit),
        ..kernel.budget()
    };

    // over the threshold and over the target with both, and inside the limit without the old one
    let plan = trim
        .plan(&kernel.items(), &within(3_000))
        .await
        .expect("over the threshold");
    assert_eq!(plan.elide, vec![seen], "the one not yet read stays");

    // and where the request cannot fit the limit even without the old one, it still stays
    let plan = trim
        .plan(&kernel.items(), &within(1_000))
        .await
        .expect("over the threshold");
    assert_eq!(plan.elide, vec![seen]);
    assert!(!plan.elide.contains(&fresh));
}

/// A context over the compactor's threshold that holds nothing it may take is said to be full,
/// once, and the person is told what can still be done.
///
/// note: `Shedder` keeps its promise and takes neither what is pinned nor the turn in progress, so
/// a context that is those is one it can do nothing about. It used to say nothing, and the session
/// went on until the kernel refused a request over the limit - which a headless run did not come
/// back from. The kernel measures it now, and this is the line it comes to.
#[tokio::test]
async fn a_context_the_compactor_cannot_help_is_said_to_be_full_once() {
    let mut harness = Harness::new([
        nachalnik::ModelResponse::text("ok"),
        nachalnik::ModelResponse::text("ok again"),
    ]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.5,
        target: 0.3,
    })));
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    // pinned, because anything else in an exchange before the one in progress is the first thing
    // a full context drops
    harness.app.kernel.push(
        ContextItem::file("notes.txt", "a line of routine output. ".repeat(limit / 10)).pinned(),
    );
    let used = harness
        .app
        .kernel
        .budget()
        .fraction_used()
        .expect("a limit");
    assert!((0.5..1.0).contains(&used), "the setup is off: {used}");

    let said = "the context is full, and the compactor has nothing more it may take";
    harness.send("go on").await;
    harness.settle().await;
    assert!(harness.flat().contains(said), "{}", harness.screen());

    // still full, and not said a second time: a screen tall enough to hold both turns and both
    // lines, had there been two
    harness.send("and again").await;
    harness.settle().await;
    let screen = harness.sized(400, 120);
    assert!(
        screen.contains("ok again"),
        "the second turn is not on it: {screen}"
    );
    assert_eq!(screen.matches(said).count(), 1, "{screen}");
}

/// And the other half of that sentence: a context that stops being full says so too, so a person
/// who excluded what was holding the room knows it worked.
///
/// note: the same event with the other value, and it is a change rather than a state - the kernel
/// says it once, on the change. What it cost to leave the second half out is that the line which
/// tells somebody to `/exclude` was the last word on a subject for the rest of the session, and
/// the thing they did about it was never acknowledged.
#[tokio::test]
async fn a_context_that_stops_being_full_says_so() {
    let mut harness = Harness::new([
        nachalnik::ModelResponse::text("ok"),
        nachalnik::ModelResponse::text("ok again"),
    ]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.5,
        target: 0.3,
    })));
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(
        ContextItem::file("notes.txt", "a line of routine output. ".repeat(limit / 10)).pinned(),
    );

    let full = "the context is full, and the compactor has nothing more it may take";
    harness.send("go on").await;
    harness.settle().await;
    assert!(harness.flat().contains(full), "{}", harness.screen());

    // what the sentence asks for, done by hand
    harness.send("/exclude all").await;
    harness.send("and again").await;
    harness.settle().await;

    let screen = harness.sized(400, 120);
    assert!(
        screen.contains("ok again"),
        "the second turn is not on it: {screen}"
    );
    assert!(
        screen.contains("the context has room again"),
        "nothing said the excluding worked: {screen}"
    );
    assert_eq!(
        screen.matches("the context has room again").count(),
        1,
        "it said it once: {screen}"
    );
}

// ------------------------------------------------------------------------- what is done with

/// One exchange: the person's message, the model's call, its result and the model's answer.
fn exchange(
    kernel: &nachalnik::Kernel,
    n: usize,
    asked: &str,
    output: &str,
) -> Vec<nachalnik::ContextId> {
    use nachalnik::Content;

    let read = call(
        &format!("c{n}"),
        "read",
        json!({"path": format!("f{n}.rs")}),
    );
    vec![
        kernel.push(ContextItem::user(asked.to_owned())),
        kernel.push(ContextItem::assistant(
            Content::text("let me look"),
            vec![read.clone()],
        )),
        kernel.push(ContextItem::tool_result(
            read.id.clone(),
            "read",
            output.to_owned(),
            false,
        )),
        kernel.push(ContextItem::assistant(Content::text("it is all x"), vec![])),
    ]
}

/// A result goes to a marker once the turn that asked for it is over, however empty the context
/// is, and while that turn lasts it stays.
///
/// note: the first of `Shedder`'s two rules, and the one that makes it a compactor for a long
/// session rather than for a full one. What a tool said is the bulk of most contexts, and once the
/// model has answered the person with it in front of it, the answer is what the conversation
/// carries forward. Kept, it is paid for on every request after, until the context fills and
/// something has to go anyway.
#[tokio::test]
async fn a_result_goes_once_its_turn_is_over() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let trim = Shedder::under(0.8);
    // a limit nothing here comes near, so that nothing about this is about room
    let budget = || Budget {
        limit: Some(1_000_000),
        ..kernel.budget()
    };

    let first = exchange(kernel, 1, "what is in f1.rs?", &"x".repeat(4_000));
    assert!(
        trim.plan(&kernel.items(), &budget()).await.is_none(),
        "the turn is not over, so the result is the model's still"
    );

    kernel.push(ContextItem::user("and what does it mean?"));
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("the turn that asked for it is over");
    assert_eq!(
        plan.elide,
        vec![first[2]],
        "the result, and only the result"
    );
    assert!(plan.remove.is_empty(), "nothing of the conversation itself");
    assert!(
        plan.summary.is_none(),
        "and no summary, since each marker says what happened in its own place"
    );
    assert!(
        plan.reason
            .starts_with("compacted after you had read it, to keep the context short"),
        "{}",
        plan.reason
    );

    kernel.apply_compaction(plan);
    assert_eq!(kernel.item(first[2]).unwrap().state, ContextState::Elided);
    assert!(
        trim.plan(&kernel.items(), &budget()).await.is_none(),
        "and once it is a marker there is nothing more to do"
    );
}

/// A result the person brings back is left alone, where it used to go again before the next
/// request.
///
/// note: a restore cleared the note, and an item with no note and a turn behind it is exactly what
/// the first rule takes - so `/restore` on an elided result undid itself the moment anything was
/// sent. A restore leaves a note now, and an item somebody has said something about is not the
/// first rule's.
#[tokio::test]
async fn a_result_brought_back_by_the_person_is_left_alone() {
    use nachalnik::{Budget, Compactor};

    let mut harness = Harness::new([]);
    let trim = Shedder::under(0.8);
    let kernel = harness.app.kernel.clone();
    let budget = || Budget {
        limit: Some(1_000_000),
        ..kernel.budget()
    };

    let first = exchange(&kernel, 1, "what is in f1.rs?", &"x".repeat(4_000));
    kernel.push(ContextItem::user("and what does it mean?"));
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("the turn is over");
    kernel.apply_compaction(plan);

    harness.send(&format!("/restore {}", first[2])).await;
    assert_eq!(kernel.item(first[2]).unwrap().state, ContextState::Active);
    assert!(
        trim.plan(&kernel.items(), &budget()).await.is_none(),
        "brought back by hand, and kept"
    );
}

/// A full context drops its oldest exchanges whole, down to the target, and never the one in
/// progress.
///
/// note: the second rule, and the reason a long session does not end at the limit. Half an
/// exchange is a conversation nobody had - a question with no answer, or a result whose call has
/// gone - so it goes whole, and removed rather than elided: a marker for each of its messages
/// would be the exchange's length in lines saying there was something there.
#[tokio::test]
async fn a_full_context_drops_its_oldest_exchanges_whole() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    // the person's own words are the bulk, so that the first rule has nothing worth taking and
    // what this measures is the second
    let words = "a long question, and then another sentence of it. ".repeat(80);
    let oldest = exchange(kernel, 1, &words, "small");
    // a note the model wrote for itself in the oldest exchange, which is the one thing in it
    // written to outlast it
    let note = kernel.push(ContextItem::new(
        nachalnik::ContextKind::Reference,
        "agent",
        "remember",
        "the build is in ./out",
    ));
    // and a file attached at the prompt for the next question, which belongs to that one
    let attached = kernel.push(ContextItem::file("plan.md", "the plan"));
    let second = exchange(kernel, 2, &words, "small");
    let current = exchange(kernel, 3, &words, "small");

    let used = kernel.budget().used();
    let trim = Shedder {
        threshold: 0.8,
        target: 0.7,
    };
    // just over the threshold, so that one exchange is enough to get under the target
    let limit = (used as f64 / 0.82) as usize;
    let budget = || Budget {
        limit: Some(limit),
        ..kernel.budget()
    };
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("over the threshold");

    assert_eq!(
        plan.remove, oldest,
        "the oldest exchange, all of it, and nothing else"
    );
    assert!(!plan.remove.contains(&note), "the model's own note stays");
    assert!(
        !plan.remove.contains(&attached),
        "and the file goes with the question it was brought in for, not the one before it"
    );
    for id in second.iter().chain(&current) {
        assert!(!plan.remove.contains(id) && !plan.elide.contains(id));
    }
    let said = plan
        .summary
        .as_ref()
        .map(|summary| summary.content.to_text().into_owned())
        .unwrap_or_default();
    assert!(said.starts_with("The 1 earliest exchange(s)"), "{said}");

    let report = kernel.apply_compaction(plan);
    assert_eq!(report.removed.len(), oldest.len());
    assert!(
        kernel.project().repairs.is_empty(),
        "and what is left is a conversation the projector did not have to mend"
    );

    // and the next one counts both, since it replaces this one's summary
    let fourth = exchange(kernel, 4, &words, "small");
    let budget = || Budget {
        limit: Some(limit),
        ..kernel.budget()
    };
    assert!(trim.wants_room(&budget()), "the setup is off");
    let plan = trim
        .plan(&kernel.items(), &budget())
        .await
        .expect("over the threshold again");
    // the file this time, with the question it was brought in for
    let going: Vec<_> = std::iter::once(attached).chain(second).collect();
    assert!(plan.remove.starts_with(&going), "{:?}", plan.remove);
    assert!(fourth.iter().all(|id| !plan.remove.contains(id)));
    let said = plan
        .summary
        .as_ref()
        .map(|summary| summary.content.to_text().into_owned())
        .unwrap_or_default();
    assert!(said.starts_with("The 2 earliest exchange(s)"), "{said}");
}

/// Between the target and the threshold nothing is dropped, so the drops come in bursts.
///
/// note: the reason there is a target at all. Dropping just enough to be under the threshold
/// again is an exchange or so before nearly every turn once the context is full, and each of those
/// moves the start of the request, which is what a provider's prompt cache keys on.
#[tokio::test]
async fn between_the_target_and_the_threshold_nothing_is_dropped() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let words = "a long question, and then another sentence of it. ".repeat(80);
    for n in 0..3 {
        exchange(kernel, n, &words, "small");
    }

    let used = kernel.budget().used();
    let trim = Shedder {
        threshold: 0.8,
        target: 0.5,
    };
    // three quarters full: over the target, under the threshold
    let budget = Budget {
        limit: Some((used as f64 / 0.75) as usize),
        ..kernel.budget()
    };
    assert!(trim.plan(&kernel.items(), &budget).await.is_none());
}

/// A pin in an old exchange holds its pair, and the rest of the exchange goes around it.
///
/// note: the kernel refuses to exclude either half of a pinned call and its result, since one
/// without the other leaves the request, so a plan naming the call would be a refusal on the
/// screen for every pass from then on.
#[tokio::test]
async fn a_pin_in_an_old_exchange_holds_its_pair() {
    use nachalnik::{Budget, Compactor};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let words = "a long question, and then another sentence of it. ".repeat(80);
    let oldest = exchange(kernel, 1, &words, "the one thing worth keeping");
    kernel.set_state([oldest[2]], ContextState::Pinned, None);
    exchange(kernel, 2, &words, "small");

    let trim = Shedder {
        threshold: 0.5,
        target: 0.4,
    };
    let budget = Budget {
        limit: Some(kernel.budget().used()),
        ..kernel.budget()
    };
    let plan = trim
        .plan(&kernel.items(), &budget)
        .await
        .expect("over the threshold");
    assert!(plan.remove.contains(&oldest[0]), "the question goes");
    assert!(plan.remove.contains(&oldest[3]), "and the answer");
    assert!(
        !plan.remove.contains(&oldest[1]) && !plan.remove.contains(&oldest[2]),
        "the pinned result and the call it answers stay: {:?}",
        plan.remove
    );
    assert!(kernel.apply_compaction(plan).refused.is_empty());
}
