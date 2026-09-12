//! That a run asked to go abreast actually does, and never wider than it was told.
//!
//! note: the counting happens in a [`Provider`], which is the only place it means anything. A
//! ceiling that holds in a unit test of the semaphore but not in a run has not been tested; what
//! matters is how many requests are on their way to an endpoint at the same moment, so the thing
//! that counts them is the thing that would be the endpoint.
//!
//! note: the provider yields before answering. A provider that returns on its first poll is never
//! concurrent with anything - every future would run to completion the moment it was polled - and
//! a test built on one would report a peak of 1 however the harness behaved, which is a test that
//! passes for the wrong reason.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use nachalnik::{
    BoxError, Config, Content, DeltaSink, Kernel, ModelInfo, ModelRequest, ModelResponse, Provider,
    StopReason, Usage, async_trait,
};
use nachalnik_eval::{Pace, Subject, evaluate, evaluate_with, suite};

/// A provider that answers nothing in particular and remembers how many were in flight at once.
struct Counting {
    now: AtomicUsize,
    most: AtomicUsize,
    calls: AtomicUsize,
}

impl Counting {
    fn new() -> Self {
        Self {
            now: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        }
    }

    fn most(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for Counting {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(64_000),
            ..ModelInfo::new("counting", "counting")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);

        // the await that makes this observable at all; see the note at the top
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        self.now.fetch_sub(1, Ordering::SeqCst);

        Ok(ModelResponse {
            content: Some(Content::text("ANSWER: omsk\nCONFIDENCE: 50")),
            reasoning: None,
            tool_calls: Vec::new(),
            stop: StopReason::EndTurn,
            usage: Some(Usage::default()),
            raw: None,
        })
    }
}

fn subject_on(provider: Arc<Counting>, name: &str) -> Subject {
    let kernel = Kernel::new(Config {
        session_name: Some(name.to_owned()),
        ..Config::default()
    });
    kernel.set_provider(provider);

    Subject::new(kernel)
}

#[tokio::test]
async fn a_plain_run_still_sends_one_request_at_a_time() {
    let provider = Arc::new(Counting::new());

    let _report = evaluate(suite::all(), |name| Ok(subject_on(provider.clone(), name))).await;

    assert!(provider.calls() > 0, "the run made no requests at all");
    assert_eq!(
        provider.most(),
        1,
        "`evaluate` says in its own doc comment that nothing runs concurrently"
    );
}

#[tokio::test]
async fn a_paced_run_goes_abreast_but_no_wider_than_it_was_told() {
    let provider = Arc::new(Counting::new());
    let at_once = 4;

    let _report = evaluate_with(
        suite::all(),
        |name| Ok(subject_on(provider.clone(), name)),
        Pace::at_once(at_once),
        |_| {},
    )
    .await;

    assert!(provider.calls() > 0, "the run made no requests at all");
    assert!(
        provider.most() > 1,
        "asked for {at_once} at once and never had more than one in flight"
    );
    assert!(
        provider.most() <= at_once,
        "asked for {at_once} at once and had {} in flight",
        provider.most()
    );
}

#[tokio::test]
async fn pacing_does_not_change_how_many_requests_a_run_makes() {
    let plain = Arc::new(Counting::new());
    let _ = evaluate(suite::all(), |name| Ok(subject_on(plain.clone(), name))).await;

    let paced = Arc::new(Counting::new());
    let _ = evaluate_with(
        suite::all(),
        |name| Ok(subject_on(paced.clone(), name)),
        Pace::at_once(4),
        |_| {},
    )
    .await;

    // the same work, done at a different width: a ceiling that changed what was asked would be
    // changing the measurement rather than the schedule
    assert_eq!(plain.calls(), paced.calls());
}

// ------------------------------------------------------------------------------------- the rate

/// note: `start_paused` runs these on tokio's own clock, which advances when every task is idle.
/// So a window of sixty seconds is waited out instantly and exactly, and the test neither sleeps
/// nor depends on how fast the machine is - the two things that make a rate-limit test flaky.
#[tokio::test(start_paused = true)]
async fn a_rate_limit_is_obeyed_over_the_window() {
    let provider = Arc::new(Counting::new());
    let allowed = 6;

    let began = tokio::time::Instant::now();
    let _report = evaluate_with(
        suite::all(),
        |name| Ok(subject_on(provider.clone(), name)),
        Pace::at_once(8).per(allowed, Duration::from_secs(10)),
        |_| {},
    )
    .await;

    let calls = provider.calls();
    assert!(calls > allowed, "too few requests to have hit the limit");

    // the run cannot have been shorter than the windows its requests had to be spread over: with
    // `allowed` per window, `calls` of them need at least this long
    let windows = (calls - 1) / allowed;
    let least = Duration::from_secs(10) * windows as u32;
    assert!(
        began.elapsed() >= least,
        "{calls} requests at {allowed} per 10s took {:?}, which is under the {least:?} that many \
         cannot be done in",
        began.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn no_rate_means_no_waiting() {
    let provider = Arc::new(Counting::new());

    let began = tokio::time::Instant::now();
    let _report = evaluate_with(
        suite::all(),
        |name| Ok(subject_on(provider.clone(), name)),
        Pace::at_once(8),
        |_| {},
    )
    .await;

    assert!(provider.calls() > 0);
    // nothing here sleeps, so on a paused clock no time passes at all
    assert_eq!(began.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn the_window_is_shared_rather_than_one_each() {
    // nine experiments under one limit of four per window: if each experiment got a window of its
    // own this would let thirty-six through in the time four are allowed
    let provider = Arc::new(Counting::new());

    let began = tokio::time::Instant::now();
    let _report = evaluate_with(
        suite::all(),
        |name| Ok(subject_on(provider.clone(), name)),
        Pace::at_once(9).per(4, Duration::from_secs(30)),
        |_| {},
    )
    .await;

    let calls = provider.calls();
    let windows = (calls - 1) / 4;
    assert!(
        began.elapsed() >= Duration::from_secs(30) * windows as u32,
        "{calls} requests went out faster than one shared window of 4/30s allows"
    );
}

/// Records when each request was admitted, so the *shape* of the traffic can be asserted on and
/// not just its total.
struct Spaced {
    at: std::sync::Mutex<Vec<tokio::time::Instant>>,
}

#[async_trait]
impl Provider for Spaced {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(64_000),
            ..ModelInfo::new("spaced", "spaced")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.at.lock().unwrap().push(tokio::time::Instant::now());
        tokio::time::sleep(Duration::from_millis(1)).await;

        Ok(ModelResponse {
            content: Some(Content::text("ANSWER: omsk\nCONFIDENCE: 50")),
            reasoning: None,
            tool_calls: Vec::new(),
            stop: StopReason::EndTurn,
            usage: Some(Usage::default()),
            raw: None,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_rate_is_a_pace_rather_than_a_quota_spent_at_once() {
    let provider = Arc::new(Spaced {
        at: std::sync::Mutex::new(Vec::new()),
    });
    // ten in ten seconds is one a second, and the ceiling of 8 is deliberately high: before the
    // spacing went in, this admitted eight instantly and then waited
    let window = Duration::from_secs(10);
    let allowed = 10;

    let _report = evaluate_with(
        suite::all(),
        |name| {
            let kernel = Kernel::new(Config {
                session_name: Some(name.to_owned()),
                ..Config::default()
            });
            kernel.set_provider(provider.clone());
            Ok(Subject::new(kernel))
        },
        Pace::at_once(8).per(allowed, window),
        |_| {},
    )
    .await;

    let at = provider.at.lock().unwrap().clone();
    assert!(at.len() > 20, "too few requests to say anything");

    let spacing = window / allowed as u32;
    let mut tightest = Duration::MAX;
    for pair in at.windows(2) {
        tightest = tightest.min(pair[1].duration_since(pair[0]));
    }

    assert!(
        tightest >= spacing,
        "two requests went {tightest:?} apart, under the {spacing:?} a rate of {allowed} per \
         {window:?} spaces them by"
    );
}
