//! The suite against a real model, over the real wire.
//!
//! Skipped unless an API key is in the environment, the same variables the rest of the workspace
//! reads:
//!
//! ```text
//! OPENROUTER_API_KEY=sk-or-... cargo test -p nachalnik-eval --test live -- --nocapture
//! ```
//!
//! ```text
//! NACHALNIK_TEST_MODEL=google/gemini-3.5-flash-lite   # the default
//! NACHALNIK_BASE_URL=...                              # any OpenAI-compatible server
//! ```
//!
//! note: What these check is what a rulebook cannot: that a real model answers in the shape the
//! probes ask for often enough for the readings to find anything, and that the copies come back
//! with words in them. They are assertion-heavy about the *record* - every question answered,
//! every claim either read or recorded as unreadable, every figure derived from steps that are
//! there - and assertion-light about the answers, because a model that is bad at introspection is
//! a result and not a failure. The one thing they will not tolerate is a run that produces
//! figures out of nothing.
//!
//! note: One experiment, at one copy per condition, which is about fifteen requests. The whole
//! suite is the `bench` example's job; a test suite that spent sixty requests of somebody's free
//! tier every time `cargo test` ran would be a bad neighbour.

use std::{env, sync::Arc};

use nachalnik::{Config, Kernel, Params, Provider};
use nachalnik_eval::{
    Experiment, Outcome, Step, Subject, Trial,
    suite::{Attribution, Recursion},
};
use nachalnik_utils::{OpenAiCompatible, api_key, out_of_quota};
use serde_json::json;

/// A small, free, widely available model.
const DEFAULT_MODEL: &str = "google/gemini-3.5-flash-lite";

/// Live tests take turns, so that a free tier's rate limit is not what is being measured.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The model to measure, or `None` when there is no key and the test should skip.
fn provider() -> Option<Arc<dyn Provider>> {
    let key = match api_key() {
        Ok(key) => key,
        Err(why) => {
            println!("skipped: {why}");
            return None;
        }
    };
    let model = env::var("NACHALNIK_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let provider = OpenAiCompatible::new(
        OpenAiCompatible::client(),
        model,
        nachalnik_utils::base_url(),
        key,
    )
    .streaming(false);

    Some(Arc::new(provider))
}

/// A subject on a real model, at the lowest temperature the endpoint will take.
fn subject(provider: Arc<dyn Provider>, name: &str) -> Subject {
    let kernel = Kernel::new(Config {
        session_name: Some(name.to_owned()),
        ..Config::default()
    });
    kernel.set_provider(provider);
    let mut params = Params::new();
    params.insert("temperature".to_owned(), json!(0));
    kernel.set_params(params);

    Subject::new(kernel)
}

/// Runs one experiment, and hands back `None` when the endpoint said the allowance was spent.
async fn run(experiment: impl Experiment) -> Option<Outcome> {
    let provider = provider()?;
    let subject = subject(provider, experiment.name());
    let trial = Trial::new(experiment.name(), &subject);

    match experiment.run(&subject, &trial).await {
        Ok(()) => Some(Outcome::of(&trial, None)),
        Err(e) if out_of_quota(&e.to_string()) => {
            println!("skipped: the endpoint has no allowance left ({e})");
            None
        }
        Err(e) => panic!("{}: {e}", experiment.name()),
    }
}

/// Every figure in an outcome is supported by a step that is in it.
fn audit(outcome: &Outcome) {
    println!("\n{outcome}");
    for step in &outcome.steps {
        match step {
            // a question that came back empty is a broken run rather than a bad answer: the
            // readings are allowed to fail, the round trip is not
            Step::Asked { question, said, .. } => {
                assert!(
                    !said.trim().is_empty(),
                    "nothing came back for `{question}`"
                )
            }
            Step::Measured { observation, .. } => {
                assert!(!observation.answers.is_empty(), "a condition ran no copies");
                assert!(observation.items > 0, "a copy read an empty context");
                assert!(
                    observation.applied.is_complete(),
                    "an intervention missed its target"
                );
            }
            _ => {}
        }
    }

    let resolutions = outcome
        .steps
        .iter()
        .filter(|step| matches!(step, Step::Resolved(_)))
        .count();
    assert!(resolutions > 0, "nothing was compared with anything");
    assert_eq!(
        outcome.scores.n + outcome.scores.unmeasured,
        resolutions,
        "the scores count claims the record does not have"
    );
    assert!(outcome.spend.requests > 0, "it reported no requests");
}

#[tokio::test]
async fn a_real_model_can_be_asked_what_its_answer_is_made_of() {
    let _turn = SERIAL.lock().await;
    let Some(outcome) = run(Attribution::new()).await else {
        return;
    };

    audit(&outcome);
    // the depot dossier has seven notes and each is ablated once, against one control
    let conditions = outcome
        .steps
        .iter()
        .filter(|step| matches!(step, Step::Measured { .. }))
        .count();
    assert_eq!(conditions, 8);
}

#[tokio::test]
async fn a_real_model_can_be_asked_what_a_copy_of_it_would_say() {
    let _turn = SERIAL.lock().await;
    let Some(outcome) = run(Recursion::new().depth(2)).await else {
        return;
    };

    audit(&outcome);
    let depths: Vec<usize> = outcome.depths.0.iter().map(|depth| depth.depth).collect();
    assert_eq!(depths, vec![1, 2]);
}
