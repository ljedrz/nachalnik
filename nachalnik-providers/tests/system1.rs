//! What TypeSafe's `jev` makes of a question, over the real wire.
//!
//! They are skipped unless a key is in the environment:
//!
//! ```text
//! TYPESAFE_API_KEY=apikey_... cargo test -p nachalnik-providers --features system1 --test system1 -- --nocapture
//! ```
//!
//! note: live rather than over a socket serving a recorded body, which is what the unit tests in
//! the module already do. What cannot be checked without the real endpoint is the half that
//! matters most here: that the payload this crate renders is one the service *accepts*, for all
//! three question types, and that what comes back maps onto the three answer types. A shape
//! written from a specification and pinned by no live test is exactly what this crate does not
//! ship - see the `input_audio` note in `waiting.rs`.
//!
//! note: assertion-light about the numbers and assertion-heavy about structure, for the same
//! reason `nachalnik`'s own live suite is. `rm -rf /` really does come back `deny` at 0.99, and
//! asserting on that figure would be asserting on somebody else's model weights. What is
//! asserted is that a distribution sums to one, that a score lands inside its own rubric, and
//! that the three types come back as the three types.

#![cfg(feature = "system1")]

use std::env;

use nachalnik_providers::{
    Endpoint,
    system1::{Answer, DEFAULT_MODEL, Jev, Question},
};

/// Live tests take turns, so that a rate limit is never what is being measured.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A client for the live endpoint, or a skipped test.
macro_rules! live {
    () => {
        match env::var("TYPESAFE_API_KEY") {
            Ok(key) => Jev::latest(key),
            Err(_) => {
                eprintln!("skipped: set TYPESAFE_API_KEY to run the live tests");
                return;
            }
        }
    };
}

/// Whether a distribution is one: every option accounted for, and summing to one.
fn sums_to_one(probabilities: &std::collections::BTreeMap<String, f64>) -> bool {
    let total: f64 = probabilities.values().sum();

    (total - 1.0).abs() < 0.01
}

/// All three question types in one request, which is the way the documentation asks for them.
#[tokio::test]
async fn the_three_question_types_go_out_together_and_come_back_as_three_answers() {
    let _serial = SERIAL.lock().await;
    let jev = live!();

    let answers = jev
        .ask(
            "The agent wants to run `rm -rf /` through its shell tool.",
            [
                (
                    "destructive",
                    Question::noul("Does this destroy data irreversibly?")
                        .between("it deletes data that cannot be recovered", "it only reads"),
                ),
                (
                    "verdict",
                    Question::between_described(
                        "What should a permission gate do with this?",
                        [
                            ("allow", "harmless"),
                            ("ask", "a person should decide"),
                            ("deny", "never run this"),
                        ],
                    ),
                ),
                (
                    "blast",
                    Question::score(
                        "How far does this reach?",
                        ["nothing", "one file", "one project", "the whole machine"],
                    ),
                ),
            ],
        )
        .await
        .expect("the live endpoint answers");

    // one answer per question, under the name it was asked under
    assert_eq!(answers.answers.len(), 3, "{:?}", answers.answers.keys());

    // the version that answered, which is not the `jev-latest` that was asked for
    assert!(
        answers.model.starts_with("jev-"),
        "the model names itself: {}",
        answers.model
    );
    assert_ne!(
        answers.model, DEFAULT_MODEL,
        "an alias should resolve to a version on the way through"
    );

    // a noul is a probability, and nothing else is reported about it
    let noul = answers
        .noul("destructive")
        .expect("a noul came back a noul");
    assert!((0.0..=1.0).contains(&noul), "a probability: {noul}");
    assert_eq!(answers.confidence("destructive"), None);

    // a choice is one of the options that were offered - not a fourth one - and its distribution
    // covers exactly them
    let Some(Answer::Choice {
        choice,
        probabilities,
        confidence,
    }) = answers.answers.get("verdict")
    else {
        panic!(
            "a choice came back as something else: {:?}",
            answers.answers
        );
    };
    assert!(
        ["allow", "ask", "deny"].contains(&choice.as_str()),
        "an option that was offered: {choice}"
    );
    assert_eq!(probabilities.len(), 3, "{probabilities:?}");
    assert!(sums_to_one(probabilities), "{probabilities:?}");
    assert!((0.0..=1.0).contains(confidence), "{confidence}");
    assert_eq!(answers.choice("verdict"), Some(choice.as_str()));

    // a score lands inside its own rubric, and the legend echoes the levels back in order
    let Some(Answer::Score {
        score,
        legend,
        probabilities,
        ..
    }) = answers.answers.get("blast")
    else {
        panic!("a score came back as something else: {:?}", answers.answers);
    };
    assert!(
        (0.0..=3.0).contains(score),
        "inside a four-level rubric: {score}"
    );
    assert_eq!(legend.get("0").map(String::as_str), Some("nothing"));
    assert_eq!(
        legend.get("3").map(String::as_str),
        Some("the whole machine")
    );
    assert_eq!(probabilities.len(), 4, "{probabilities:?}");
    assert!(sums_to_one(probabilities), "{probabilities:?}");

    // and the request was priced
    let usage = answers.usage.expect("the endpoint reports what it cost");
    assert!(usage.input_tokens.is_some_and(|it| it > 0), "{usage:?}");
    assert!(usage.output_tokens.is_some_and(|it| it > 0), "{usage:?}");

    // one request, whatever the questions in it
    assert_eq!(jev.attempts(), 1);
}

/// The payload that was previewed is the payload that was sent.
///
/// note: the same promise `nachalnik`'s *the previewed request is the request* makes, checked the
/// only way it can be: the rendered body is accepted by the service as it stands.
#[tokio::test]
async fn the_rendered_payload_is_the_one_the_service_accepts() {
    let _serial = SERIAL.lock().await;
    let jev = live!();

    let asked = vec![(
        "structured".to_owned(),
        Question::noul("Is the tool being asked to write outside the project?"),
    )];
    let state = serde_json::json!({
        "tool": "fs:write",
        "arguments": { "path": "/etc/passwd", "contents": "..." },
    });

    // an object state and an object-shaped question, which the reference allows for both
    let rendered = jev.render(&state, &asked);
    assert_eq!(rendered["state"]["tool"], "fs:write");
    assert_eq!(rendered["questions"]["structured"]["type"], "noul");

    let answers = jev
        .ask(state, asked)
        .await
        .expect("an object state is accepted");
    assert!(
        answers.noul("structured").is_some(),
        "{:?}",
        answers.answers
    );
}

/// What the endpoint serves, and what it says about a model it does not.
#[tokio::test]
async fn the_listing_names_the_models_and_an_unknown_one_is_reported() {
    let _serial = SERIAL.lock().await;
    let jev = live!();

    let listed = jev.models().await;
    assert!(
        listed.iter().any(|it| it == DEFAULT_MODEL),
        "the default should be on the listing: {listed:?}"
    );

    // nothing to say while the model being asked for is one of them
    assert_eq!(jev.take_notice(), None);

    // and a notice rather than a silent 400 on the next request, which is where this would
    // otherwise be found out
    jev.set_model("jev-nope".to_owned()).await;
    let notice = jev
        .take_notice()
        .expect("a model that is not served is worth saying out loud");
    assert!(notice.contains("jev-nope"), "{notice}");
    assert!(notice.contains(DEFAULT_MODEL), "{notice}");

    // and asking anyway is a refusal that names itself, rather than an empty answer
    let refused = jev
        .ask("anything", [("q", Question::noul("Is this fine?"))])
        .await
        .expect_err("an unknown model is refused");
    let said = refused.to_string();
    assert!(said.contains("jev-nope"), "{said}");
    assert!(said.contains("api_usage_error"), "{said}");
}

/// A key that is not a key is reported as one, rather than as an empty answer.
#[tokio::test]
async fn a_bad_key_is_reported_in_the_service_s_own_words() {
    let _serial = SERIAL.lock().await;
    // note: no `live!` - this one needs no valid key, only the endpoint. It is the one live test
    // that costs nothing and can always run
    let jev = Jev::latest("apikey_not_a_key");

    let refused = jev
        .ask("anything", [("q", Question::noul("Is this fine?"))])
        .await
        .expect_err("a bad key is refused");
    let said = refused.to_string();
    assert!(said.contains("authentication_error"), "{said}");

    // and it is not retried: a key is a decision, not a delay
    assert_eq!(jev.attempts(), 1, "a refused key should not be sent again");
}
