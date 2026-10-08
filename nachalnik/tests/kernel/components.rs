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
        ScriptedProvider::new([ModelResponse::text("thanks")]).with_info(
            ModelInfo::new("scripted", "small")
                .with_context_limit(1_000)
                .with_tool_calling(true),
        ),
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

    assert!(kernel.undo().unwrap(), "and even that is one operation");
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

/// The parameters are the one component a caller can hand back unchanged and the kernel can tell,
/// so they follow the rule the context already follows: a set that changes nothing is announced
/// as nothing.
#[tokio::test]
async fn parameters_that_are_already_in_force_are_not_a_change() {
    let kernel = Kernel::new(Config::default());
    let params = json!({"temperature": 0.2}).as_object().unwrap().clone();

    kernel.set_params(params.clone());
    let mut events = kernel.subscribe();

    let previous = kernel.set_params(params.clone());
    assert_eq!(previous, params, "the ones in force are still handed back");
    assert_eq!(kernel.params(), params);
    assert!(
        names(&mut events).is_empty(),
        "a request that will go out byte for byte the same is not a parameter change"
    );

    // and a real one still is
    kernel.set_params(json!({"temperature": 0.9}).as_object().unwrap().clone());
    assert!(names(&mut events).contains(&"model.params".to_owned()));
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

/// A provider that switches its model in place, the way a client sharing one has to.
struct Switching(parking_lot::Mutex<String>);

#[nachalnik::async_trait]
impl nachalnik::Provider for Switching {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("switching", self.0.lock().clone())
    }

    async fn respond(
        &self,
        _request: nachalnik::ModelRequest,
        _deltas: nachalnik::DeltaSink,
    ) -> Result<ModelResponse, nachalnik::BoxError> {
        Ok(ModelResponse::text("hello"))
    }
}

/// A switch made inside the provider is announced once, from the model last announced.
///
/// note: setting the same provider again asked it what it was after the switch, so the record said
/// the change was from the new model to itself - and not setting it said nothing at all.
#[test]
fn a_switch_in_place_is_announced_from_what_was_announced() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Switching(parking_lot::Mutex::new("first".to_owned())));
    kernel.set_provider(provider.clone());
    let mut events = kernel.subscribe();

    assert!(!kernel.provider_changed(), "nothing has changed yet");
    *provider.0.lock() = "second".to_owned();
    assert!(kernel.provider_changed());
    assert!(!kernel.provider_changed(), "and it is announced once");

    let changed: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::ModelChanged { from, to } => {
                Some((from.map(|it| it.model), to.map(|it| it.model)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        changed,
        [(Some("first".to_owned()), Some("second".to_owned()))]
    );
}

/// Setting a provider, or clearing it, names as `from` the model last announced, whatever the
/// provider being let go has switched to since.
///
/// note: both asked the outgoing provider what it was, so one that switched in place and was then
/// set again was recorded as changing from the new model to itself, and the record never said it
/// had been the old one.
#[test]
fn setting_a_provider_names_the_model_last_announced() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Switching(parking_lot::Mutex::new("first".to_owned())));
    kernel.set_provider(provider.clone());
    let mut events = kernel.subscribe();

    *provider.0.lock() = "second".to_owned();
    kernel.set_provider(provider.clone());
    *provider.0.lock() = "third".to_owned();
    kernel.clear_provider();

    let changed: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::ModelChanged { from, to } => {
                Some((from.map(|it| it.model), to.map(|it| it.model)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        changed,
        [
            (Some("first".to_owned()), Some("second".to_owned())),
            (Some("second".to_owned()), None),
        ]
    );
}

/// A kernel whose model takes a thousand tokens, answering `answers` in turn.
fn limited(answers: usize) -> Kernel {
    let provider = Arc::new(
        ScriptedProvider::new((0..answers).map(|_| ModelResponse::text("ok")))
            .with_info(ModelInfo::new("scripted", "small").with_context_limit(1_000)),
    );
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider);

    kernel
}

/// A context the compactor wants room in and can take nothing more from is said to be full, once,
/// and said not to be once room is made.
///
/// note: measured, not claimed: the compactor is asked, proposes nothing it may take, and still
/// wants room. What is left is for somebody else to take, and this is the moment to tell them -
/// so it is said as it becomes true and not before every request it stays true for.
#[tokio::test]
async fn a_context_nothing_more_can_be_taken_from_is_full() {
    let kernel = limited(3);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    let kept = kernel.push(ContextItem::file("kept.txt", "k".repeat(2_400)).pinned());
    kernel.push(ContextItem::user("go on"));

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();
    let full: Vec<Event> = drain(&mut events)
        .into_iter()
        .filter(|event| event.name() == "context.full")
        .collect();
    assert_eq!(full.len(), 1, "{full:?}");
    assert!(
        matches!(
            full[0],
            Event::ContextFull {
                full: true,
                limit: Some(1_000),
                ..
            }
        ),
        "{full:?}"
    );

    // still full, and not said again
    kernel.push(ContextItem::user("and again"));
    kernel.turn().await.unwrap();
    assert_eq!(crate::common::count(&drain(&mut events), "context.full"), 0);

    // and the person makes the room the compactor could not
    kernel.set_state([kept], ContextState::Excluded, None);
    kernel.push(ContextItem::user("and now"));
    kernel.turn().await.unwrap();
    let full: Vec<Event> = drain(&mut events)
        .into_iter()
        .filter(|event| event.name() == "context.full")
        .collect();
    assert!(
        matches!(full.as_slice(), [Event::ContextFull { full: false, .. }]),
        "{full:?}"
    );
}

/// Whether the context is full is asked of the compactor after a pass, not decided by the pass
/// having happened - and by default the question is the one it was asked before it, asked again
/// of the budget the pass left behind.
///
/// note: the default is what keeps the answer honest in both directions. A compactor asked to pass
/// over half a limit and left a request under a fifth of it says no, and no `context.full` is
/// emitted for a context with room in it; and the override is there for the compactor that wants a
/// pass for some other reason - a look at something it cannot price - which does not thereby find
/// the context full, and is believed when it says the context still is.
#[tokio::test]
async fn a_compactor_that_made_room_does_not_find_the_context_full() {
    /// The compactor the tests above use, insisting afterwards that the context is still full.
    struct AlwaysFull(LargestFirstCompactor);

    #[async_trait::async_trait]
    impl nachalnik::Compactor for AlwaysFull {
        fn should_compact(&self, budget: &nachalnik::Budget) -> bool {
            self.0.should_compact(budget)
        }

        fn wants_room(&self, _budget: &nachalnik::Budget) -> bool {
            true
        }

        async fn plan(
            &self,
            items: &[Arc<ContextItem>],
            budget: &nachalnik::Budget,
        ) -> Option<nachalnik::CompactionPlan> {
            self.0.plan(items, budget).await
        }
    }

    /// A tool result over half the limit, and the turn that asked for it.
    fn push_a_big_result(kernel: &Kernel) {
        let call = call("c1", "grep", json!({}));
        kernel.push(ContextItem::user("go on"));
        kernel.push(ContextItem::assistant("looking", vec![call.clone()]));
        kernel.push(ContextItem::tool_result(
            call.id,
            "grep",
            "x".repeat(2_400),
            false,
        ));
    }

    let kernel = limited(1);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    push_a_big_result(&kernel);
    assert!(
        kernel.budget().fraction_used().unwrap() >= 0.5,
        "which is over the threshold the compactor works at"
    );

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    // the pass took the result, and what is left is a fraction of the limit - so no request has
    // found the context full
    let full = drain(&mut events)
        .into_iter()
        .filter(|event| event.name() == "context.full")
        .collect::<Vec<_>>();
    assert!(full.is_empty(), "{full:?}");
    assert!(
        kernel.budget().fraction_used().unwrap() < 0.2,
        "the pass made room, and there was room to make"
    );

    // while a compactor that says the context is too full for its liking is believed, on the
    // budget the pass left behind rather than on the one it answered
    let full = limited(1);
    full.set_compactor(Some(Arc::new(AlwaysFull(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    }))));
    push_a_big_result(&full);

    let mut full_events = full.subscribe();
    full.turn().await.unwrap();
    assert!(
        full.budget().fraction_used().unwrap() < 0.2,
        "and there was room to make"
    );
    assert_eq!(
        crate::common::count(&drain(&mut full_events), "context.full"),
        1,
        "a compactor that says so is not overruled"
    );
}

/// The caller's notice is put into the context as it becomes full, carried by the request that
/// follows, placed once for as long as it stays full, and excluded as there is room again.
///
/// note: this is how the model hears it. An event reaches a client, and a headless run has no
/// client that can act on one; the notice reaches the model, inside the turn that filled the
/// context, while there is still room under the limit for the request carrying it.
#[tokio::test]
async fn the_full_notice_reaches_the_model_once_and_goes_when_there_is_room() {
    let provider = Arc::new(
        ScriptedProvider::new((0..3).map(|_| ModelResponse::text("ok")))
            .with_info(ModelInfo::new("scripted", "small").with_context_limit(1_000)),
    );
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    let notice = ContextItem::new(
        nachalnik::ContextKind::Reference,
        "client",
        "context full",
        "the context is full; exclude what you no longer need",
    );
    kernel.set_full_notice(Some(notice));
    let kept = kernel.push(ContextItem::file("kept.txt", "k".repeat(2_400)).pinned());
    kernel.push(ContextItem::user("go on"));

    let placed = |kernel: &Kernel| -> Vec<(nachalnik::ContextId, ContextState)> {
        kernel
            .items()
            .iter()
            .filter(|item| item.label == "context full")
            .map(|item| (item.id, item.state))
            .collect()
    };

    kernel.turn().await.unwrap();
    let first = placed(&kernel);
    assert!(
        matches!(first.as_slice(), [(_, ContextState::Active)]),
        "{first:?}"
    );
    assert!(
        format!("{:?}", provider.requests()[0]).contains("exclude what you no longer need"),
        "the request that followed carried it"
    );

    // still full, and not placed again
    kernel.push(ContextItem::user("and again"));
    kernel.turn().await.unwrap();
    assert_eq!(placed(&kernel), first);

    // and the room the compactor could not make retires it
    kernel.set_state([kept], ContextState::Excluded, None);
    kernel.push(ContextItem::user("and now"));
    kernel.turn().await.unwrap();
    assert_eq!(placed(&kernel), vec![(first[0].0, ContextState::Excluded)]);
}

/// A notice that would take the request over the limit is left out, and the request that fits
/// without it goes.
///
/// note: a live run filled its context to a hundred tokens under the limit with one tool result,
/// and the notice placed after it made the request one the kernel refused - so the turn ended
/// with nothing sent and the model never read the notice either.
#[tokio::test]
async fn a_notice_that_would_not_fit_is_left_out_and_the_request_goes() {
    let provider = Arc::new(
        ScriptedProvider::new([ModelResponse::text("ok")])
            .with_info(ModelInfo::new("scripted", "small").with_context_limit(1_000)),
    );
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    kernel.set_full_notice(Some(ContextItem::new(
        nachalnik::ContextKind::Reference,
        "client",
        "context full",
        "n".repeat(400),
    )));
    kernel.push(ContextItem::user("go on"));
    // pinned up to twenty tokens under the limit, which the compactor may not touch
    let room = 1_000 - 20 - kernel.budget().used();
    kernel.push(ContextItem::file("kept.txt", "k".repeat(room * 4)).pinned());
    let used = kernel.budget().used();
    assert!((970..1_000).contains(&used), "{used}");

    kernel
        .turn()
        .await
        .expect("the request fits without the notice");

    assert_eq!(provider.requests().len(), 1, "the request went");
    assert!(
        !kernel
            .items()
            .iter()
            .any(|item| item.label == "context full"),
        "a notice nobody could read was placed"
    );
}

/// A notice already standing in the context is not placed a second time.
///
/// note: the case a resumed session makes. Whether the context is full is not in a snapshot, so a
/// session resumed while full finds it full again as if for the first time - and a notice
/// recognised by an identifier held in memory would be pushed on top of the one already there.
#[tokio::test]
async fn a_standing_notice_is_not_placed_twice() {
    let kernel = limited(1);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    let notice = ContextItem::new(
        nachalnik::ContextKind::Reference,
        "client",
        "context full",
        "the context is full",
    );
    kernel.set_full_notice(Some(notice.clone()));
    kernel.push(ContextItem::file("kept.txt", "k".repeat(2_400)).pinned());
    kernel.push(notice);
    kernel.push(ContextItem::user("go on"));

    kernel.turn().await.unwrap();

    let standing = kernel
        .items()
        .iter()
        .filter(|item| item.label == "context full")
        .count();
    assert_eq!(standing, 1);
}

/// What only resembles the notice does not stand in for it.
///
/// note: the notice is recognised by its kind, source, label and content together. An item of
/// the same kind from the same source says something else, and the person's own message in the
/// notice's words is not the notice either - so with both pinned in the context as it fills, the
/// notice is still placed.
#[tokio::test]
async fn a_notice_is_recognised_by_all_of_what_it_is() {
    let kernel = limited(1);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));
    let words = "the context is full";
    kernel.set_full_notice(Some(ContextItem::new(
        nachalnik::ContextKind::Reference,
        "client",
        "context full",
        words,
    )));
    kernel.push(ContextItem::file("kept.txt", "k".repeat(2_400)).pinned());
    kernel.push(
        ContextItem::new(
            nachalnik::ContextKind::Reference,
            "client",
            "a note",
            "something else",
        )
        .pinned(),
    );
    kernel.push(ContextItem::user(words).pinned());
    kernel.push(ContextItem::user("go on"));

    kernel.turn().await.unwrap();

    let placed = kernel
        .items()
        .iter()
        .filter(|item| item.label == "context full" && item.state == ContextState::Active)
        .count();
    assert_eq!(placed, 1, "a lookalike was taken for the notice");
}
