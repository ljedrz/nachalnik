//! The six seams, swapped while a session is running - and what a compactor may and may not do.
//!
//! note: every component is replaceable mid-session and each one can say what it is, which is
//! what lets a client put the seams on a screen. The compaction tests are here rather than with
//! the context because what is being checked is the *kernel's* half of the bargain: a plan is
//! applied in the open, reported exactly, reversible with one undo, and refused where it reaches
//! for a pin.

use std::sync::Arc;

use nachalnik::{
    Config, ContextItem, ContextState, Event, Kernel, ModelInfo, ModelResponse, Verdict,
    test::{AllowAll, DenyAll, LargestFirstCompactor, ScriptedProvider, call},
};
use serde_json::json;

use crate::common::{drain, names, permissive};

#[tokio::test]
async fn compaction_is_visible_and_reversible() {
    let provider = Arc::new(
        ScriptedProvider::new([ModelResponse::text("thanks")]).with_info(ModelInfo {
            context_limit: Some(1_000),
            tool_calling: true,
            ..ModelInfo::new("scripted", "small")
        }),
    );
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));

    // the turn that asked for them, so that the results are answering something and the budget
    // counts them: an orphaned result is one the projector drops, and a budget that counted it
    // would be quoting for a request that is not going to be sent
    kernel.push(ContextItem::assistant(
        "",
        vec![
            call("c1", "cargo", json!({})),
            call("c2", "grep", json!({})),
        ],
    ));
    let huge = kernel.push(ContextItem::tool_result(
        "c1".into(),
        "cargo",
        "x".repeat(2_000),
        false,
    ));
    kernel.push(ContextItem::tool_result(
        "c2".into(),
        "grep",
        "y".repeat(400),
        false,
    ));
    kernel.push(ContextItem::user("what now?"));
    let before = kernel.budget().context_tokens;

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    let report = drain(&mut events)
        .into_iter()
        .find_map(|e| match e {
            Event::Compacted { report } => Some(report),
            _ => None,
        })
        .expect("the context was over the threshold");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].label, "cargo");
    assert_eq!(report.removed[0].tokens, 500);
    assert!(report.refused.is_empty());
    assert!(report.summary.is_some());
    assert!(report.reason.contains("1000-token limit"));
    assert!(report.tokens_after < report.tokens_before);

    // "no, put it back"
    assert_eq!(
        kernel.set_state([huge], ContextState::Active, None).changed,
        vec![huge]
    );
    assert!(kernel.budget().context_tokens > before);
}

/// A report's two totals are what a request would cost before and after, and an elided item costs
/// a marker rather than nothing. Summed over the items instead, a pass that made the request
/// bigger reported a decrease - and the figure a person is shown disagreed with the budget beside
/// it, which is the one thing these numbers exist to be held against.
#[tokio::test]
async fn a_compaction_report_charges_for_the_markers_it_left_behind() {
    let kernel = Kernel::new(Config::default());
    let call = call("c1", "grep", json!({}));
    kernel.push(ContextItem::user("go"));
    kernel.push(ContextItem::assistant("looking", vec![call.clone()]));
    let result = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "grep",
        "y".repeat(4_000),
        false,
    ));

    // a note long enough to be worth counting, which is what a compactor's reason really is
    let reason = "compacted to make room; the context had reached 84% of the 1,000-token limit";
    let report = kernel.apply_compaction(nachalnik::CompactionPlan {
        elide: vec![result],
        reason: reason.to_owned(),
        ..Default::default()
    });

    assert_eq!(report.elided.len(), 1);
    assert_eq!(
        report.tokens_after,
        kernel.budget().context_tokens,
        "the report and the budget are counting the same request"
    );
    // the marker is in there: what is left is not nothing, and it is not the item either
    assert!(
        report.tokens_after >= reason.len() / 4,
        "the marker went uncounted: {} left after {}",
        report.tokens_after,
        report.tokens_before
    );
    assert!(report.tokens_after < report.tokens_before / 10);
}

#[tokio::test]
async fn a_pin_is_a_promise() {
    let kernel = Kernel::new(Config::default());
    let pinned = kernel.push(ContextItem::file("src/foo.rs", "x".repeat(400)).pinned());
    let doomed = kernel.push(ContextItem::file("src/bar.rs", "y".repeat(400)));

    let report = kernel.apply_compaction(nachalnik::CompactionPlan {
        remove: vec![pinned, doomed],
        elide: Vec::new(),
        summary: None,
        reason: "an overzealous compactor".into(),
    });

    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].id, pinned);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].id, doomed);
    assert!(kernel.item(pinned).unwrap().is_projected());

    assert!(kernel.undo(), "and even that is one operation");
    assert!(kernel.item(doomed).unwrap().is_projected());
}

#[tokio::test]
async fn the_model_can_be_swapped_mid_session() {
    let (kernel, _) = permissive([ModelResponse::text("one")]);
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    let mut events = kernel.subscribe();
    let previous = kernel
        .set_provider(Arc::new(
            ScriptedProvider::new([ModelResponse::text("two")])
                .with_info(ModelInfo::new("other", "model-2")),
        ))
        .unwrap();
    assert_eq!(previous.info().model, "scripted");
    assert_eq!(kernel.model_info().unwrap().provider, "other");
    assert!(names(&mut events).contains(&"model.changed".to_owned()));

    kernel.push(ContextItem::user("and again"));
    kernel.turn().await.unwrap();
    assert_eq!(
        kernel
            .last_response()
            .unwrap()
            .content
            .clone()
            .unwrap()
            .to_text(),
        "two"
    );
}

#[tokio::test]
async fn every_seam_can_say_what_is_plugged_into_it() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor::default())));

    // a trait object nobody can name is a seam nobody can inspect, which for a runtime whose
    // whole claim is that the parts are visible and replaceable is the wrong way round
    assert!(
        kernel.policy().name().ends_with("AllowAll"),
        "{}",
        kernel.policy().name()
    );
    assert!(
        kernel.projector().name().ends_with("LinearProjector"),
        "the default projector should name itself: {}",
        kernel.projector().name()
    );
    // the default counter is `Calibrating<BytesPerToken>`, and the name says both halves: which
    // estimate is being made, and that it is being corrected
    let counter = kernel.counter().name();
    assert!(
        counter.contains("Calibrating") && counter.contains("BytesPerToken"),
        "{counter}"
    );
    assert!(
        kernel
            .compactor()
            .expect("one was set")
            .name()
            .ends_with("LargestFirstCompactor")
    );
    assert!(kernel.provider().is_some());

    // and swapping one through the seam is visible through the same accessor
    kernel.set_policy(Arc::new(DenyAll));
    assert!(kernel.policy().name().ends_with("DenyAll"));
}

#[tokio::test]
async fn a_policy_can_say_something_friendlier_than_its_type() {
    struct Bespoke;

    #[async_trait::async_trait]
    impl nachalnik::PermissionPolicy for Bespoke {
        async fn evaluate(&self, _: &nachalnik::PermissionRequest) -> Verdict {
            Verdict::Ask
        }
        fn name(&self) -> &'static str {
            "the one from the config file"
        }
    }

    let kernel = Kernel::new(Config::default());
    kernel.set_policy(Arc::new(Bespoke));

    assert_eq!(kernel.policy().name(), "the one from the config file");
}

#[tokio::test]
async fn a_provider_can_be_taken_out_again() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    let mut events = kernel.subscribe();

    let previous = kernel.clear_provider();
    assert!(previous.is_some(), "the one that was there is handed back");
    assert!(kernel.provider().is_none());
    assert!(kernel.model_info().is_none());

    // detaching is a change to the session like any other, so it is on the record
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| matches!(event, Event::ModelChanged { to: None, .. })),
        "clearing the provider should be announced"
    );

    // and a step with nothing to talk to says so rather than doing something surprising
    assert!(matches!(
        kernel.step().await,
        Err(nachalnik::Error::NoProvider)
    ));
}
