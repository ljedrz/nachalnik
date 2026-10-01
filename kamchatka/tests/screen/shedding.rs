//! `Shedder`, rule by rule and over whole sessions.
//!
//! note: `compaction.rs` is what a pass looks like from the screen and what each of its sentences
//! says; this is what the pass takes and leaves, at every edge of its two rules and its last
//! resort, and then the same rules held to over long sessions driven through the kernel's own turn
//! loop - across a restart, an undo and a context too small for the work.
//!
//! note: most of these build the context by hand and call `plan` directly, with the limit set on
//! the budget rather than on a provider, because what is under test is the decision, and a
//! decision is cheapest to pin down where nothing else moves.

use std::sync::Arc;

use kamchatka::tools::Shedder;
use nachalnik::{
    Budget, CompactionPlan, Compactor, Config, Content, ContextId, ContextItem, ContextKind,
    ContextState, Event, Kernel, ModelInfo, ModelResponse,
    test::{AllowAll, ScriptedProvider, call},
};
use serde_json::json;

use crate::harness::Harness;

// ------------------------------------------------------------------------------------- helpers

/// A `read` that answers with as many bytes as its call asks for.
///
/// note: not the runtime's `EchoTool`, which answers with its arguments - so a large answer was a
/// large call as well, and the turn in progress, which no pass may take, filled the context with
/// calls rather than results.
struct Sized {
    limit: Option<usize>,
}

#[nachalnik::async_trait]
impl nachalnik::Tool for Sized {
    fn spec(&self) -> nachalnik::ToolSpec {
        let mut spec = nachalnik::ToolSpec::new("read", "answers with as many bytes as asked");
        spec.output_limit = self.limit;
        spec
    }

    async fn invoke(
        &self,
        call: &nachalnik::ToolCall,
        _: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        let bytes = call.args["bytes"].as_u64().unwrap_or_default() as usize;

        Ok(nachalnik::ToolOutput::new(Content::text("z".repeat(bytes))))
    }
}

/// The person's words, at a length that is the bulk of a context and nothing a pass can elide.
fn words(bytes: usize) -> String {
    "a long question, and then another sentence of it. "
        .repeat(bytes / 50 + 1)
        .chars()
        .take(bytes)
        .collect()
}

/// One exchange, as the items it pushed.
struct Exchange {
    asked: ContextId,
    turns: Vec<ContextId>,
    results: Vec<ContextId>,
    answer: ContextId,
}

impl Exchange {
    /// Every item it pushed, in the order it pushed them.
    fn all(&self) -> Vec<ContextId> {
        let mut all = vec![self.asked];
        for (turn, result) in self.turns.iter().zip(&self.results) {
            all.push(*turn);
            all.push(*result);
        }
        all.push(self.answer);
        all
    }
}

/// An exchange: the person asks, the model makes one call per output, each answered in turn, and
/// then answers in words.
fn exchange(kernel: &Kernel, tag: &str, asked: &str, outputs: &[String]) -> Exchange {
    let asked = kernel.push(ContextItem::user(asked.to_owned()));
    let (mut turns, mut results) = (Vec::new(), Vec::new());
    for (n, output) in outputs.iter().enumerate() {
        let read = call(&format!("{tag}-{n}"), "read", json!({ "n": n }));
        turns.push(kernel.push(ContextItem::assistant(
            Content::text("let me look"),
            vec![read.clone()],
        )));
        results.push(kernel.push(ContextItem::tool_result(
            read.id.clone(),
            "read",
            output.clone(),
            false,
        )));
    }
    let answer = kernel.push(ContextItem::assistant(Content::text("that is all"), vec![]));

    Exchange {
        asked,
        turns,
        results,
        answer,
    }
}

/// The budget as it stands, measured against this limit.
fn against(kernel: &Kernel, limit: Option<usize>) -> Budget {
    Budget {
        limit,
        ..kernel.budget()
    }
}

/// What the pass would do now, against this limit.
async fn planned(trim: &Shedder, kernel: &Kernel, limit: Option<usize>) -> Option<CompactionPlan> {
    trim.plan(&kernel.items(), &against(kernel, limit)).await
}

/// What the plan's summary says, or nothing.
fn said(plan: &CompactionPlan) -> String {
    plan.summary
        .as_ref()
        .map(|summary| summary.content.to_text().into_owned())
        .unwrap_or_default()
}

/// A limit the context as it stands fills to this fraction of.
fn filled_to(kernel: &Kernel, fraction: f64) -> usize {
    (kernel.budget().used() as f64 / fraction) as usize
}

// --------------------------------------------------------------------- the first rule's edges

/// A result no bigger than its marker stays when its turn is over: eliding it would make the
/// request bigger, every turn, for as long as the session lasts.
#[tokio::test]
async fn a_result_smaller_than_its_marker_stays_when_its_turn_is_over() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let first = exchange(
        kernel,
        "a",
        "write it",
        &["wrote 412 bytes to f.rs".to_owned()],
    );
    kernel.push(ContextItem::user("and now?"));

    assert!(
        planned(&Shedder::under(0.8), kernel, Some(1_000_000))
            .await
            .is_none()
    );
    assert_eq!(
        kernel.item(first.results[0]).unwrap().state,
        ContextState::Active
    );
}

/// Every result of a finished turn goes in one pass, in the order they happened, and from every
/// finished turn at once.
#[tokio::test]
async fn every_result_of_every_finished_turn_goes_in_one_pass() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let big = || "x".repeat(4_000);
    let first = exchange(kernel, "a", "read three", &[big(), big(), big()]);
    let second = exchange(kernel, "b", "read one", &[big()]);
    let current = exchange(kernel, "c", "read one more", &[big()]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(1_000_000))
        .await
        .expect("two turns are over");
    let expected: Vec<_> = first
        .results
        .iter()
        .chain(&second.results)
        .copied()
        .collect();
    assert_eq!(plan.elide, expected, "the turn in progress keeps its own");
    assert!(plan.remove.is_empty());
    assert!(plan.summary.is_none());
    assert!(!plan.elide.contains(&current.results[0]));
}

/// What is pinned, excluded, already elided, or answers a call no longer sent is not the first
/// rule's - and a plan naming any of it is a refusal on the screen or a count that is wrong.
#[tokio::test]
async fn the_first_rule_takes_nothing_it_may_not_or_need_not() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let big = || "x".repeat(4_000);
    let ex = exchange(kernel, "a", "read four", &[big(), big(), big(), big()]);
    kernel.set_state([ex.results[0]], ContextState::Pinned, None);
    kernel.set_state([ex.results[1]], ContextState::Excluded, None);
    kernel.set_state([ex.results[2]], ContextState::Elided, None);
    // the turn that asked for the fourth, taken out, which takes the result with it
    kernel.set_state([ex.turns[3]], ContextState::Excluded, None);
    kernel.push(ContextItem::user("and now?"));

    assert!(
        planned(&Shedder::under(0.8), kernel, Some(1_000_000))
            .await
            .is_none()
    );
}

/// An unpinned result is ordinary again: a pin taken off with `p` leaves no note behind, and the
/// first rule takes it as it takes any other.
#[tokio::test]
async fn a_result_unpinned_is_ordinary_again() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let ex = exchange(kernel, "a", "read it", &["x".repeat(4_000)]);
    kernel.set_state([ex.results[0]], ContextState::Pinned, None);
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder::under(0.8);
    assert!(planned(&trim, kernel, Some(1_000_000)).await.is_none());

    kernel.set_state([ex.results[0]], ContextState::Active, None);
    let plan = planned(&trim, kernel, Some(1_000_000))
        .await
        .expect("an ordinary result whose turn is over");
    assert_eq!(plan.elide, vec![ex.results[0]]);
}

/// A result brought back with `space` on the context tab is left alone, as one brought back with
/// `/restore` is: the ring ends with a note saying who did it.
#[tokio::test]
async fn a_result_brought_back_on_the_tab_is_left_alone() {
    let mut harness = Harness::new([]);
    let kernel = harness.app.kernel.clone();
    let ex = exchange(&kernel, "a", "read it", &["x".repeat(4_000)]);
    kernel.push(ContextItem::user("and now?"));
    let trim = Shedder::under(0.8);
    let plan = planned(&trim, &kernel, Some(1_000_000))
        .await
        .expect("the turn is over");
    kernel.apply_compaction(plan);

    // elided, then out altogether, then back
    assert_eq!(harness.app.cycle(ex.results[0]), Ok(ContextState::Excluded));
    assert_eq!(harness.app.cycle(ex.results[0]), Ok(ContextState::Active));
    assert!(kernel.item(ex.results[0]).unwrap().note.is_some());
    assert!(planned(&trim, &kernel, Some(1_000_000)).await.is_none());

    // and the room rule still takes it with its exchange, since a pin is what keeps something
    let limit = filled_to(&kernel, 0.95);
    let plan = planned(&trim, &kernel, Some(limit))
        .await
        .expect("over the threshold");
    assert!(plan.remove.contains(&ex.results[0]), "{:?}", plan.remove);
}

/// The marker a routine pass leaves says why the item went without claiming the context was full,
/// and the pass leaves the standing summary of an earlier one where it is.
#[tokio::test]
async fn a_routine_pass_says_nothing_about_room_and_leaves_the_summary_standing() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let standing = kernel.push(ContextItem::summary("The 1 earliest exchange(s) ..."));
    exchange(kernel, "a", "read it", &["x".repeat(4_000)]);
    kernel.push(ContextItem::user("and now?"));

    let plan = planned(&Shedder::under(0.8), kernel, Some(1_000_000))
        .await
        .expect("the turn is over");
    assert!(
        plan.reason
            .starts_with("compacted after you had read it, to keep the context short"),
        "{}",
        plan.reason
    );
    assert!(!plan.reason.contains('%'), "{}", plan.reason);
    assert!(!plan.remove.contains(&standing));
}

/// A picture attached at the prompt goes once the model has been shown it, and not before; one
/// attached at startup is pinned and stays.
#[tokio::test]
async fn an_attached_picture_goes_once_it_has_been_shown() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let at_startup = kernel.push(
        ContextItem::file("logo.png", Content::blob("image/png", "B".repeat(60_000))).pinned(),
    );
    let attached = kernel.push(ContextItem::file(
        "screen.png",
        Content::blob("image/png", "A".repeat(600_000)),
    ));
    kernel.push(ContextItem::user("what is on this screen?"));
    let trim = Shedder::under(0.8);

    assert!(
        planned(&trim, kernel, Some(1_000_000)).await.is_none(),
        "the request about to go is the first to carry it"
    );

    kernel.push(ContextItem::assistant(
        Content::text("a login form"),
        vec![],
    ));
    let plan = planned(&trim, kernel, Some(1_000_000))
        .await
        .expect("it has been shown");
    assert_eq!(plan.elide, vec![attached], "and the pinned one is not");
    assert!(!plan.elide.contains(&at_startup));
}

/// With no limit known the first rule runs as ever, since it is not about room, and the second
/// never does, since there is nothing to measure room against.
#[tokio::test]
async fn with_no_limit_the_first_rule_runs_and_the_second_does_not() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let first = exchange(kernel, "a", &words(40_000), &["x".repeat(4_000)]);
    exchange(kernel, "b", &words(40_000), &[]);

    let trim = Shedder {
        threshold: 0.01,
        target: 0.01,
    };
    assert!(!trim.wants_room(&against(kernel, None)));
    let plan = planned(&trim, kernel, None).await.expect("a turn is over");
    assert_eq!(plan.elide, first.results);
    assert!(
        plan.remove.is_empty(),
        "no exchange is dropped against no limit"
    );
}

/// The whole of an output a limit shortened is archived and stays so; the short copy the model
/// was shown is what the first rule takes.
#[tokio::test]
async fn the_whole_of_a_shortened_output_is_not_the_first_rules() {
    let (kernel, _) = session(4, 1_000_000, Shedder::under(0.8));
    kernel.add_tool(Arc::new(Sized { limit: Some(200) }));
    let provider = Arc::new(
        ScriptedProvider::new([
            ModelResponse::tool_calls(vec![call("c1", "read", json!({ "bytes": 4_000 }))]),
            ModelResponse::text("it is all x"),
        ])
        .with_info(ModelInfo::new("m", "m").with_context_limit(1_000_000)),
    );
    kernel.set_provider(provider);
    kernel.push(ContextItem::user("read a lot"));
    kernel.turn().await.expect("the turn");
    kernel.push(ContextItem::user("and now?"));

    let items = kernel.items();
    let whole = items
        .iter()
        .find(|item| item.state == ContextState::Archived)
        .expect("the whole of it is archived")
        .id;
    let short = items
        .iter()
        .find(|item| {
            matches!(item.kind, ContextKind::ToolResult { .. })
                && item.state == ContextState::Active
        })
        .expect("the copy the model was shown")
        .id;

    let plan = planned(&Shedder::under(0.8), &kernel, Some(1_000_000)).await;
    // the short copy is smaller than a marker at 200 bytes, so there may be nothing to do at all;
    // what must not happen is the archived whole being named
    if let Some(plan) = plan {
        assert!(!plan.elide.contains(&whole) && !plan.remove.contains(&whole));
        assert!(plan.elide.iter().all(|id| *id == short), "{:?}", plan.elide);
    }
}

// -------------------------------------------------------------------- the second rule's edges

/// As many exchanges as it takes go, oldest first, and no more than that: the pass stops at the
/// first one that brings the context under the target.
#[tokio::test]
async fn as_many_exchanges_go_as_it_takes_and_no_more() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let all: Vec<Exchange> = (0..6)
        .map(|n| exchange(kernel, &format!("e{n}"), &words(4_000), &[]))
        .collect();

    // six even exchanges at 95%: down to 50% is three of them
    let limit = filled_to(kernel, 0.95);
    let trim = Shedder {
        threshold: 0.8,
        target: 0.5,
    };
    let plan = planned(&trim, kernel, Some(limit))
        .await
        .expect("over the threshold");
    let expected: Vec<_> = all[..3].iter().flat_map(Exchange::all).collect();
    assert_eq!(plan.remove, expected);
    assert!(
        said(&plan).starts_with("The 3 earliest exchange(s)"),
        "{}",
        said(&plan)
    );

    let report = kernel.apply_compaction(plan);
    assert!(
        report.tokens_after <= (limit as f64 * 0.5) as usize,
        "{} of {limit}",
        report.tokens_after
    );
    assert!(
        report.tokens_after > (limit as f64 * 0.3) as usize,
        "and it stopped there, rather than taking a fourth: {} of {limit}",
        report.tokens_after
    );
}

/// A target equal to the threshold drops just enough to be under it again.
#[tokio::test]
async fn a_target_at_the_threshold_drops_just_enough() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let all: Vec<Exchange> = (0..6)
        .map(|n| exchange(kernel, &format!("e{n}"), &words(4_000), &[]))
        .collect();
    let limit = filled_to(kernel, 0.85);

    let plan = planned(
        &Shedder {
            threshold: 0.8,
            target: 0.8,
        },
        kernel,
        Some(limit),
    )
    .await
    .expect("over the threshold");
    assert_eq!(plan.remove, all[0].all(), "one exchange is enough");
}

/// The turn in progress is never dropped, however large: when it is all that is left, the pass
/// can only elide what the model has already read of it, and the context is full.
#[tokio::test]
async fn the_turn_in_progress_is_never_dropped() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let only = exchange(kernel, "a", &words(40_000), &["x".repeat(4_000)]);
    let limit = filled_to(kernel, 0.9);
    let trim = Shedder::under(0.8);

    let plan = planned(&trim, kernel, Some(limit))
        .await
        .expect("a result it has read");
    assert!(
        plan.remove.iter().all(|id| !only.all().contains(id)),
        "{:?}",
        plan.remove
    );
    assert_eq!(plan.elide, only.results);

    kernel.apply_compaction(plan);
    assert!(planned(&trim, kernel, Some(limit)).await.is_none());
    assert!(
        trim.wants_room(&against(kernel, Some(limit))),
        "and so it is full"
    );
}

/// What outlasts the exchange it was written in stays when the exchange goes: the system
/// instruction, a note the model wrote, a note the person wrote, the kernel's notice, and a file
/// the session was set up with.
#[tokio::test]
async fn what_outlasts_an_exchange_stays_when_it_goes() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let system = kernel.push(ContextItem::system("be brief").pinned());
    let setup = kernel.push(ContextItem::file("README.md", "the readme").pinned());
    let oldest = exchange(kernel, "a", &words(8_000), &[]);
    let kept = [
        kernel.push(ContextItem::new(
            ContextKind::Reference,
            "agent",
            "remember",
            "the build is in ./out",
        )),
        kernel.push(ContextItem::new(
            ContextKind::Reference,
            "memory",
            "note",
            "the CI runner has no network",
        )),
        kernel.push(ContextItem::new(
            ContextKind::Reference,
            "kamchatka",
            "context full",
            "The context is full ...",
        )),
    ];
    exchange(kernel, "b", &words(8_000), &[]);
    exchange(kernel, "c", "and now?", &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert_eq!(plan.remove, oldest.all());
    for id in kept.iter().chain([&system, &setup]) {
        assert!(!plan.remove.contains(id) && !plan.elide.contains(id));
    }
    assert!(kernel.apply_compaction(plan).refused.is_empty());
}

/// A pinned turn keeps the results answering it, and both are left where they are while the rest
/// of their exchange goes.
#[tokio::test]
async fn a_pinned_turn_keeps_its_results() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let oldest = exchange(
        kernel,
        "a",
        &words(8_000),
        &["kept".to_owned(), "also kept".to_owned()],
    );
    kernel.set_state([oldest.turns[0]], ContextState::Pinned, None);
    exchange(kernel, "b", &words(8_000), &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert!(plan.remove.contains(&oldest.asked));
    assert!(!plan.remove.contains(&oldest.turns[0]));
    assert!(
        !plan.remove.contains(&oldest.results[0]),
        "its result stays with it"
    );
    assert!(
        plan.remove.contains(&oldest.turns[1]) && plan.remove.contains(&oldest.results[1]),
        "and the other pair goes: {:?}",
        plan.remove
    );
    assert!(kernel.apply_compaction(plan).refused.is_empty());
    assert!(kernel.project().repairs.is_empty());
}

/// What the person or the model already took out is not taken again, and what they elided goes
/// with the exchange as the marker it is.
#[tokio::test]
async fn an_exchange_partly_taken_out_already_goes_as_what_is_left_of_it() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let oldest = exchange(
        kernel,
        "a",
        &words(8_000),
        &["x".repeat(4_000), "y".repeat(4_000)],
    );
    kernel.set_state(
        [oldest.turns[0]],
        ContextState::Excluded,
        Some("by hand".into()),
    );
    kernel.set_state(
        [oldest.results[1]],
        ContextState::Elided,
        Some("I have it".into()),
    );
    exchange(kernel, "b", &words(8_000), &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert!(!plan.remove.contains(&oldest.turns[0]), "already out");
    assert!(
        !plan.remove.contains(&oldest.results[0]),
        "out with the turn that asked for it"
    );
    assert!(
        plan.remove.contains(&oldest.results[1]),
        "the marker goes too"
    );
    assert!(plan.remove.contains(&oldest.asked) && plan.remove.contains(&oldest.answer));
    let report = kernel.apply_compaction(plan);
    assert!(report.refused.is_empty());
}

/// An exchange the person took out by hand is not counted as one the compactor dropped.
#[tokio::test]
async fn an_exchange_excluded_by_hand_is_not_counted_as_dropped() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let by_hand = exchange(kernel, "a", &words(8_000), &[]);
    kernel.set_state(
        by_hand.all(),
        ContextState::Excluded,
        Some("at the terminal".into()),
    );
    exchange(kernel, "b", &words(8_000), &[]);
    exchange(kernel, "c", &words(8_000), &[]);
    exchange(kernel, "d", "and now?", &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert!(said(&plan).starts_with("The 1 earliest"), "{}", said(&plan));
}

/// An exchange smaller than the summary its going would leave is not dropped: the pass would
/// put a paragraph where two short lines were.
#[tokio::test]
async fn an_exchange_smaller_than_its_summary_is_not_dropped() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let pinned = kernel.push(ContextItem::file("big.txt", words(8_000)).pinned());
    exchange(kernel, "a", "hi", &[]);
    exchange(kernel, "b", "and now?", &[]);

    assert!(
        planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
            .await
            .is_none()
    );
    assert_eq!(kernel.item(pinned).unwrap().state, ContextState::Pinned);
}

/// A pass that drops exchanges and then makes room in the turn in progress says both.
#[tokio::test]
async fn the_summary_says_what_went_from_the_past_and_from_this_turn() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    exchange(kernel, "a", &words(4_000), &[]);
    let current = exchange(
        kernel,
        "b",
        &words(400),
        &["x".repeat(8_000), "y".repeat(8_000)],
    );

    let plan = planned(
        &Shedder {
            threshold: 0.5,
            target: 0.4,
        },
        kernel,
        Some(filled_to(kernel, 0.9)),
    )
    .await
    .expect("over the threshold");
    let said = said(&plan);
    assert!(said.contains("The 1 earliest exchange(s)"), "{said}");
    assert!(said.contains("you had already read in this turn"), "{said}");
    assert!(plan.elide.contains(&current.results[0]));
}

/// A summary the person pinned is theirs, and the next pass's summary does not supersede it.
#[tokio::test]
async fn a_pinned_summary_is_not_superseded() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let pinned = kernel.push(ContextItem::summary("an earlier pass's summary").pinned());
    let older = kernel.push(ContextItem::summary("another earlier pass's summary"));
    exchange(kernel, "a", &words(8_000), &[]);
    exchange(kernel, "b", &words(8_000), &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert!(!plan.remove.contains(&pinned));
    assert!(
        plan.remove.contains(&older),
        "the unpinned one is superseded"
    );
}

/// Everything a pass dropped, put back, reads as the conversation it was: nothing the projector
/// has to mend, and every call with its answer.
#[tokio::test]
async fn a_dropped_exchange_put_back_is_whole() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let oldest = exchange(
        kernel,
        "a",
        &words(8_000),
        &["x".repeat(400), "y".repeat(400)],
    );
    exchange(kernel, "b", &words(8_000), &[]);
    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    let report = kernel.apply_compaction(plan);
    assert!(kernel.project().repairs.is_empty());

    let back: Vec<_> = report.removed.iter().map(|removed| removed.id).collect();
    kernel.set_state(back, ContextState::Active, None);
    assert!(kernel.project().repairs.is_empty());
    let going = kernel.project().included;
    assert!(oldest.all().iter().all(|id| going.contains(id)));
}

/// A pinned result keeps the whole turn it answers, the results beside it included.
///
/// note: found by the property below. The turn stayed for the pin's sake and the results beside
/// the pinned one went with the exchange, so the turn kept calls with no answer, and the
/// projector took them out of it on every request from then on.
#[tokio::test]
async fn a_pinned_result_keeps_the_turn_it_answers_whole() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    kernel.push(ContextItem::user(words(8_000)));
    let calls = vec![
        call("c1", "read", json!({ "n": 1 })),
        call("c2", "read", json!({ "n": 2 })),
    ];
    let turn = kernel.push(ContextItem::assistant(Content::text(""), calls.clone()));
    let results: Vec<_> = calls
        .iter()
        .map(|asked| {
            kernel.push(ContextItem::tool_result(
                asked.id.clone(),
                "read",
                "kept",
                false,
            ))
        })
        .collect();
    kernel.push(ContextItem::assistant(Content::text("done"), vec![]));
    kernel.set_state([results[0]], ContextState::Pinned, None);
    exchange(kernel, "b", &words(8_000), &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    for id in [turn, results[0], results[1]] {
        assert!(!plan.remove.contains(&id), "{id}: {:?}", plan.remove);
    }
    kernel.apply_compaction(plan);
    assert!(
        kernel.project().repairs.is_empty(),
        "{:?}",
        kernel.project().repairs
    );
}

/// A result nothing followed before the next question goes with its exchange, and is not elided
/// by the last resort as well.
///
/// note: found by the property below: an unread result at the end of an older exchange is in the
/// range the last resort reads, and the plan named it in both lists.
#[tokio::test]
async fn an_unread_result_in_an_older_exchange_goes_with_it_once() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    kernel.push(ContextItem::user(words(400)));
    let read = call("c1", "read", json!({}));
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![read.clone()],
    ));
    // the turn was stopped here, before the model read it
    let unread = kernel.push(ContextItem::tool_result(
        read.id.clone(),
        "read",
        "x".repeat(4_000),
        false,
    ));
    // and a question past the limit on its own, so that the last resort still runs once the
    // older exchange has gone
    kernel.push(ContextItem::user(words(6_000)));

    let plan = planned(&Shedder::under(0.8), kernel, Some(1_000))
        .await
        .expect("over the limit");
    assert!(plan.remove.contains(&unread));
    assert!(!plan.elide.contains(&unread), "{plan:?}");
}

/// What somebody else elided is priced at the marker it really is: a few words of theirs, not a
/// sentence of the compactor's.
///
/// note: found by the property below. Three short items elided by hand were each credited with
/// this pass's marker when they went, so an exchange of short lines read as worth the summary
/// its going leaves - and the request grew.
#[tokio::test]
async fn what_somebody_else_elided_is_priced_at_its_own_marker() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let pinned = kernel.push(ContextItem::file("big.txt", words(8_000)).pinned());
    let small = exchange(
        kernel,
        "a",
        "hi",
        &["one".to_owned(), "two".to_owned(), "three".to_owned()],
    );
    kernel.set_state(
        small.results.clone(),
        ContextState::Elided,
        Some("seen".into()),
    );
    exchange(kernel, "b", "and now?", &[]);

    let limit = filled_to(kernel, 0.95);
    if let Some(plan) = planned(&Shedder::under(0.8), kernel, Some(limit)).await {
        let report = kernel.apply_compaction(plan);
        assert!(
            report.tokens_after < report.tokens_before,
            "{} -> {}",
            report.tokens_before,
            report.tokens_after
        );
    }
    assert_eq!(kernel.item(pinned).unwrap().state, ContextState::Pinned);
}

/// A turn with nothing left in it for the request is not named when its exchange goes: the
/// projector leaves out a turn that said nothing and whose only call lost its result.
///
/// note: found by the property below. The kernel will not move what the request is not
/// carrying, so the turn stayed as it was, and every pass after named it again.
#[tokio::test]
async fn a_turn_the_request_no_longer_carries_is_not_named() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let asked = kernel.push(ContextItem::user(words(8_000)));
    // a turn that said nothing and made one call, which is how most tool turns look
    let read = call("c1", "read", json!({}));
    let turn = kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![read.clone()],
    ));
    let result = kernel.push(ContextItem::tool_result(
        read.id.clone(),
        "read",
        "x".repeat(400),
        false,
    ));
    kernel.push(ContextItem::assistant(Content::text("done"), vec![]));
    kernel.set_state([result], ContextState::Excluded, Some("by hand".into()));
    exchange(kernel, "b", &words(8_000), &[]);

    let plan = planned(&Shedder::under(0.8), kernel, Some(filled_to(kernel, 0.95)))
        .await
        .expect("over the threshold");
    assert!(plan.remove.contains(&asked));
    assert!(!plan.remove.contains(&turn), "{:?}", plan.remove);
    let report = kernel.apply_compaction(plan);
    assert_eq!(
        report.removed.len(),
        2,
        "the question and the answer: {report:?}"
    );
}

// ------------------------------------------------------------------------- the last resort

/// A picture the model has not been shown is not taken even by the last resort: the counter
/// cannot price it, so taking it would not be seen to help, and it is the thing the model asked for.
#[tokio::test]
async fn the_last_resort_does_not_take_an_unseen_picture() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    kernel.push(ContextItem::user("look at the screen, and read the log"));
    // one turn asking for both, so that neither has been shown: a result before a later turn was
    // in the request that turn answered
    let (shot, read) = (
        call("c1", "screenshot", json!({})),
        call("c2", "read", json!({})),
    );
    kernel.push(ContextItem::assistant(
        Content::text(""),
        vec![shot.clone(), read.clone()],
    ));
    let picture = kernel.push(ContextItem::tool_result(
        shot.id.clone(),
        "screenshot",
        Content::blob("image/png", "A".repeat(600_000)),
        false,
    ));
    let text = kernel.push(ContextItem::tool_result(
        read.id.clone(),
        "read",
        "x".repeat(8_000),
        false,
    ));

    let plan = planned(&Shedder::under(0.8), kernel, Some(1_000))
        .await
        .expect("over the limit");
    assert!(!plan.elide.contains(&picture));
    assert_eq!(plan.elide, vec![text]);
}

/// The last resort takes what the request needs to fit and stops there, oldest first.
#[tokio::test]
async fn the_last_resort_stops_once_the_request_fits() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    kernel.push(ContextItem::user("read both"));
    let calls = vec![
        call("c1", "read", json!({ "n": 1 })),
        call("c2", "read", json!({ "n": 2 })),
    ];
    kernel.push(ContextItem::assistant(Content::text(""), calls.clone()));
    let results: Vec<_> = calls
        .iter()
        .map(|asked| {
            kernel.push(ContextItem::tool_result(
                asked.id.clone(),
                "read",
                "x".repeat(8_000),
                false,
            ))
        })
        .collect();

    // the two of them are 4,000 tokens; one of them alone fits
    let plan = planned(&Shedder::under(0.8), kernel, Some(3_000))
        .await
        .expect("over the limit");
    assert_eq!(plan.elide, vec![results[0]]);
    assert!(
        said(&plan).contains("1 of them before you could read them"),
        "{}",
        said(&plan)
    );
}

// ----------------------------------------------------------------- whole sessions, end to end

/// A kernel with the sized `read`, a scripted model behind it at this limit, and this compactor.
///
/// note: a `Kernel` and not a `Harness`, because what is under test here is what the turn loop
/// sends, and the screen would only be in the way of reading it.
fn session(
    turns: usize,
    limit: usize,
    trim: Shedder,
) -> (Kernel, tokio::sync::broadcast::Receiver<Event>) {
    let kernel = Kernel::new(Config {
        max_requests_per_turn: Some(turns + 2),
        ..Config::default()
    });
    let events = kernel.subscribe();
    kernel.set_provider(Arc::new(
        ScriptedProvider::new([]).with_info(ModelInfo::new("m", "m").with_context_limit(limit)),
    ));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(Sized { limit: None }));
    kernel.set_compactor(Some(Arc::new(trim)));

    (kernel, events)
}

/// A small generator that needs no crate and gives the same session every run.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % below.max(1)
    }
}

/// What a session's script is: per turn, the person's words, the sizes of what each step asks
/// `read` for, and the size of the answer.
struct Script {
    turns: Vec<(usize, Vec<Vec<usize>>, usize)>,
}

impl Script {
    fn rolled(dice: &mut Dice, turns: usize, scale: usize) -> Self {
        Self {
            turns: (0..turns)
                .map(|_| {
                    let steps = (0..dice.roll(4))
                        .map(|_| (0..1 + dice.roll(3)).map(|_| dice.roll(scale)).collect())
                        .collect();
                    (1 + dice.roll(scale / 2), steps, 1 + dice.roll(scale / 4))
                })
                .collect(),
        }
    }

    /// The model's side of turn `n`: one response per step, then the answer.
    fn responses(&self, n: usize) -> Vec<ModelResponse> {
        let (_, steps, answer) = &self.turns[n];
        let mut out: Vec<ModelResponse> = steps
            .iter()
            .enumerate()
            .map(|(step, sizes)| {
                ModelResponse::tool_calls(
                    sizes
                        .iter()
                        .enumerate()
                        .map(|(k, size)| {
                            call(
                                &format!("t{n}-s{step}-{k}"),
                                "read",
                                json!({ "bytes": size }),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        out.push(ModelResponse::text("w".repeat(*answer)));
        out
    }
}

/// What every request a session sent, and the passes it made, must have been.
#[derive(Default)]
struct Watched {
    requests: usize,
    passes: usize,
    dropped: usize,
    /// How many times the session was said to be full.
    full: usize,
}

/// Runs turn `n` of the script, and holds every request and pass of it to the rules.
async fn play(
    kernel: &Kernel,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    script: &Script,
    n: usize,
    limit: usize,
    watched: &mut Watched,
) {
    let (asked, ..) = &script.turns[n];
    kernel.set_provider(Arc::new(
        ScriptedProvider::new(script.responses(n))
            .with_info(ModelInfo::new("m", "m").with_context_limit(limit)),
    ));
    let start = kernel.push(ContextItem::user(words(*asked)));
    kernel.turn().await.expect("the turn");

    while let Ok(event) = events.try_recv() {
        match event {
            Event::ModelRequested {
                tokens,
                items,
                repairs,
                ..
            } => {
                watched.requests += 1;
                assert!(
                    tokens <= limit,
                    "turn {n}: a request of {tokens} against {limit} was sent"
                );
                assert!(repairs.is_empty(), "turn {n}: {repairs:?}");
                assert!(
                    items.contains(&start),
                    "turn {n}: the person's message went missing"
                );
            }
            Event::Compacted { report } => {
                watched.passes += 1;
                watched.dropped += report.removed.len();
                assert!(report.refused.is_empty(), "turn {n}: {:?}", report.refused);
                assert!(
                    report.tokens_after <= report.tokens_before,
                    "turn {n}: a pass grew the request, {} -> {}",
                    report.tokens_before,
                    report.tokens_after
                );
                // nothing of the turn in progress is ever excluded
                let current: Vec<_> = kernel
                    .items()
                    .iter()
                    .skip_while(|item| item.id != start)
                    .map(|item| item.id)
                    .collect();
                for removed in &report.removed {
                    let item = kernel.item(removed.id).unwrap();
                    assert!(
                        !current.contains(&removed.id) || item.source == "compaction",
                        "turn {n}: {} of the turn in progress was dropped",
                        removed.id
                    );
                }
            }
            Event::ContextFull { full: true, .. } => watched.full += 1,
            _ => {}
        }
    }
}

/// What the context must look like between turns, whatever happened in them.
fn hold(kernel: &Kernel, n: usize) {
    let items = kernel.items();
    let dropped = |item: &ContextItem| {
        item.state == ContextState::Excluded
            && item
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with("compaction: compacted"))
    };

    // the person's messages the compactor dropped are the oldest of them, in a run
    let asked: Vec<_> = items
        .iter()
        .filter(|item| item.kind == ContextKind::UserMessage)
        .collect();
    let gone = asked.iter().take_while(|item| dropped(item)).count();
    assert!(
        asked[gone..].iter().all(|item| !dropped(item)),
        "turn {n}: an exchange went out of order"
    );
    assert!(
        gone < asked.len(),
        "turn {n}: the newest exchange was dropped"
    );

    // one summary at most is standing, and it counts what went
    let standing: Vec<_> = items
        .iter()
        .filter(|item| item.source == "compaction" && item.state.sends_content())
        .collect();
    assert!(
        standing.len() <= 1,
        "turn {n}: {} summaries",
        standing.len()
    );
    assert!(
        gone == 0 || standing.len() == 1,
        "turn {n}: {gone} exchanges gone and nothing saying so"
    );
    if let Some(summary) = standing.first() {
        let text = summary.content.to_text();
        if gone > 0 {
            assert!(
                text.starts_with(&format!("The {gone} earliest exchange(s)")),
                "turn {n}: {gone} gone, and the summary says {text}"
            );
        }
    }

    // every finished turn's result worth eliding is elided or gone with its exchange
    let last = items
        .iter()
        .rposition(|item| item.kind == ContextKind::UserMessage)
        .unwrap();
    for item in &items[..last] {
        if matches!(item.kind, ContextKind::ToolResult { .. }) && item.state == ContextState::Active
        {
            assert!(
                item.tokens < 120,
                "turn {n}: a finished turn's result of {} tokens is still being sent",
                item.tokens
            );
        }
    }

    // and the request is one the projector did not have to mend
    assert!(kernel.project().repairs.is_empty(), "turn {n}");
}

/// A long session holds to every rule at every request: never over the limit, never a repair,
/// never the turn in progress dropped, exchanges dropped oldest first, and one summary counting
/// them.
#[tokio::test]
async fn a_long_session_holds_to_the_rules_at_every_request() {
    for seed in [1, 7, 42] {
        let mut dice = Dice(seed);
        let limit = 12_000;
        let script = Script::rolled(&mut dice, 60, 6_000);
        let (kernel, mut events) = session(4, limit, Shedder::under(0.8));
        let mut watched = Watched::default();
        for n in 0..script.turns.len() {
            play(&kernel, &mut events, &script, n, limit, &mut watched).await;
            hold(&kernel, n);
        }
        assert!(watched.requests > 60, "seed {seed}: {}", watched.requests);
        assert!(
            watched.dropped > 0,
            "seed {seed}: a session this long never filled"
        );
    }
}

/// The same, with the target at the threshold: just enough each time, and the rules the same.
#[tokio::test]
async fn a_long_session_at_a_target_equal_to_its_threshold() {
    let mut dice = Dice(3);
    let limit = 10_000;
    let script = Script::rolled(&mut dice, 50, 5_000);
    let (kernel, mut events) = session(
        4,
        limit,
        Shedder {
            threshold: 0.7,
            target: 0.7,
        },
    );
    let mut watched = Watched::default();
    for n in 0..script.turns.len() {
        play(&kernel, &mut events, &script, n, limit, &mut watched).await;
        hold(&kernel, n);
    }
    assert!(watched.dropped > 0);
}

/// A gap between target and threshold makes fewer passes that drop anything than none does, which
/// is what it is for.
#[tokio::test]
async fn a_lower_target_drops_in_fewer_bursts() {
    let mut drops = Vec::new();
    for target in [0.8, 0.5] {
        let mut dice = Dice(11);
        let limit = 12_000;
        // small turns, so that one of them is a few points of the limit rather than most of the
        // gap between the two marks
        let script = Script::rolled(&mut dice, 80, 1_200);
        let (kernel, mut events) = session(
            4,
            limit,
            Shedder {
                threshold: 0.8,
                target,
            },
        );
        let mut bursts = 0;
        for n in 0..script.turns.len() {
            let mut watched = Watched::default();
            play(&kernel, &mut events, &script, n, limit, &mut watched).await;
            bursts += (watched.dropped > 0) as usize;
        }
        drops.push(bursts);
    }
    assert!(
        drops[1] < drops[0],
        "a target of 0.5 dropped in {} turns, and one at the threshold in {}",
        drops[1],
        drops[0]
    );
}

/// A session carried across a snapshot goes on as though it had not stopped: the notes the
/// summary counts by are in the snapshot, so the count carries on from where it was.
#[tokio::test]
async fn a_session_carried_across_a_restart_counts_on_from_where_it_was() {
    let mut dice = Dice(5);
    let limit = 12_000;
    let script = Script::rolled(&mut dice, 40, 6_000);
    let (kernel, mut events) = session(4, limit, Shedder::under(0.8));
    let mut watched = Watched::default();
    for n in 0..20 {
        play(&kernel, &mut events, &script, n, limit, &mut watched).await;
    }
    hold(&kernel, 19);

    let snapshot = serde_json::from_str(&serde_json::to_string(&kernel.snapshot()).unwrap())
        .expect("a snapshot reads back");
    let resumed = Kernel::resume(
        Config {
            max_requests_per_turn: Some(6),
            ..Config::default()
        },
        snapshot,
    );
    let mut events = resumed.subscribe();
    resumed.set_policy(Arc::new(AllowAll));
    resumed.add_tool(Arc::new(Sized { limit: None }));
    resumed.set_compactor(Some(Arc::new(Shedder::under(0.8))));
    for n in 20..40 {
        play(&resumed, &mut events, &script, n, limit, &mut watched).await;
        hold(&resumed, n);
    }
}

/// One undo puts a whole pass back, exchanges and markers alike, and the next request takes them
/// again rather than sending a context over the threshold.
#[tokio::test]
async fn one_undo_puts_a_pass_back() {
    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;
    let oldest = exchange(kernel, "a", &words(8_000), &["x".repeat(4_000)]);
    exchange(kernel, "b", &words(8_000), &[]);
    let limit = filled_to(kernel, 0.95);
    let trim = Shedder::under(0.8);
    let plan = planned(&trim, kernel, Some(limit))
        .await
        .expect("over the threshold");
    kernel.apply_compaction(plan);
    assert!(
        oldest
            .all()
            .iter()
            .all(|id| !kernel.item(*id).unwrap().state.sends_content())
    );

    assert!(kernel.undo().expect("undo"));
    assert!(
        oldest
            .all()
            .iter()
            .all(|id| kernel.item(*id).unwrap().state == ContextState::Active),
        "every item of the pass is back as it was"
    );
    assert!(
        kernel
            .items()
            .iter()
            .all(|item| item.source != "compaction"),
        "and the summary it left is gone"
    );
    assert!(planned(&trim, kernel, Some(limit)).await.is_some());
}

/// A tool loop that fills the context mid-turn has older exchanges dropped between its steps, and
/// is told the context is full only once nothing older is left.
#[tokio::test]
async fn a_turn_that_fills_the_context_has_the_past_dropped_between_its_steps() {
    let limit = 8_000;
    let (kernel, mut events) = session(8, limit, Shedder::under(0.8));
    let mut watched = Watched::default();
    // three earlier exchanges of words, a third of the limit between them
    let quiet = Script {
        turns: vec![(3_000, vec![], 100); 3],
    };
    for n in 0..3 {
        play(&kernel, &mut events, &quiet, n, limit, &mut watched).await;
    }
    assert_eq!(watched.dropped, 0);

    // and one that reads six large things in a row
    let busy = Script {
        turns: vec![(100, vec![vec![6_000]; 6], 100)],
    };
    play(&kernel, &mut events, &busy, 0, limit, &mut watched).await;
    hold(&kernel, 3);
    let gone = kernel
        .items()
        .iter()
        .filter(|item| {
            item.kind == ContextKind::UserMessage && item.state == ContextState::Excluded
        })
        .count();
    assert!(gone >= 1, "the past made room for the turn");
    assert_eq!(
        watched.full, 0,
        "and with the past to drop and the turn's reads to elide, it was never full"
    );
}

/// A turn too big for the context on its own - its own message and turns, which no pass takes -
/// is said to be full once, however many requests it goes on to make.
#[tokio::test]
async fn a_turn_too_big_on_its_own_is_said_to_be_full_once() {
    let limit = 8_000;
    let (kernel, mut events) = session(8, limit, Shedder::under(0.8));
    let mut watched = Watched::default();
    // the person's own message is most of the limit, and then four small reads
    let script = Script {
        turns: vec![(26_000, vec![vec![200]; 4], 100)],
    };
    play(&kernel, &mut events, &script, 0, limit, &mut watched).await;
    assert_eq!(watched.full, 1, "{} requests", watched.requests);
    assert!(watched.requests >= 5);
}

// -------------------------------------------------------------------------------- `/compact`

/// `/compact` in a context between the target and the threshold says the compactor has nothing
/// to do, measured against where it starts - not that nothing in the context is eligible.
#[tokio::test]
async fn compact_between_the_marks_says_where_the_compactor_starts() {
    let mut harness = Harness::new([]);
    harness.app.kernel.set_compactor(Some(Arc::new(Shedder {
        threshold: 0.8,
        target: 0.3,
    })));
    harness.app.compact_target = Some(0.3);
    harness.app.compact_threshold = Some(0.8);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(ContextItem::user(words(limit * 2)));
    let used = harness
        .app
        .kernel
        .budget()
        .fraction_used()
        .expect("a limit");
    assert!((0.3..0.8).contains(&used), "the setup is off: {used}");
    harness.drain();

    harness.send("/compact").await;
    harness.settle().await;
    let screen = harness.flat();
    assert!(screen.contains("starts making room at"), "{screen}");
    assert!(!screen.contains("found nothing it may take"), "{screen}");
}

// ------------------------------------------------------------------------------ any context

/// Whatever the context holds and whatever the marks are, a pass keeps every promise the rules
/// make: it names nothing pinned and nothing twice, drops nothing of the turn in progress and
/// nothing that outlasts an exchange, elides nothing unread except to make the request fit,
/// leaves the conversation as whole as it found it, and does not make the request bigger.
///
/// note: a property rather than more cases, because the cases above are the edges somebody
/// thought of, and the interleavings of a pin, a hand-made exclusion, a restore with a note and a
/// picture in an attachment are where a rule written for one of them is wrong about another.
#[test]
fn any_pass_over_any_context_keeps_the_rules() {
    use proptest::prelude::*;

    /// What a generated context is made of, in the order it is pushed.
    #[derive(Debug, Clone)]
    enum Piece {
        /// The person asks, at this length.
        Asked(usize),
        /// A file attached before the next question: text, or a picture.
        Attached { picture: bool },
        /// One step of the model's: a call per size, answered in order.
        Step(Vec<usize>),
        /// A picture a tool answered with.
        Screenshot,
        /// The model answers in words.
        Answer(usize),
        /// A note the model or the person wrote.
        Note,
    }

    /// Something somebody did to an item afterwards.
    #[derive(Debug, Clone, Copy)]
    enum Touch {
        Pin(usize),
        Exclude(usize),
        Elide(usize),
        Restore(usize),
    }

    let piece = prop_oneof![
        3 => (10usize..6_000).prop_map(Piece::Asked),
        1 => any::<bool>().prop_map(|picture| Piece::Attached { picture }),
        4 => prop::collection::vec(10usize..8_000, 1..3).prop_map(Piece::Step),
        1 => Just(Piece::Screenshot),
        3 => (10usize..2_000).prop_map(Piece::Answer),
        1 => Just(Piece::Note),
    ];
    let touch = prop_oneof![
        (0usize..64).prop_map(Touch::Pin),
        (0usize..64).prop_map(Touch::Exclude),
        (0usize..64).prop_map(Touch::Elide),
        (0usize..64).prop_map(Touch::Restore),
    ];
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");

    /// What the runs reached, so that a property over every kind of pass cannot quietly become one
    /// over the empty plan.
    #[derive(Default, Debug)]
    struct Reached {
        routine: usize,
        dropped: usize,
        this_turn: usize,
        last_resort: usize,
        a_picture: usize,
        around_a_pin: usize,
    }
    let reached = std::cell::RefCell::new(Reached::default());

    proptest!(
        ProptestConfig {
            cases: 512,
            failure_persistence: None,
            ..ProptestConfig::default()
        },
        |(
            pieces in prop::collection::vec(piece, 1..40),
            touches in prop::collection::vec(touch, 0..6),
            threshold in 0.05f64..1.0,
            under in 0.0f64..1.0,
            filled in 0.2f64..2.0,
            known in prop::bool::weighted(0.9),
        )| {
            let kernel = Kernel::new(Config::default());
            kernel.push(ContextItem::system("be brief").pinned());
            let mut calls = 0;
            for piece in &pieces {
                match piece {
                    Piece::Asked(bytes) => {
                        kernel.push(ContextItem::user(words(*bytes)));
                    }
                    Piece::Attached { picture: true } => {
                        kernel.push(ContextItem::file(
                            "screen.png",
                            Content::blob("image/png", "A".repeat(60_000)),
                        ));
                    }
                    Piece::Attached { picture: false } => {
                        kernel.push(ContextItem::file("notes.md", words(3_000)));
                    }
                    Piece::Step(sizes) => {
                        let asked: Vec<_> = sizes
                            .iter()
                            .map(|_| {
                                calls += 1;
                                call(&format!("c{calls}"), "read", json!({}))
                            })
                            .collect();
                        kernel.push(ContextItem::assistant(Content::text(""), asked.clone()));
                        for (asked, size) in asked.iter().zip(sizes) {
                            kernel.push(ContextItem::tool_result(
                                asked.id.clone(),
                                "read",
                                "x".repeat(*size),
                                false,
                            ));
                        }
                    }
                    Piece::Screenshot => {
                        calls += 1;
                        let shot = call(&format!("c{calls}"), "screenshot", json!({}));
                        kernel.push(ContextItem::assistant(Content::text(""), vec![shot.clone()]));
                        kernel.push(ContextItem::tool_result(
                            shot.id.clone(),
                            "screenshot",
                            Content::blob("image/png", "A".repeat(60_000)),
                            false,
                        ));
                    }
                    Piece::Answer(bytes) => {
                        kernel.push(ContextItem::assistant(Content::text("w".repeat(*bytes)), vec![]));
                    }
                    Piece::Note => {
                        kernel.push(ContextItem::new(
                            ContextKind::Reference,
                            "agent",
                            "remember",
                            "the build is in ./out",
                        ));
                    }
                }
            }
            let ids: Vec<_> = kernel.items().iter().map(|item| item.id).collect();
            for touch in &touches {
                let at = |k: usize| ids[k % ids.len()];
                match *touch {
                    Touch::Pin(k) => kernel.set_state([at(k)], ContextState::Pinned, None),
                    Touch::Exclude(k) => {
                        kernel.set_state([at(k)], ContextState::Excluded, Some("by hand".into()))
                    }
                    Touch::Elide(k) => {
                        kernel.set_state([at(k)], ContextState::Elided, Some("I have it".into()))
                    }
                    Touch::Restore(k) => {
                        kernel.set_state([at(k)], ContextState::Active, Some("brought back".into()))
                    }
                };
            }

            let trim = Shedder {
                threshold,
                target: threshold * under.max(0.05),
            };
            let limit = known.then(|| ((kernel.budget().used() as f64 / filled) as usize).max(1));
            let items = kernel.items();
            let budget = against(&kernel, limit);
            let repaired_before = kernel.project().repairs.is_empty();
            let Some(plan) = runtime.block_on(trim.plan(&items, &budget)) else {
                return Ok(());
            };

            let index = |id: ContextId| items.iter().position(|item| item.id == id).unwrap();
            let item = |id: ContextId| items[index(id)].clone();
            let seen = items
                .iter()
                .rposition(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
                .unwrap_or(0);
            let current = {
                let last = items.iter().rposition(|item| item.kind == ContextKind::UserMessage);
                let mut start = last.unwrap_or(0);
                while last.is_some()
                    && start > 0
                    && items[start - 1].kind == ContextKind::Reference
                    && items[start - 1].source == "file"
                {
                    start -= 1;
                }
                start
            };

            // nothing twice, and nothing in both lists
            let mut named: Vec<_> = plan.elide.iter().chain(&plan.remove).copied().collect();
            named.sort();
            let total = named.len();
            named.dedup();
            prop_assert_eq!(named.len(), total, "named twice: {:?}", plan);

            for id in &plan.elide {
                let it = item(*id);
                prop_assert!(it.state != ContextState::Pinned, "{id} is pinned");
                prop_assert!(it.state.sends_content(), "{id} is not being sent");
                prop_assert!(
                    index(*id) < seen || plan.reason.starts_with("compacted because the request"),
                    "{id} unread, and elided for `{}`",
                    plan.reason
                );
                prop_assert!(
                    matches!(it.kind, ContextKind::ToolResult { .. })
                        || (it.kind == ContextKind::Reference && !it.content.blobs().is_empty()),
                    "{id} is not a result or a picture"
                );
                // an item somebody said something about is the room rules' only
                prop_assert!(
                    it.note.is_none() || trim.wants_room(&budget),
                    "{id} carries a note and the context is not full"
                );
            }
            for id in &plan.remove {
                let it = item(*id);
                prop_assert!(it.state != ContextState::Pinned, "{id} is pinned");
                if it.source == "compaction" {
                    continue;
                }
                prop_assert!(index(*id) < current, "{id} is in the turn in progress");
                prop_assert!(
                    matches!(
                        it.kind,
                        ContextKind::UserMessage
                            | ContextKind::AssistantMessage { .. }
                            | ContextKind::ToolResult { .. }
                    ) || it.source == "file",
                    "{id} outlasts its exchange and was dropped with it"
                );
            }
            if !trim.wants_room(&budget) {
                prop_assert!(plan.remove.is_empty(), "nothing is dropped short of the threshold");
                prop_assert!(plan.summary.is_none());
            }

            let elided_a_picture = plan
                .elide
                .iter()
                .any(|id| !item(*id).content.blobs().is_empty());
            {
                let mut tally = reached.borrow_mut();
                tally.routine += plan.reason.contains("to keep the context short") as usize;
                tally.dropped += plan.remove.iter().any(|id| index(*id) < current) as usize;
                tally.this_turn += plan.elide.iter().any(|id| index(*id) >= current) as usize;
                tally.last_resort +=
                    plan.reason.starts_with("compacted because the request") as usize;
                tally.a_picture += elided_a_picture as usize;
                tally.around_a_pin += (!plan.remove.is_empty()
                    && items.iter().any(|it| it.state == ContextState::Pinned && index(it.id) > 0))
                    as usize;
            }
            let report = kernel.apply_compaction(plan.clone());
            prop_assert!(report.refused.is_empty(), "refused: {:?}", report.refused);
            prop_assert!(
                !repaired_before || kernel.project().repairs.is_empty(),
                "the pass broke the conversation: {:?}",
                kernel.project().repairs
            );
            // a picture is priced at nothing and leaves a marker priced at something, so a pass
            // that took one may read as growing by the marker; nothing else may
            if !elided_a_picture {
                prop_assert!(
                    report.tokens_after < report.tokens_before,
                    "{} -> {}: {:?}",
                    report.tokens_before,
                    report.tokens_after,
                    plan
                );
            }

            // and the next pass does not take again what this one took: an elided item may go on
            // to be dropped with its exchange, but nothing is elided twice or dropped twice
            let again = runtime.block_on(trim.plan(&kernel.items(), &against(&kernel, limit)));
            if let Some(again) = again {
                for id in &again.elide {
                    prop_assert!(
                        !plan.elide.contains(id) && !plan.remove.contains(id),
                        "{id} elided again"
                    );
                }
                for id in &again.remove {
                    prop_assert!(!plan.remove.contains(id), "{id} dropped again");
                }
            }
        }
    );

    let reached = reached.into_inner();
    assert!(
        reached.routine > 0
            && reached.dropped > 0
            && reached.this_turn > 0
            && reached.last_resort > 0
            && reached.a_picture > 0
            && reached.around_a_pin > 0,
        "a kind of pass was never made: {reached:?}"
    );
}
