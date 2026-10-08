//! The status line and the budget: what a request cost, and what this model takes.
//!
//! note: the numbers, and where they came from. An estimate is shown beside what a provider
//! actually charged because the counter in this runtime is honest about being an estimate, and
//! `/params` says what an endpoint publishes rather than what the program hopes it accepts -
//! these check that neither claims more than it knows.

use std::{sync::Arc, time::Duration};

use crossterm::event::KeyCode;
use nachalnik::{
    BoxError, Capability, Content, ContextItem, ContextState, DeltaSink, ModelInfo, ModelRequest,
    ModelResponse, OutputSink, Provider, State, ToolCall, ToolOutput, ToolSpec, Usage, async_trait,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;

use crate::harness::{Harness, grouped};
use kamchatka::app::Tab;

#[tokio::test]
async fn the_budget_puts_the_estimate_beside_what_was_really_charged() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(1_234),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness.app.kernel.push(ContextItem::file(
        "haystack.txt",
        "a needle in it. ".repeat(200),
    ));

    harness.send("go").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.screen();
    // the estimate is not the truth, and the screen is not allowed to imply that it is
    assert!(screen.contains("the next request: ~"), "{screen}");
    assert!(
        screen.contains("really cost 1,234"),
        "the provider's own figure is missing: {screen}"
    );
    assert!(
        screen.contains("learned from 1 request"),
        "the correction it drew from the difference is missing: {screen}"
    );
}

/// A picture the counter cannot price is a floor under the estimate, and not under the anchored
/// figure once a request carrying it has been answered.
///
/// note: the two figures `/budget` puts side by side are made two ways, and the unpriced piece
/// only misses one of them. The estimate is the counter guessing at a whole request, and it has
/// no number for a screenshot; the anchored figure starts from what the provider charged for the
/// last request, which carried it, so that figure has it. Saying "every figure above is a floor"
/// once a request has gone out describes a figure the corner is not showing.
#[tokio::test]
async fn the_budget_says_the_estimate_is_a_floor_and_not_the_anchored_figure() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(4_321),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness.app.kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "A".repeat(4_000),
    )));

    harness.send("what is on the screen?").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.flat();
    assert!(screen.contains("anchored on the last response"), "{screen}");
    assert!(
        screen.contains("the estimate above is a floor"),
        "the estimate is the figure the counter is guessing at: {screen}"
    );
    assert!(
        screen.contains("the anchored figure has what was in the context"),
        "and the provider counted the picture: {screen}"
    );
    assert!(
        !screen.contains("every figure above is a floor"),
        "which is not true of the anchored figure: {screen}"
    );
}

/// And before anything has been sent there is nothing to anchor on, so every figure is the
/// counter's.
#[tokio::test]
async fn the_budget_says_every_figure_is_a_floor_before_anything_has_gone_out() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "A".repeat(4_000),
    )));

    harness.send("/budget").await;

    let screen = harness.flat();
    assert!(screen.contains("every figure above is a floor"), "{screen}");
}

/// A model billed for reasoning it never sends back leaves nothing on the context tab where the
/// thinking was, so the only trace of the money is a number - and until it was read out, not even
/// that. `mercury-2.5` answers one question with 1,139 reasoning tokens and 273 of answer.
#[tokio::test]
async fn the_budget_says_what_the_answer_cost_and_how_much_of_it_was_reasoning() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(9),
            output_tokens: Some(1_412),
            reasoning_tokens: Some(1_139),
            ..Default::default()
        }),
        ..ModelResponse::text("No.")
    }]);

    harness.send("is 9409 prime?").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.screen();
    assert!(
        screen.contains("generated 1,412 out, 1,139 of it reasoning"),
        "the thinking is a share of what was generated, not a second figure: {screen}"
    );
    // and the turn itself said so once, because the words are gone and the tokens are not
    assert!(
        screen.contains("charged for reasoning it does not send back"),
        "nothing said where the turn went: {screen}"
    );
}

/// Said once, because it is true of the endpoint rather than of a turn - the same trap a standing
/// repair fell into, which put one line about item 4 after every message for a whole session.
#[tokio::test]
async fn a_model_that_hides_its_reasoning_is_remarked_on_once() {
    let thinking = || ModelResponse {
        usage: Some(Usage {
            output_tokens: Some(500),
            reasoning_tokens: Some(400),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    };
    let mut harness = Harness::new([thinking(), thinking(), thinking()]);

    for _ in 0..3 {
        harness.send("go").await;
        harness.settle().await;
    }

    let screen = harness.screen();
    assert_eq!(
        screen
            .matches("charged for reasoning it does not send back")
            .count(),
        1,
        "three turns, one sentence: {screen}"
    );
}

/// And said again for the next model, because what it describes is the endpoint, and the endpoint
/// changed.
#[tokio::test]
async fn a_change_of_model_is_remarked_on_afresh() {
    let thinking = || ModelResponse {
        usage: Some(Usage {
            output_tokens: Some(500),
            reasoning_tokens: Some(400),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    };
    let mut harness = Harness::new([thinking(), thinking()]);

    harness.send("go").await;
    harness.settle().await;
    harness.send("/model something-else").await;
    harness.send("go").await;
    // the switch coming back, which hands in the `go` that waited for it, and then its turn
    harness.settle().await;
    harness.settle().await;

    let screen = harness.screen();
    assert_eq!(
        screen
            .matches("charged for reasoning it does not send back")
            .count(),
        2,
        "the second model hides its reasoning too, and nothing said so: {screen}"
    );
}

/// A provider that reports no reasoning is not a provider reporting none of it, and the line that
/// says a model hides its reasoning must appear for neither - nor for one that reports the figure
/// as zero. The zero is said, though: it is the endpoint saying the model did not think, which is
/// worth knowing for somebody who may not know which kind of model they picked.
#[tokio::test]
async fn a_model_that_reports_no_reasoning_is_not_said_to_be_hiding_any() {
    for (reported, said) in [
        (None, "generated 30 out "),
        (Some(0), "generated 30 out, none of it reasoning"),
    ] {
        let mut harness = Harness::new([ModelResponse {
            usage: Some(Usage {
                input_tokens: Some(9),
                output_tokens: Some(30),
                reasoning_tokens: reported,
                ..Default::default()
            }),
            ..ModelResponse::text("done")
        }]);

        harness.send("go").await;
        harness.settle().await;
        harness.send("/budget").await;

        let screen = harness.screen();
        assert!(screen.contains(said), "{reported:?}: {screen}");
        assert!(
            !screen.contains("does not send back"),
            "{reported:?}: no reasoning is not reasoning hidden: {screen}"
        );
        if reported.is_none() {
            assert!(
                !screen.contains("reasoning"),
                "silence about reasoning is not a claim there was none: {screen}"
            );
        }
    }
}

/// What the provider served from its cache is the figure that says what a *change* to the front
/// of the request would cost, and both dialects have always reported it while nothing read it out.
#[tokio::test]
async fn the_budget_says_how_much_of_the_last_request_was_served_from_cache() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(20_000),
            cached_input_tokens: Some(18_000),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);

    harness.send("go").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.flat();
    assert!(screen.contains("really cost 20,000"), "{screen}");
    // the sentence wraps in the pane, so this is the part of it that fits on one line
    assert!(
        screen.contains("18,000 of it (90%) served"),
        "the figure that prices a change to the prefix is missing: {screen}"
    );
}

/// The token figure and the count in one sentence have to be answering the same question. An
/// elided item is projected - as a marker - and is not sending what it holds, so a count of what
/// is *not projected* put nothing beside nine thousand tokens.
#[tokio::test]
async fn the_budget_counts_the_items_the_tokens_it_reports_belong_to() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("go"));
    let elided = harness.app.kernel.push(ContextItem::tool_result(
        nachalnik::ToolCallId::from("c1"),
        "shell",
        "y".repeat(9_000),
        false,
    ));
    harness
        .app
        .kernel
        .set_state([elided], ContextState::Elided, Some("compacted".into()));

    harness.send("/budget").await;
    let screen = harness.flat();

    assert!(
        !screen.contains("in 0 item"),
        "a count of nothing beside the tokens it is supposed to account for: {screen}"
    );
    assert!(screen.contains("in 1 item(s)"), "{screen}");
    // and an elided item *is* sent, as a marker, so the sentence must not say it is not
    assert!(
        !screen.contains("the projector is not sending"),
        "an elided item is in the request: {screen}"
    );
    // read off the panel's own text rather than the screen, where the clause can be broken
    // across two of the panel's lines with whatever is behind the panel showing between them
    let Some(kamchatka::app::Overlay::Text { pages, .. }) = &harness.app.overlay else {
        panic!("`/budget` opened no panel: {screen}");
    };
    assert!(
        pages
            .iter()
            .any(|page| page.body.contains("elided to a marker")),
        "{screen}"
    );
}

#[tokio::test]
async fn the_status_line_says_what_the_budget_is_measured_against() {
    let mut harness = Harness::new([]);
    // a context big enough that its share of the limit is a figure with a decimal place in it,
    // rather than the `0.0%` a two-token context rounds to
    harness.app.kernel.push(ContextItem::file(
        "haystack.txt",
        "a needle in it. ".repeat(400),
    ));

    let screen = harness.screen();
    let status = screen.lines().last().expect("a status line");

    assert!(status.contains("idle"), "{status}");
    assert!(status.contains("scripted"), "{status}");
    // the estimate, marked as one, and the limit it is a percentage *of* - a bare percentage is
    // not something anybody can act on
    assert!(status.contains("~"), "{status}");
    assert!(status.contains("% (128k)"), "{status}");

    // and the percentage is a *share* of that limit rather than the figure itself. The two are
    // one keystroke apart in the source - a quotient and a remainder over the same pair - and
    // only reading the line tells them apart, because a remainder where a quotient belongs
    // comes out as the figure again with a per cent on the end
    let (_, rest) = status.split_once('~').expect("an estimate");
    let (used, rest) = rest.split_once(" tokens, ").expect("a count");
    let (percent, _) = rest.split_once('%').expect("a percentage");
    let used: f64 = used.replace(',', "").parse().expect("a count");
    let percent: f64 = percent.parse().expect("a percentage");
    assert!(used > 1_000.0, "a figure worth a percentage: {status}");
    assert!(
        (percent - used / 128_000.0 * 100.0).abs() < 0.05,
        "{used} of 128,000 is {:.1}%, and the line says {percent}%: {status}",
        used / 128_000.0 * 100.0
    );
}

/// The corner is coloured by how much of the limit is left, and each band is a different colour.
///
/// note: one context in each band rather than one at a threshold. What a person reads is which
/// colour arrived, and a band that had stopped existing altogether would leave every context the
/// same colour - which a test that only sampled the middle of the scale would not catch.
///
/// note: the colour is read from the figure's own first cell rather than from the `Span` the
/// drawing was handed, because what is under test is what a person sees.
#[tokio::test]
async fn the_corner_is_coloured_by_how_full_the_limit_is() {
    // the limit is set rather than inherited, so each case is a fraction of a limit this test
    // chose and the colour cannot be reached by accident through the counter's estimate
    let colour_at = |target: f64| {
        let mut harness = Harness::new([]);
        harness
            .app
            .kernel
            .set_provider(Arc::new(ScriptedProvider::new([]).with_info(
                ModelInfo::new("scripted", "scripted").with_context_limit(100_000),
            )));
        harness.app.kernel.push(ContextItem::file(
            "big.txt",
            "x".repeat((target * 400_000.0) as usize),
        ));
        harness.drain();

        let percent = harness
            .screen()
            .lines()
            .last()
            .and_then(|line| line.split_once(" tokens, "))
            .and_then(|(_, rest)| rest.split_once('%'))
            .map(|(percent, _)| percent.trim().parse::<f64>().expect("a percentage"))
            .unwrap_or_else(|| panic!("the figure is on the line: {target}"));
        // the figure is written in the colour the bands choose, so the needle is the count
        // itself rather than the `tokens` beside it, which every band shares with the dim
        // separator drawn before it
        let count = grouped(harness.app.kernel.budget().used());
        let colour = harness.style_of(&count).0;

        (percent, colour)
    };

    // well clear, nearly full, and past it: three contexts a person reads differently
    let (low, green) = colour_at(0.3);
    assert_eq!(green, ratatui::style::Color::Green, "at {low}%");

    let (high, yellow) = colour_at(0.8);
    assert_eq!(yellow, ratatui::style::Color::Yellow, "at {high}%");

    let (over, red) = colour_at(1.1);
    assert_eq!(red, ratatui::style::Color::Red, "at {over}%");
}

#[tokio::test]
async fn the_address_the_requests_go_to_is_visible_and_can_be_changed() {
    let mut harness = Harness::new([]);

    // where they are going, before anything is switched. A model name means a different model at a
    // different address, so a comparison that cannot see the address is a comparison of names
    harness.send("/endpoint").await;
    let screen = harness.screen();
    assert!(screen.contains("http://127.0.0.1:1"), "{screen}");

    harness
        .send("/endpoint http://127.0.0.1:2/v1 a-model-served-there")
        .await;
    // the probe it starts is a round trip to a port with nothing on it; the address itself changes
    // here, and that is what the next request would use
    for _ in 0..50 {
        if harness.app.provider.endpoint() == "http://127.0.0.1:2/v1" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(harness.app.provider.endpoint(), "http://127.0.0.1:2/v1");
    // the model went with the address, because a model belongs to the address that serves it
    assert_eq!(
        nachalnik::Provider::info(&*harness.app.provider).model,
        "a-model-served-there"
    );

    harness.send("/seams").await;
    let seams = harness.screen();
    assert!(
        seams.contains("http://127.0.0.1:2/v1"),
        "the seam that names the provider says where it is: {seams}"
    );
}

/// The status line pairs the model with where it is being served from, without being asked for it.
///
/// note: `/model`, `/endpoint` and `/seams` have always named the address, but only when asked,
/// so a session pointed at a local ollama drew exactly like one talking to OpenRouter - and the
/// reason for naming the address at all is that those are two different models.
#[tokio::test]
async fn the_status_line_says_where_the_requests_go() {
    let mut harness = Harness::new([ModelResponse::text("done")]);

    harness.send("go").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        screen.contains("@ 127.0.0.1:1"),
        "the status line does not say where the requests go: {screen}"
    );
}

#[tokio::test]
async fn a_session_that_is_working_says_so_with_something_that_moves() {
    let mut harness = Harness::new([]);

    // nothing is happening, so nothing moves: the marker is not decoration on a resting line
    assert!(!harness.screen().contains('•'), "{}", harness.screen());

    harness.app.busy = true;
    let lit = |harness: &mut Harness, ms: u64| -> usize {
        harness.app.since = std::time::Instant::now()
            .checked_sub(Duration::from_millis(ms))
            .expect("a clock with some road behind it");
        harness.dots()
    };

    // three dots, and which one is lit comes from the clock rather than from a frame counter -
    // so it moves at the same rate whatever the screen is doing, and stops where it is if the
    // screen stops being drawn at all, which is the one thing it is there to make visible
    let first = lit(&mut harness, 0);
    let second = lit(&mut harness, 300);
    let third = lit(&mut harness, 600);
    assert_ne!(first, second, "the lit dot did not move");
    assert_ne!(second, third, "the lit dot did not move on");
    assert_eq!(lit(&mut harness, 840), first, "and it goes round");

    // one blink per dot and not one per frame: a marker that steps as fast as the terminal can
    // redraw is a different signal from one that moves at a rate somebody can read across the
    // room, and the number of blinks in the time is what tells the two apart
    let cycles = (0..3)
        .map(|n| lit(&mut harness, 280 * (n + 1)))
        .collect::<Vec<_>>();
    assert_eq!(cycles, [1, 2, 0], "the dot moves on its own clock");
    let quarter = lit(&mut harness, 700);
    assert_eq!(quarter, 2, "and stays lit until the next blink");

    // a short turn stays clean; a long one says how long, because "is it hung?" is the question
    // the marker raises and cannot answer on its own
    harness.app.since = std::time::Instant::now();
    assert!(!harness.screen().contains("0s ·"), "{}", harness.screen());
    harness.app.since = std::time::Instant::now()
        .checked_sub(Duration::from_secs(42))
        .expect("a clock");
    assert!(harness.screen().contains("42s"), "{}", harness.screen());

    // and it goes when the work does
    harness.app.busy = false;
    assert!(!harness.screen().contains('•'), "{}", harness.screen());
}

/// The status line says what the runtime is doing, in words, and a different word for each of the
/// five things it can be doing.
///
/// note: the word is the whole of it. A turn under way, a question somebody has to answer and a
/// call that has been decided but has not run are three different situations, and one word that
/// stands for all three leaves somebody who pressed `esc` and watched nothing happen with no way to
/// tell a wedged turn from one the program has finished with and is waiting on them for.
#[tokio::test]
async fn the_status_line_says_which_thing_the_runtime_is_doing() {
    /// The word the status line opens with, which is the one that names the state.
    ///
    /// note: every word of it rather than the first, because the line is drawn without wrapping and
    /// `waiting on you` runs past the right edge of a hundred-column window on its own. What a
    /// person reads is the whole phrase either way.
    fn said(harness: &mut Harness) -> String {
        let status = harness.sized(100, 30);
        let line = status.lines().last().expect("a status line");
        let first = line.split("·").next().expect("the state is on it");

        // and the marker goes on with the state, so it is dropped: the word says what the runtime
        // is doing, the three dots say how long it has been at it
        first
            .split_whitespace()
            .filter(|word| !word.chars().all(|c| c == '\u{2022}'))
            .collect::<Vec<_>>()
            .join(" ")
    }

    // nothing done and nothing outstanding
    let mut harness = Harness::new([]);
    assert!(matches!(harness.app.kernel.state(), State::Idle));
    assert_eq!(said(&mut harness), "idle");

    // the model has asked for a tool and it is not allowed to run unasked: a question is waiting,
    // and the box it is drawn in says so as well
    let mut asking = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "dig",
        json!({ "where": "there" }),
    )])]);
    asking.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));
    asking.send("dig there").await;
    asking.settle().await;
    assert!(matches!(asking.app.kernel.state(), State::Deciding { .. }));
    assert_eq!(said(&mut asking), "waiting on you");

    // every call decided and none run. `/step` is the only way to stand here, which is why this
    // state has a key of its own
    let mut decided = Harness::new([ModelResponse::tool_calls(vec![call(
        "c2",
        "look",
        json!({}),
    )])]);
    // a tool that needs nothing is decided without being asked about, which is what leaves the
    // runtime in `Ready` rather than in `Deciding`
    decided
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "nothing to see")));
    decided.send("/step look around").await;
    decided.settle().await;
    assert!(matches!(decided.app.kernel.state(), State::Ready { .. }));
    assert_eq!(said(&mut decided), "ready");

    // the next transition runs it, and a tool that has not finished yet is the only way to be
    // caught in the middle of one
    // and the one that needs nothing, so the turn runs it rather than stopping to ask
    let mut running = Harness::new([
        ModelResponse::tool_calls(vec![call("c3", "wait", json!({}))]),
        ModelResponse::text("done"),
    ]);
    running
        .app
        .kernel
        .add_tool(Arc::new(Slow::new(Duration::from_millis(400))));
    running.send("wait").await;
    for _ in 0..2_000 {
        if matches!(running.app.kernel.state(), State::Executing { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        matches!(running.app.kernel.state(), State::Executing { .. }),
        "the tool should still be running"
    );
    assert_eq!(said(&mut running), "running");
    running.settle().await;

    // and the model ended its turn, which is a resting state of its own rather than `idle`: the
    // turn is over and the answer is in
    let mut answered = Harness::new([ModelResponse::text("done")]);
    answered.send("go").await;
    answered.settle().await;
    assert!(matches!(
        answered.app.kernel.state(),
        State::Finished { .. }
    ));
    assert_eq!(said(&mut answered), "done");
}

/// A tool that takes a moment, so that a turn is still running when it is drawn.
///
/// note: `ConstTool` answers between two instructions, which is a turn over before anybody can
/// look at it. The gap is the point of this type; the sleep is the gap.
struct Slow(Duration);

impl Slow {
    fn new(how_long: Duration) -> Self {
        Self(how_long)
    }
}

#[async_trait]
impl nachalnik::Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("wait", "takes a moment")
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        tokio::time::sleep(self.0).await;

        Ok(ToolOutput::new("a bone"))
    }
}

/// And a request that has not come back yet says `asking`, which is the one state a person cannot
/// reach by typing and the only one they need it for: a turn that is in flight is the one that
/// `esc` stops.
///
/// note: a provider that does not answer is what holds the runtime in `Requesting`. Everything else
/// on the line is drawn the same way - the word, the three dots and the clock - so this is the
/// word being read rather than a second mechanism.
#[tokio::test]
async fn a_request_that_has_not_come_back_yet_says_it_is_asking() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .set_provider(Arc::new(Waiting { gate: gate.clone() }));
    harness.app.kernel.push(ContextItem::user("go"));
    harness.app.start_turn();

    let mut asking = String::new();
    for _ in 0..500 {
        if matches!(harness.app.kernel.state(), State::Requesting) {
            asking = harness
                .screen()
                .lines()
                .last()
                .expect("a status line")
                .split_whitespace()
                .next()
                .expect("a word on it")
                .to_owned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        matches!(harness.app.kernel.state(), State::Requesting),
        "the request should still be out"
    );
    assert_eq!(asking, "asking", "a request in flight says so");

    gate.notify_waiters();
    harness.settle().await;
}

/// A model that answers only once the test says so, which is how a request in flight is held open.
///
/// note: `std::future::pending` would do, and cannot be woken again - so this is a gate rather than
/// a hang, and the turn ends when the test opens it.
struct Waiting {
    gate: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for Waiting {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("scripted", "scripted").with_context_limit(128_000)
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.gate.notified().await;

        Ok(ModelResponse::text("late"))
    }
}

#[tokio::test]
async fn params_says_what_this_model_takes_and_what_it_will_quietly_ignore() {
    // the failure this closes: a parameter the model does not take is not refused. It is sent,
    // ignored, and nothing says so - a `seed` set for reproducibility against a model with no
    // `seed` buys none, and the run looks exactly like one that worked. Two models a session
    // apart differ by eight of these
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .set_provider(Arc::new(ScriptedProvider::new([]).with_info(
            ModelInfo::new("scripted", "a/model").with_parameters(vec![
                "temperature".to_owned(),
                "top_p".to_owned(),
                "tools".to_owned(),
            ]),
        )));

    harness.send("/params seed 42").await;
    let screen = harness.screen();
    assert!(
        screen.contains("does not list seed"),
        "the one that will do nothing is named: {screen}"
    );
    // and so is what it would have taken instead, but not a field the request is built from,
    // which `/params` refuses
    assert_eq!(
        listed(&screen, "a/model also takes:"),
        ["temperature", "top_p"],
        "{screen}"
    );

    // one that *is* listed draws no complaint, and drops out of what is left to try
    harness.send("/params temperature 0.2").await;
    let screen = harness.screen();
    assert_eq!(
        listed(&screen, "a/model also takes:"),
        ["top_p"],
        "a parameter already set is not still on offer: {screen}"
    );
    assert!(
        !screen.contains("does not list temperature"),
        "and it is not complained about: {screen}"
    );
}

/// The rows of the last list on `screen` that opens with `opening`, one to a parameter.
fn listed(screen: &str, opening: &str) -> Vec<String> {
    screen
        .lines()
        .rev()
        .take_while(|row| !row.contains(opening))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|row| {
            row.trim_matches(|c: char| c == '│' || c.is_whitespace())
                .to_owned()
        })
        .take_while(|row| !row.is_empty())
        .collect()
}

/// Serves `listing` to whatever asks for it, and, where there is a `refusal`, refuses every other
/// request with it - then hands back a provider that has read the listing.
async fn serving(listing: &'static str, refusal: Option<&'static str>) -> Arc<OpenAiCompatible> {
    let provider = Arc::new(OpenAiCompatible::new(
        "mercury-2.5",
        listening(listing, refusal).await,
        "no key needed",
    ));
    provider.probe().await;
    provider
}

/// Answers every request for a listing with `listing`, and any other with `refusal` where there is
/// one, and hands back the address.
async fn listening(listing: &'static str, refusal: Option<&'static str>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut asked = [0u8; 4096];
            let read = socket.read(&mut asked).await.unwrap_or(0);
            let asked = String::from_utf8_lossy(&asked[..read]);
            let (status, body) = match refusal {
                Some(refusal) if !asked.contains("/models") => ("400 Bad Request", refusal),
                _ => ("200 OK", listing),
            };
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{at}")
}

/// `/models` marks the one this session is asking, where the endpoint listed it.
///
/// note: the mark is what makes the list usable without a second command - somebody reading
/// twenty names has to be told which one they are on - and it stays where the listing put it.
#[tokio::test]
async fn models_marks_the_one_in_use_where_the_endpoint_put_it() {
    let listing = r#"{"data":[{"id":"another"},{"id":"mercury-2.5"},{"id":"a third"}]}"#;
    let mut harness = Harness::served_by([], serving(listing, None).await);

    harness.send("/models").await;
    // the listing is asked for off the loop, and shown when it comes back
    harness.settle().await;

    let screen = harness.screen();
    let marked = screen.find("▸ mercury-2.5").expect(&screen);
    assert!(
        marked > screen.find("another").expect(&screen),
        "the list kept the order the endpoint gave: {screen}"
    );
    // and only that one: the mark is how the single row to hand `/model` is found
    assert!(!screen.contains("▸ another"), "{screen}");
}

/// `/models` given a word lists only what names it.
#[tokio::test]
async fn models_filters_the_listing_to_what_is_named() {
    let listing = r#"{"data":[{"id":"vendor/one"},{"id":"mercury-2.5"},{"id":"vendor/two"}]}"#;
    let mut harness = Harness::served_by([], serving(listing, None).await);

    harness.send("/models two").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("vendor/two"), "{screen}");
    assert!(!screen.contains("vendor/one"), "{screen}");
}

/// Serves one model listing, in the shape of an endpoint that publishes its *sampling* parameters
/// only, and hands back a provider that has read it.
async fn sampling_only() -> Arc<OpenAiCompatible> {
    // quoted from what api.inceptionlabs.ai really answers, trimmed to the fields read here
    let listing = r#"{"data":[{"id":"mercury-2.5","context_length":260000,
        "supported_sampling_parameters":["temperature","stop"],
        "supported_features":["tools","json_mode","structured_outputs"]}]}"#;
    serving(listing, None).await
}

#[tokio::test]
async fn params_lists_what_the_endpoint_publishes_beside_each_parameter() {
    // trimmed from what openrouter.ai answers: most defaults published as `null`, which is a
    // default it does not know
    let listing = r#"{"data":[{"id":"mercury-2.5","context_length":262144,
        "top_provider":{"context_length":262144,"max_completion_tokens":32768},
        "supported_parameters":["max_tokens","seed","temperature","top_p"],
        "default_parameters":{"temperature":0.3,"top_p":null}}]}"#;
    let mut harness = Harness::served_by([], serving(listing, None).await);

    harness.send("/params").await;
    let screen = harness.screen();
    assert!(
        screen.contains("max_tokens   at most 32768")
            && screen.contains("temperature  default 0.3"),
        "each published fact is beside the parameter it is about: {screen}"
    );
    assert!(
        !screen.contains("null"),
        "and a default published as `null` is not one: {screen}"
    );

    // a bound is said of one that is set as well, which is where it can be crossed
    harness.send("/params max_tokens 100000").await;
    let screen = harness.screen();
    assert!(
        screen.contains("max_tokens is 100000, and mercury-2.5 publishes at most 32768"),
        "{screen}"
    );
    harness.send("/params max_tokens 1000").await;
    assert!(
        !harness.screen().contains("max_tokens is 1000,"),
        "and one under it is not remarked on: {}",
        harness.screen()
    );
}

/// Under `--gemini` the figures the listing publishes are about fields of `generationConfig`, and
/// `/params` names them by their path into it, sets one without taking its neighbours away, and
/// reads a bound inside one that was set whole.
#[tokio::test]
async fn params_reads_inside_a_parameter_where_the_listing_names_fields_inside_it() {
    // what `GET /models/gemini-3.5-flash-lite` really answers, trimmed to the fields read here
    let entry = r#"{"name":"models/gemini-3.5-flash-lite","inputTokenLimit":1048576,
        "outputTokenLimit":65536,"temperature":1,"topP":0.95,"topK":64,"maxTemperature":2,
        "thinking":true}"#;
    let provider = Arc::new(nachalnik_providers::Gemini::new(
        "gemini-3.5-flash-lite",
        listening(entry, None).await,
        "no key needed",
    ));
    provider.probe().await;
    let mut harness = Harness::served_by([], provider);

    harness.send("/params").await;
    let screen = harness.screen();
    assert_eq!(
        listed(&screen, "also takes, of the ones it publishes:"),
        [
            "generationConfig.maxOutputTokens  at most 65536",
            "generationConfig.temperature      default 1, at most 2",
            "generationConfig.topP             default 0.95",
            "generationConfig.topK             default 64",
        ],
        "{screen}"
    );

    // one set whole is read field by field, and the field the listing never names is the one
    // nothing is known about
    harness
        .send(r#"/params generationConfig {"thinkingConfig":{"thinkingBudget":1024}}"#)
        .await;
    let screen = harness.screen();
    assert!(screen.contains("sent, and unchecked"), "{screen}");
    assert!(
        screen.contains("generationConfig.thinkingConfig"),
        "{screen}"
    );

    // and a path sets the one field, beside what was already there
    harness.send("/params generationConfig.temperature 3").await;
    assert_eq!(
        serde_json::Value::Object(harness.app.kernel.params()),
        json!({ "generationConfig": {
            "thinkingConfig": { "thinkingBudget": 1024 },
            "temperature": 3,
        } })
    );
    let screen = harness.screen();
    assert!(
        screen.contains("generationConfig.temperature is 3, and gemini-3.5-flash-lite"),
        "a bound is said of a field inside a parameter: {screen}"
    );
    assert!(
        !listed(&screen, "also takes, of the ones it publishes:")
            .iter()
            .any(|row| row.starts_with("generationConfig.temperature")),
        "and one set is no longer on offer: {screen}"
    );

    // taking both away takes `generationConfig` with them, rather than sending it empty
    harness
        .send("/params generationConfig.thinkingConfig null")
        .await;
    harness
        .send("/params generationConfig.temperature null")
        .await;
    assert!(
        harness.app.kernel.params().is_empty(),
        "{:?}",
        harness.app.kernel.params()
    );

    // a path with nothing between two of its dots names nothing, and one into a field built from
    // the session is refused as that field is
    harness.send("/params generationConfig..topK 1").await;
    assert!(
        harness.screen().contains("is not a parameter"),
        "{}",
        harness.screen()
    );
    harness.send("/params contents.role 1").await;
    assert!(
        harness.screen().contains("built from the session"),
        "{}",
        harness.screen()
    );
    assert!(harness.app.kernel.params().is_empty());
}

/// `stream` is not a parameter a model lists, and is not ignored for being missing from the list.
#[tokio::test]
async fn a_parameter_of_the_transport_is_not_called_ignored() {
    let listing = r#"{"data":[{"id":"mercury-2.5","context_length":260000,
        "supported_parameters":["temperature","max_tokens"]}]}"#;
    let mut harness = Harness::served_by([], serving(listing, None).await);

    harness.send("/params stream false").await;
    let screen = harness.screen();
    assert!(!screen.contains("sent, and ignored"), "{screen}");

    // and one the model really does not list still is
    harness.send("/params seed 7").await;
    assert!(
        harness.screen().contains("does not list seed"),
        "{}",
        harness.screen()
    );
}

#[tokio::test]
async fn a_sampling_only_listing_does_not_claim_a_parameter_missing_from_it_is_ignored() {
    // the case that prompted this: `reasoning_effort` is absent from `mercury-2.5`'s published
    // list and is read all the same - validated hard enough that a bad value comes back a 400.
    // Calling it ignored would be a restriction invented out of a list that never claimed to be
    // complete, which is the one thing this program must not do with somebody else's metadata
    let mut harness = Harness::served_by([], sampling_only().await);

    harness.send(r#"/params reasoning_effort "high""#).await;
    let screen = harness.screen();
    assert!(
        !screen.contains("sent, and ignored"),
        "nothing here settles what becomes of it: {screen}"
    );
    assert!(
        screen.contains("sampling parameters only") && screen.contains("sent, and unchecked"),
        "and what is not known is said: {screen}"
    );
    assert!(
        screen.contains("also takes, of the ones it publishes"),
        "the list beside it is as partial as the list it came from: {screen}"
    );

    // one that *is* on the list is settled either way, so it draws no line of its own and drops
    // out of what is left to try
    harness.send("/params temperature 0.2").await;
    let screen = harness.screen();
    assert!(
        !screen.contains("what becomes of temperature"),
        "a listed parameter is not remarked on: {screen}"
    );
    assert_eq!(
        listed(&screen, "also takes, of the ones it publishes:"),
        ["stop"],
        "and it is no longer on offer: {screen}"
    );
}

/// Serves a listing, and refuses every request that follows in the shape a validated endpoint
/// refuses one: a list of what was wrong with it rather than a sentence about it.
async fn refusing() -> Arc<OpenAiCompatible> {
    let listing = r#"{"data":[{"id":"mercury-2.5","context_length":260000,
        "supported_sampling_parameters":["temperature","stop"]}]}"#;
    // quoted from what api.inceptionlabs.ai really answers `reasoning_effort: "banana"`
    let refusal = concat!(
        r#"{"error":{"message":[{"type":"value_error","loc":["body","reasoning_effort"],"#,
        r#""msg":"Value error, reasoning_effort must be one of: 'instant', 'low', "#,
        r#"'medium', 'high'","input":"banana","ctx":{"error":"reasoning_effort must be "#,
        r#"one of: 'instant', 'low', 'medium', 'high'"}}],"#,
        r#""type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
    );
    serving(listing, Some(refusal)).await
}

#[tokio::test]
async fn a_refused_request_says_what_was_wrong_with_it_once() {
    let mut harness = Harness::served_by([], refusing().await);

    harness.send("Say the single word: ready.").await;
    harness.settle().await;
    let screen = harness.screen();

    // the sentence the server wrote, which is the thing a person can act on
    assert!(
        screen.contains("reasoning_effort must be one of"),
        "the refusal says what was wrong with the request: {screen}"
    );
    // and not the machinery around it. A validated endpoint answers with a *list* of failures
    // rather than a sentence, and reading only a sentence left three hundred characters of
    // envelope - clipped mid-key - in the transcript and in the session log
    for envelope in ["\"loc\"", "value_error", "\"ctx\"", "invalid_request_error"] {
        assert!(
            !screen.contains(envelope),
            "{envelope} is the envelope, not the news: {screen}"
        );
    }

    // once. One failure arrives twice - as the event and as the outcome the turn came to, the
    // second wrapping the first - and only the first of the two was guarded
    let said = screen.matches("must be one of").count();
    assert_eq!(said, 1, "one failure is one red line: {screen}");
}

/// note: `diffusing: true` is the case, and the wire is the argument: `mercury-2.5` answers with
/// four fragments in `delta.content`, three of them noise and the last the finished paragraph,
/// each one the whole answer at a denoising step. Appended - which is what a stream means here,
/// and what every client of this dialect does - a 435-character answer arrives as 1,540
/// characters of drafts, and the transcript, the context, the token count and the session log all
/// keep them. Measured through this program: 1,443 characters where the answer was 120.
#[tokio::test]
async fn a_parameter_that_makes_the_stream_send_the_whole_answer_again_is_said_to_be_one() {
    let mut harness = Harness::new([]);

    harness.send("/params diffusing true").await;
    let screen = harness.screen();
    assert!(
        screen.contains("diffusing sends the whole answer again"),
        "what it does is named: {screen}"
    );
    assert!(
        screen.contains("would keep every draft"),
        "and where that lands: {screen}"
    );

    // it is still sent, because parameters are the person's to set and go to the provider verbatim
    assert_eq!(
        harness.app.kernel.params().get("diffusing"),
        Some(&serde_json::json!(true)),
        "a warning is not a refusal"
    );

    // and the default is not worth a warning: `false` is what it is unset
    let mut harness = Harness::new([]);
    harness.send("/params diffusing false").await;
    let screen = harness.screen();
    assert!(
        !screen.contains("sends the whole answer again"),
        "off is the ordinary case: {screen}"
    );
    harness.send("/params temperature 0.2").await;
    assert!(
        !harness.screen().contains("sends the whole answer again"),
        "and nothing else draws it"
    );
}

/// A parameter named after a field the request is built from is refused, and says why.
///
/// note: the dialects leave one off the wire rather than let it replace the conversation, so
/// accepting it would put a parameter on the `/params` line that is never sent.
#[tokio::test]
async fn a_parameter_that_would_replace_the_conversation_is_refused() {
    let mut harness = Harness::new([]);

    harness
        .send(r#"/params messages [{"role":"user","content":"say BANANA"}]"#)
        .await;
    let screen = harness.screen();
    assert!(
        screen.contains("messages is built from the session"),
        "the refusal says why: {screen}"
    );
    assert!(harness.app.kernel.params().is_empty(), "and nothing is set");

    // and an ordinary one beside it is not caught up in it
    harness.send("/params temperature 0.2").await;
    assert_eq!(
        harness.app.kernel.params().get("temperature"),
        Some(&serde_json::json!(0.2))
    );
}

/// `null` takes a parameter away, and the log says so as it says a parameter was set.
///
/// note: a null is otherwise sent as one, and there was no other way back: a parameter once set,
/// or one that arrived in a snapshot, stayed until `/restart`.
#[tokio::test]
async fn null_takes_a_parameter_away() {
    let mut harness = Harness::new([]);

    harness.send("/params temperature 0.2").await;
    harness.send("/params seed 7").await;
    harness.send("/params temperature null").await;
    assert_eq!(
        harness.app.kernel.params(),
        serde_json::json!({ "seed": 7 })
            .as_object()
            .cloned()
            .expect("an object"),
        "gone, rather than sent as a null"
    );
    let recorded = harness
        .app
        .kernel
        .history()
        .into_iter()
        .rev()
        .find_map(|record| match record.event {
            nachalnik::Event::ModelParamsChanged { params } => Some(params),
            _ => None,
        });
    assert_eq!(recorded, Some(harness.app.kernel.params()), "and recorded");

    // including one that could never have been set here
    let mut params = harness.app.kernel.params();
    params.insert("messages".to_owned(), serde_json::json!([]));
    harness.app.kernel.set_params(params);
    harness.send("/params messages null").await;
    assert!(!harness.app.kernel.params().contains_key("messages"));
}

#[tokio::test]
async fn an_endpoint_that_publishes_no_parameters_is_not_read_as_forbidding_them() {
    // ollama and a bare OpenAI-compatible proxy both say nothing about parameters. Silence is
    // not a prohibition, and a warning invented out of it would be worse than no warning
    let mut harness = Harness::new([]);

    harness.send("/params seed 42").await;
    let screen = harness.screen();
    assert!(screen.contains("parameters:"), "{screen}");
    assert!(
        !screen.contains("does not list"),
        "nothing is claimed about a model that said nothing: {screen}"
    );
    assert!(!screen.contains("also takes"), "{screen}");
}

/// The figure in the corner starts from what the provider charged and estimates only what has
/// changed since.
///
/// note: the whole point of the arithmetic. A counter without the model's tokenizer is out by a
/// few percent of everything it is asked about, so an estimate of a large context is out by a
/// lot of tokens even when it is out by very little as a percentage - and "does the next
/// message fit" is exactly the question that figure is read for. Anchored, an item that has not
/// moved contributes what the provider *measured* and no error at all.
#[tokio::test]
async fn the_next_request_is_reckoned_from_what_the_last_one_really_cost() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(9_000),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    // a context the counter is going to be wrong about, and a provider that says so: the
    // estimate will be nowhere near 9,000
    harness.app.kernel.push(ContextItem::file(
        "haystack.txt",
        "a needle in it. ".repeat(200),
    ));

    harness.send("go").await;
    harness.settle().await;

    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    let estimate = budget.used();
    let anchored = harness
        .app
        .anchored(&going, &budget)
        .expect("a response reported what it cost");

    // the request carried the file and the question; what is in the context that was not in it
    // is the answer, and that is the only thing the anchored figure has to estimate
    let answered = harness.app.kernel.items().last().unwrap().clone();
    assert_eq!(
        anchored,
        9_000 + going.costs.get(&answered.id).copied().unwrap_or(0),
        "the provider's own figure for what it answered, plus what came after it"
    );

    // and the from-scratch estimate is a different number, which is the reason for any of this.
    // `Calibrating` has pulled it towards the truth - one observation, so its scale is fitted to
    // exactly this request - and it still lands 10% under, because that scale is a single
    // multiplier standing in for per-message framing it cannot see
    assert!(estimate < 9_000, "the counter is low, as it is: {estimate}");
    assert!(
        anchored - estimate > 500,
        "and the two are far enough apart to change what somebody does: {estimate} vs {anchored}"
    );

    // the corner reads the anchored figure, so it says thousands where the estimate says
    // hundreds - which is the difference somebody would actually have acted on
    let screen = harness.screen();
    assert!(
        screen.contains("~9,"),
        "the status line should show the anchored figure, not the estimate: {screen}"
    );
}

/// Taking something out of the request takes it off the anchored figure too, by what it was
/// holding rather than by what it costs as a marker.
#[tokio::test]
async fn what_stops_being_sent_comes_back_off_the_anchored_figure() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(9_000),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    let file = harness.app.kernel.push(ContextItem::file(
        "haystack.txt",
        "a needle in it. ".repeat(200),
    ));

    harness.send("go").await;
    harness.settle().await;
    let before = harness
        .app
        .anchored(&harness.app.going(), &harness.app.kernel.budget())
        .expect("anchored");

    // excluded: it was in the request the 9,000 was charged for, so it has to come out of it
    harness
        .app
        .kernel
        .set_state([file], ContextState::Excluded, Some("by hand".into()));
    let after = harness
        .app
        .anchored(&harness.app.going(), &harness.app.kernel.budget())
        .expect("anchored");

    let held = harness.app.kernel.item(file).unwrap().tokens;
    assert_eq!(
        before - after,
        held,
        "what came off should be what the item holds, not nothing and not the whole context"
    );
}

/// An item elided since the request it was in comes off the anchored figure by what it holds.
///
/// note: the third of [`App::valued`]'s three answers, and the one the other two cannot reach. An
/// elided item is still in the request as a marker, so the subtraction has something to take out -
/// the marker's dozen - and taking that is what leaves a context of eight thousand tokens
/// describing the next request as what the provider once charged for all of it. The figure it was
/// charged for had the whole of the file in it, so the whole of what the file holds is what comes
/// back out; the marker is counted on the other side, as part of the request that stands for it.
#[tokio::test]
async fn an_item_elided_since_the_request_it_was_in_comes_off_by_what_it_held() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(9_000),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    // four times the provider's own figure, so the difference between the two answers cannot be
    // an artefact of what a marker costs
    let file = harness
        .app
        .kernel
        .push(ContextItem::file("haystack.txt", "a needle in it. ".repeat(2_000)).pinned());

    harness.send("go").await;
    harness.settle().await;
    let before = harness
        .app
        .anchored(&harness.app.going(), &harness.app.kernel.budget())
        .expect("anchored");
    assert!(
        before > 8_500,
        "the request was charged for the whole of that file: {before}"
    );

    harness
        .app
        .kernel
        .set_state([file], ContextState::Elided, Some("not any more".into()));
    let after = harness
        .app
        .anchored(&harness.app.going(), &harness.app.kernel.budget())
        .expect("anchored");

    assert!(
        after < 3_000,
        "the figure is still carrying a request whose content is not going out: {before} -> {after}"
    );
}

/// A counter that has learned nothing says so in a sentence, rather than in a table of zeroes.
///
/// note: the honest figure is `0 observations, a scale of 1.000`, and every one of those is a
/// number somebody would have to know the meaning of to read. What the sentence has to say is why
/// the scale is what it is: four bytes a token, and no request big enough to argue with it.
#[tokio::test]
async fn the_budget_says_the_counter_has_learned_nothing_yet() {
    // nothing has been sent, so nothing could have taught the counter anything
    let mut harness = Harness::new([]);
    assert_eq!(
        harness
            .app
            .kernel
            .counter()
            .calibration()
            .map(|c| c.observations),
        Some(0),
        "the counter this ships with learns, and has been told nothing"
    );

    let panel = budget_of(&mut harness).await;
    assert!(
        panel.contains("the counter has not been corrected yet"),
        "a scale of 1.0 is a guess, and this is where it says so: {panel}"
    );
    assert!(
        !panel.contains("learned from 0 request"),
        "a count of nothing is not what it has learned from: {panel}"
    );
}

/// What `/budget` answered with, as one string.
///
/// note: the page rather than the screen, because a page is what a caller down a pipe or over a
/// socket reads and a screen is what a person at a desk reads - and the keys that close a panel
/// mean a test that reads the screen cannot press them afterwards.
async fn budget_of(harness: &mut Harness) -> String {
    let kamchatka::app::Overlay::Text { pages, .. } = harness
        .app
        .submit("/budget")
        .await
        .page
        .expect("`/budget` answered with a page");

    pages
        .iter()
        .map(|page| page.body.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A message being typed is in the corner before it is anywhere else.
///
/// note: it is not context and never will be until it is sent, so nothing in the runtime can
/// answer for it - and it is the one number somebody actually wants while deciding whether to
/// press enter. Worth showing only because the figure it lands on is anchored: a draft moving a
/// total that is itself a thousand tokens uncertain would be precision theatre.
#[tokio::test]
async fn a_message_being_typed_is_counted_before_it_is_sent() {
    let mut harness = Harness::new([]);

    assert_eq!(harness.app.drafted(), 0, "nothing typed, nothing counted");

    for c in "here is a question of some length".chars() {
        harness.press(KeyCode::Char(c)).await;
    }
    let drafted = harness.app.drafted();
    assert!(drafted > 0, "a typed message costs something");

    let screen = harness.screen();
    assert!(
        screen.contains(&format!("{drafted} of it typed")),
        "the corner should say how much of the figure is not sent yet: {screen}"
    );

    // and `/budget` names what the corner is counting that the context is not, rather than
    // leaving the reader to take a half-written message for part of the request
    let panel = budget_of(&mut harness).await;
    assert!(
        panel.contains(&format!("{drafted} tokens of message typed but not sent")),
        "the panel should say what the corner is counting that the context is not: {panel}"
    );

    // a slash command is not a message and is not going into any request. The panel is closed
    // first, since it takes the first key as the gesture that closes it and that key would
    // otherwise be one backspace the prompt never sees
    harness.press(KeyCode::Esc).await;
    for _ in 0.."here is a question of some length".len() {
        harness.press(KeyCode::Backspace).await;
    }
    harness.press(KeyCode::Char('/')).await;
    harness.press(KeyCode::Char('b')).await;
    assert_eq!(harness.app.drafted(), 0, "a command is not a message");
}

/// A request that failed does not leave its item set to be paired with somebody else's figure.
///
/// note: the anchor is assembled from two events - `ModelRequested` names what went out,
/// `ModelFinished` says what it cost - and between them is every way a request can go wrong. If
/// a failure left the first half standing, the next successful response would be paired with
/// the *failed* request's item set, and the budget would subtract items that figure never
/// covered. It cannot happen, because `ModelRequested` fires before every attempt and
/// overwrites what is pending; this is the test that says so, since "cannot happen" is a claim
/// about ordering and nothing else here checks it.
#[tokio::test]
async fn a_failed_request_does_not_leave_an_anchor_behind() {
    use nachalnik::{ContextId, Delta, Event, StopReason};

    let mut harness = Harness::new([]);
    let asked = harness
        .app
        .kernel
        .push(ContextItem::user("the first question"));

    // a request that went out naming one item, and then failed
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 9,
        items: vec![asked],
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    harness.app.on_event(Event::ModelFailed {
        error: "502 from somewhere".to_owned(),
        overrun: None,
    });
    assert!(
        harness.app.anchor.is_none(),
        "a request that failed reported no cost, so there is nothing to anchor on"
    );
    assert!(
        !harness.flat().contains("the model read that request as"),
        "and a failure carrying no measurement says nothing about one"
    );

    // a second request, naming a different item, which succeeds
    let more = harness
        .app
        .kernel
        .push(ContextItem::user("the second question"));
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 2,
        tools: 0,
        tokens: 18,
        items: vec![asked, more],
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("an answer".to_owned()),
    });
    let answered = harness
        .app
        .kernel
        .push(ContextItem::assistant("an answer", vec![]));
    harness.app.on_event(Event::ModelFinished {
        item: answered,
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Some(Usage {
            input_tokens: Some(500),
            ..Default::default()
        }),
    });

    let anchor = harness
        .app
        .anchor
        .as_ref()
        .expect("the second one reported");
    assert_eq!(anchor.reported, 500);
    assert_eq!(
        anchor.sent,
        vec![asked, more],
        "the 500 covers what the *second* request carried, not the failed one's"
    );
    let _: ContextId = asked;
}

/// A request the model refused for its length says what that length was, and how much of it has
/// to go.
///
/// note: the sentence the server sent is the server's, and every vendor writes those two numbers
/// in a different order - so the screen says what they mean here rather than leaving somebody to
/// work it out. It matters because the only *other* figure in front of them is the corner, which
/// is the estimate that has just turned out to be wrong.
#[tokio::test]
async fn a_request_refused_for_its_length_says_how_much_of_it_has_to_go() {
    use nachalnik::{Event, Overrun};

    let mut harness = Harness::new([]);
    harness.app.on_event(Event::ModelFailed {
        error: "400 Bad Request: The request is 286315 tokens long and exceeds this model's \
                context length of 262144 tokens."
            .to_owned(),
        overrun: Some(Overrun {
            tokens: 286_315,
            limit: Some(262_144),
        }),
    });

    let packed = harness.packed();
    assert!(packed.contains("286,315"), "what it was: {packed}");
    assert!(
        packed.contains("24,171"),
        "and what it is over by, which is the number to act on: {packed}"
    );
}

/// A request refused here says the figure is this counter's, because nothing has read it.
///
/// note: the two refusals carry the same field and mean different things, and the sentence is
/// where the difference has to live. An endpoint's is the model's own tokenizer reporting on a
/// request it read; this one is an estimate of a request nobody has seen, made by the counter
/// whose being wrong is the reason any of this exists. Saying "the model read that request as"
/// a number the model was never shown is a confident wrong sentence, and the alternative costs
/// one word.
#[tokio::test]
async fn a_request_refused_here_does_not_claim_the_model_read_it() {
    use nachalnik::{Event, Overrun};

    let mut harness = Harness::new([]);
    harness.app.on_event(Event::StepFailed {
        error: "the request is about 71231 tokens and the model takes 65536, so it was not sent"
            .to_owned(),
        overrun: Some(Overrun {
            tokens: 71_231,
            limit: Some(65_536),
        }),
    });

    let packed = harness.packed();
    assert!(
        packed.contains("5,695"),
        "how much has to go, which is the number to act on: {packed}"
    );
    assert!(
        !packed.contains("the model read"),
        "and nothing read it: {packed}"
    );
    assert!(
        packed.contains("estimate"),
        "so the sentence says whose figure it is: {packed}"
    );
}

/// An item elided *before* a request does not come back out of what that request cost.
///
/// note: reported from a real session, and the figure was not slightly wrong. Attach a
/// 12,278-token file, elide it, ask one more question, and the corner reads `~0 tokens` for the
/// rest of the session - a context of twelve thousand tokens describing itself as empty, which
/// is the one direction this number must never be wrong in.
///
/// note: the cause is that an elided item is still *in* the request, as a marker. The anchor
/// recorded every item the request was built from and then took the whole of what each one
/// **holds** back out of the provider's figure - so an item that contributed a line of text had
/// twelve thousand tokens subtracted for it, and the subtraction ran away with the total.
#[tokio::test]
async fn an_item_already_elided_is_not_subtracted_as_though_it_had_been_sent() {
    let mut harness = Harness::new([
        ModelResponse {
            usage: Some(Usage {
                input_tokens: Some(12_800),
                ..Default::default()
            }),
            ..ModelResponse::text("first")
        },
        ModelResponse {
            usage: Some(Usage {
                input_tokens: Some(1_485),
                ..Default::default()
            }),
            ..ModelResponse::text("second")
        },
    ]);
    let big = harness
        .app
        .kernel
        .push(ContextItem::file("AGENTS.md", "a file. ".repeat(6_000)).pinned());

    // one request with the file in it, then the file is elided, then another without it
    harness.send("is this a clear AGENTS file?").await;
    harness.settle().await;
    harness.app.kernel.set_state(
        [big],
        ContextState::Elided,
        Some("removed from view by the user".into()),
    );
    harness.send("and now?").await;
    harness.settle().await;

    // the anchor is the *second* request: the file was a marker in it, so it is not among the
    // items whose content that 1,485 covered
    let anchor = harness.app.anchor.as_ref().expect("a reported request");
    assert_eq!(anchor.reported, 1_485);
    assert!(
        !anchor.sent.contains(&big),
        "an elided item's content was not in that request: {:?}",
        anchor.sent
    );
    assert!(
        anchor.markers > 0,
        "it was in it as a marker, which costs something"
    );

    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    let anchored = harness.app.anchored(&going, &budget).expect("anchored");
    assert!(
        anchored >= 1_485,
        "the context has not shrunk since that request, so the figure cannot be below it: \
         {anchored}"
    );
    // and the screen says a number rather than nothing
    let screen = harness.flat();
    assert!(
        !screen.contains("~0 tokens"),
        "a context of twelve thousand tokens is not empty: {screen}"
    );

    // putting it back puts its cost back, which is the other half of the same arithmetic
    harness
        .app
        .kernel
        .set_state([big], ContextState::Active, None);
    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    let restored = harness.app.anchored(&going, &budget).expect("anchored");
    assert!(
        restored > anchored + 10_000,
        "restoring a twelve-thousand-token file should show up: {anchored} -> {restored}"
    );
}

/// Switching model drops the anchor, because it was the other model's arithmetic.
///
/// note: `App::anchored` has always documented a fallback "after a change of model until the next
/// response", and nothing implemented it - the anchor was set on a response and never cleared by
/// anything. Found by switching models against a real endpoint and watching the corner not move.
/// What it costs is one request's worth of confidently wrong: the previous model's reported figure
/// is that model's tokenizer counting that model's framing, and applied to another it is a number
/// with no relationship to what is about to be charged.
#[tokio::test]
async fn a_change_of_model_drops_the_anchor() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(900),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.md", "a sentence. ".repeat(200)));

    harness.send("go").await;
    harness.settle().await;

    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    assert!(
        harness.app.anchored(&going, &budget).is_some(),
        "a response reported a figure, so there is something to anchor on"
    );

    harness.send("/model something-else").await;
    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    assert_eq!(
        harness.app.anchored(&going, &budget),
        None,
        "the anchor belonged to the model that was answered by, and that model is gone"
    );
    // and the corner falls back to the plain estimate rather than showing nothing
    let screen = harness.flat();
    assert!(
        screen.contains("~") && screen.contains("tokens"),
        "the corner should fall back to the plain estimate rather than showing nothing: {screen}"
    );
}

/// A provider's figure wider than half of `i64` comes back as the figure it reported, rather than
/// as one that has wrapped.
///
/// note: the sum behind the anchored figure is the provider's own number plus what the context
/// estimates now, less what that number already covered, and it was done in `i64` - so a
/// `prompt_tokens` of `u64::MAX` went through the cast as a negative and the answer was read off
/// as a number far below zero. Saturating arithmetic is the honest way to add three numbers one of
/// which is whatever an endpoint typed into a JSON field.
#[tokio::test]
async fn a_provider_figure_wider_than_half_an_i64_does_not_wrap_the_next_request() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(u64::MAX),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.md", "a sentence. ".repeat(200)));

    harness.send("go").await;
    harness.settle().await;

    let going = harness.app.going();
    let budget = harness.app.kernel.budget();
    let anchored = harness
        .app
        .anchored(&going, &budget)
        .expect("a response reported what it cost");

    // the provider's figure, less what that figure already covered - so within a few hundred of
    // the largest a token count can be, and a long way from where a signed cast would have put it
    assert!(
        anchored > usize::MAX - 1_000,
        "the anchored figure is the provider's own number, not one that has wrapped: {anchored}"
    );

    // the corner prints it rather than a number that has come back through the cast. The line is
    // as wide as the window, so what is checked is the head of the figure - which is where a wrap
    // would be least visible and a real one is unmistakable
    let screen = harness.screen();
    assert!(
        screen.contains("~18,446,744,073,709,551,"),
        "the status line should show the figure that was reported: {screen}"
    );
}

/// And the correction with it, for the same reason one word further in.
///
/// note: `Calibrating` is the ratio between what this counter guessed and what a provider billed,
/// and it is cumulative - so a scale learnt from one tokenizer goes on correcting the next one's
/// figures, and the new model's own observations are averaged into the old one's totals instead of
/// replacing them. `Calibrating::reset` is documented as being for exactly this and had no caller
/// anywhere in the workspace; a live session read a scale of 1.152 off one model, switched, and
/// settled at 1.017, which is neither model's number.
#[tokio::test]
async fn a_change_of_model_drops_the_correction_too() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(1_300),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.md", "a sentence. ".repeat(200)));

    harness.send("go").await;
    harness.settle().await;

    let learned = harness
        .app
        .kernel
        .counter()
        .calibration()
        .expect("the counter this ships with learns");
    assert_eq!(learned.observations, 1);
    assert_ne!(learned.scale, 1.0, "and it learnt something from that one");

    harness.send("/model something-else").await;

    assert_eq!(
        harness.app.kernel.counter().calibration(),
        Some(nachalnik::Calibration::default()),
        "what it learnt was about the model that is gone"
    );
}

/// The line after a switch is read by the session the switch produced, not the one it replaced.
///
/// note: `/model` and `/endpoint` hand the switch to a task, because finding out what the new
/// model holds and whether the new address serves it is two round trips and a screen should not
/// stop for them. Nothing was waiting for that task, so `/endpoint URL ID` followed by `/model`
/// answered with the old model - watched live, twice, against two different endpoints - and a
/// *message* on the next line could be asked of whichever of the two won the race. At a keyboard
/// it is a race that usually resolves in the gap before somebody types; down a pipe there is no
/// gap at all, so the ordinary case in a script is the one that loses.
#[tokio::test]
async fn a_line_after_a_switch_waits_for_the_switch() {
    let mut harness = Harness::new([]);

    harness.send("/model something-else").await;
    harness.send("/model").await;
    // the second line waited for the switch, and is handed in when it has come back
    harness.settle().await;

    let screen = harness.flat();
    assert!(
        screen.contains("something-else"),
        "the model the last line asked for: {screen}"
    );
    assert!(
        harness.app.settling.is_none(),
        "and nothing is still settling once the line that waited for it has run"
    );
}

/// A switch typed into a running turn waits for the end of it, and what is typed after the switch
/// waits behind it.
///
/// note: it took effect there and then, so the rest of the turn went to a model it had not
/// started with, and the counter's calibration was reset under a request in flight.
#[tokio::test]
async fn a_switch_typed_into_a_turn_waits_for_the_end_of_it() {
    let mut harness = Harness::new([
        ModelResponse::text("the first answer"),
        ModelResponse::text("the second answer"),
    ]);

    harness.send("go").await;
    assert!(harness.app.busy, "the turn is running");
    harness.send("/model something-else").await;
    harness.send("and after it").await;
    // which the loops do on every pass, the turn's included
    harness.app.release().await;

    let screen = harness.flat();
    assert!(
        screen.contains("this switches when the turn ends"),
        "{screen}"
    );
    assert!(
        harness.app.settling.is_none(),
        "nothing was switched under the turn"
    );
    assert_ne!(harness.app.provider.model(), "something-else");

    // the turn, then the switch it held, then the line that waited behind the switch
    for _ in 0..3 {
        harness.settle().await;
    }
    assert_eq!(harness.app.provider.model(), "something-else");
    let items: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    let answered = items
        .iter()
        .position(|text| text.contains("the first answer"))
        .expect("the turn the switch waited for");
    let after = items
        .iter()
        .position(|text| text.contains("and after it"))
        .expect("the line that waited behind the switch went in");
    assert!(answered < after, "{items:?}");
}

/// Two switches typed into one turn are made in the order they were typed, so the later one stands.
#[tokio::test]
async fn two_switches_typed_into_a_turn_are_made_in_order() {
    let mut harness = Harness::new([ModelResponse::text("the answer")]);

    harness.send("go").await;
    harness.send("/model the-first").await;
    harness.send("/model the-second").await;

    // the turn, then each switch coming back
    for _ in 0..3 {
        harness.settle().await;
    }
    assert_eq!(harness.app.provider.model(), "the-second");
}

/// The context tab says how much has to go, because that is the tab somebody goes to in order to
/// make it go.
///
/// note: the corner turns red past the limit and says the compactor runs first, and it has no
/// room for the figure that decides what to do next. The difference between "over" and "over by
/// two thousand" is the difference between reading forty rows and taking one of them out - a
/// session was watched five exclusions deep and still 2,814 over, with nothing on the screen
/// saying how close it was.
#[tokio::test]
async fn the_context_tab_says_how_far_over_the_limit_the_request_is() {
    let mut harness = Harness::new([]);
    let limit = harness.app.kernel.budget().limit.expect("a limit");
    harness.app.kernel.push(ContextItem::file(
        "big.txt",
        "a line of routine diagnostic output. ".repeat(limit),
    ));
    harness.drain();
    harness.tab(Tab::Context);

    let over = harness.app.kernel.budget().used() - limit;
    let packed = harness.packed();
    // the same spelling the rest of the screen uses for a figure this size
    let written = grouped(over);
    assert!(packed.contains(&written), "over by {over}: {packed}");
    assert!(packed.contains("overthelimit"), "{packed}");
}

/// Under it, there is no such figure, because there is nothing to act on.
#[tokio::test]
async fn the_context_tab_says_nothing_about_a_request_that_fits() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("a short question"));
    harness.drain();
    harness.tab(Tab::Context);

    assert!(!harness.packed().contains("overthelimit"));
}

/// And it counts one item as one item.
///
/// note: small, and the reason it is worth a test is that it is small. A status line reading
/// `1 items` is the corner of a screen quietly saying it is not looking; somebody who notices it
/// has no way to tell whether the number beside it is approximate too. Every other count on that
/// line agrees with its noun because they all go through the same place now.
#[tokio::test]
async fn the_context_tab_counts_one_item_as_one_item() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the only thing in here"));
    harness.drain();
    harness.tab(Tab::Context);

    let packed = harness.packed();
    assert!(packed.contains("1item,"), "{packed}");
    assert!(!packed.contains("1items"), "{packed}");

    // and so does every other line that counts them for a person. The trace draws the same
    // sentences, so `1 items now` off an undo is the same wrongness one tab along
    harness.app.on_event(nachalnik::Event::ContextUndone {
        items: 1,
        removed: Vec::new(),
        changed: Vec::new(),
    });
    harness.app.on_event(nachalnik::Event::SessionResumed {
        session: "s".to_owned(),
        items: 1,
        tokens: 12_345,
    });
    harness.tab(Tab::Trace);

    let packed = harness.packed();
    assert!(packed.contains("1itemnow"), "{packed}");
    assert!(packed.contains("1item,~12,345tokens"), "{packed}");
}

/// A session that has not picked a model draws a placeholder where the name goes.
///
/// note: the corner is the only thing on the screen that says what this session is talking to, so
/// a session with nothing to talk to has to say that in the same place. Drawing one chunk fewer
/// reads as a corner that has not caught up yet, which is the one state nobody can act on - and
/// the address stays, because it is settled and it is what `/models` is about to list.
#[tokio::test]
async fn the_corner_names_the_gap_where_no_model_has_been_picked() {
    let mut harness = Harness::new([ModelResponse::text("never asked for")]);
    // what `Setup::wire` leaves behind for a session started without `-m`: an address and a key,
    // and nothing in the kernel to ask
    harness.app.kernel.clear_provider();

    let flat = harness.flat();
    assert!(flat.contains("no model @ 127.0.0.1:1"), "{flat}");
    // and it is not drawn in the colour everything else in that line is, because it is the one
    // thing there that is asking for something
    assert_eq!(
        harness.style_of("no model @").0,
        ratatui::style::Color::Yellow
    );
}

/// And nothing is sent while it says so.
///
/// note: the refusal is `App::start_turn`'s rather than a check at the prompt, which is what makes
/// it the answer to every way of starting a turn. The kernel refuses this too - it holds no
/// provider at all - and what the line adds is whose move it is.
#[tokio::test]
async fn a_message_typed_before_a_model_is_picked_is_not_sent() {
    let mut harness = Harness::new([ModelResponse::text("never asked for")]);
    harness.app.kernel.clear_provider();

    harness.send("what is 2+2").await;

    let flat = harness.flat();
    assert!(
        flat.contains("nothing is sent until there is a model"),
        "{flat}"
    );
    assert!(flat.contains("`/model ID` picks one"), "{flat}");
    // the line is kept rather than thrown away: it is in the context, and the model picked a
    // moment from now is asked it
    assert_eq!(
        harness.app.kernel.items()[0].content.to_text(),
        "what is 2+2"
    );
}

/// Nothing was spent on advice, and `/spend` says nothing about advice.
///
/// note: the split is a line of its own, said only when there is a figure to split. In a session
/// with no advisor the figure is nothing at all, and `0 of it the advisor's` would name an
/// advisor that was never asked and a share of the total that does not exist - on every `/spend`,
/// which is the line somebody reads to find out what a session has cost. The other side of it is
/// `tests/screen/advisor.rs`, which is where the figure is there to be split.
#[tokio::test]
async fn spend_does_not_mention_an_advisor_nothing_was_spent_on() {
    let mut harness = Harness::new([]);

    assert_eq!(
        harness.app.spent_on_advice(),
        0,
        "nothing was asked, so nothing was charged"
    );
    harness.send("/spend").await;

    let said = harness.flat();
    assert!(said.contains("this run has spent"), "{said}");
    assert!(
        !said.contains("the advisor's"),
        "there is no split to report: {said}"
    );

    // and none when a ceiling is set either, which is the other sentence this figure rides on
    harness.send("/spend 5000").await;
    assert!(
        !harness.flat().contains("the advisor's"),
        "{}",
        harness.flat()
    );
}
