//! The status line and the budget: what a request cost, and what this model takes.
//!
//! note: the numbers, and where they came from. An estimate is shown beside what a provider
//! actually charged because the counter in this runtime is honest about being an estimate, and
//! `/params` says what an endpoint publishes rather than what the program hopes it accepts -
//! these check that neither claims more than it knows.

use std::{sync::Arc, time::Duration};

use crossterm::event::KeyCode;
use nachalnik::{
    ContextItem, ContextState, ModelInfo, ModelResponse, Usage, test::ScriptedProvider,
};
use nachalnik_providers::OpenAiCompatible;

use crate::harness::Harness;

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

/// A provider that reports no reasoning is not a provider reporting none of it, and the line that
/// would say so must not appear for the ordinary case.
#[tokio::test]
async fn a_model_that_reports_no_reasoning_is_not_said_to_be_hiding_any() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(9),
            output_tokens: Some(30),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);

    harness.send("go").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.screen();
    assert!(screen.contains("generated 30 out"), "{screen}");
    assert!(
        !screen.contains("of it reasoning") && !screen.contains("does not send back"),
        "silence about reasoning is not a claim there was none: {screen}"
    );
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
    assert!(screen.contains("elided to a marker"), "{screen}");
}

#[tokio::test]
async fn the_status_line_says_what_the_budget_is_measured_against() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("hello"));

    let screen = harness.screen();
    let status = screen.lines().last().expect("a status line");

    assert!(status.contains("idle"), "{status}");
    assert!(status.contains("scripted"), "{status}");
    // the estimate, marked as one, and the limit it is a percentage *of* - a bare percentage is
    // not something anybody can act on
    assert!(status.contains("~"), "{status}");
    assert!(status.contains("% (128k)"), "{status}");
}

#[tokio::test]
async fn the_address_the_requests_go_to_is_visible_and_can_be_changed() {
    let mut harness = Harness::new([]);

    // where they are going, before anything is switched. A model name means a different model at a
    // different address, so a comparison that cannot see the address is a comparison of names
    harness.send("/provider").await;
    let screen = harness.screen();
    assert!(screen.contains("http://127.0.0.1:1"), "{screen}");

    harness
        .send("/provider http://127.0.0.1:2/v1 a-model-served-there")
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
/// note: `/model`, `/provider` and `/seams` have always named the address, but only when asked,
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
        .set_provider(Arc::new(ScriptedProvider::new([]).with_info(ModelInfo {
            parameters: vec!["temperature".to_owned(), "top_p".to_owned()],
            ..ModelInfo::new("scripted", "a/model")
        })));

    harness.send("/params seed 42").await;
    let screen = harness.screen();
    assert!(
        screen.contains("does not list seed"),
        "the one that will do nothing is named: {screen}"
    );
    assert!(
        screen.contains("also takes: temperature, top_p"),
        "and so is what it would have taken instead: {screen}"
    );

    // one that *is* listed draws no complaint, and drops out of what is left to try
    harness.send("/params temperature 0.2").await;
    let screen = harness.screen();
    assert!(
        screen.contains("also takes: top_p"),
        "a parameter already set is not still on offer: {screen}"
    );
    assert!(
        !screen.contains("does not list temperature"),
        "and it is not complained about: {screen}"
    );
}

/// Serves one model listing, in the shape of an endpoint that publishes its *sampling* parameters
/// only, and hands back a provider that has read it.
async fn sampling_only() -> Arc<OpenAiCompatible> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // quoted from what api.inceptionlabs.ai really answers, trimmed to the fields read here
        let body = r#"{"data":[{"id":"mercury-2.5","context_length":260000,
            "supported_sampling_parameters":["temperature","stop"],
            "supported_features":["tools","json_mode","structured_outputs"]}]}"#;
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 4096];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    let provider = Arc::new(OpenAiCompatible::new(
        "mercury-2.5",
        format!("http://{at}"),
        "no key needed",
    ));
    provider.probe().await;
    provider
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
    assert!(
        screen.contains("also takes, of the ones it publishes: stop"),
        "and it is no longer on offer: {screen}"
    );
}

/// Serves a listing, and refuses every request that follows in the shape a validated endpoint
/// refuses one: a list of what was wrong with it rather than a sentence about it.
async fn refusing() -> Arc<OpenAiCompatible> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut asked = [0u8; 4096];
            let read = socket.read(&mut asked).await.unwrap_or(0);
            let asked = String::from_utf8_lossy(&asked[..read]);
            let (status, body) = match asked.contains("/models") {
                true => ("200 OK", listing),
                false => ("400 Bad Request", refusal),
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

    let provider = Arc::new(OpenAiCompatible::new(
        "mercury-2.5",
        format!("http://{at}"),
        "no key needed",
    ));
    provider.probe().await;
    provider
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

    // a slash command is not a message and is not going into any request
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
    });
    assert!(
        harness.app.anchor.is_none(),
        "a request that failed reported no cost, so there is nothing to anchor on"
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
        anchor.items,
        vec![asked, more],
        "the 500 covers what the *second* request carried, not the failed one's"
    );
    let _: ContextId = asked;
}
