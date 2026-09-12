//! Running independent work at the same time, and capping how much of it is in flight.
//!
//! note: written here rather than taken from `futures-util`, which is where a combinator like
//! [`together`] normally comes from. This crate depends on `nachalnik`, `async-trait`, `parking_lot`,
//! `serde` and `serde_json`, and on nothing else; a suite that measures models has no business
//! growing a dependency tree to poll two futures at once. What `futures-util` would buy over the
//! eighty lines below is an intrusive linked list so that waking one future costs O(1) instead of
//! re-polling all of them. At the sizes here - a few dozen requests in flight, each of them a
//! round trip over a network - that is an optimisation of the cheapest thing in the run.
//!
//! note: nothing in here spawns. [`together`] is a future like any other - it makes progress when
//! the caller polls it, and the concurrency comes from the child futures having their I/O
//! registered with the caller's reactor rather than from anything here. Which means one task, one
//! stack, and no question about what happens to a spawned request when a run is dropped halfway.
//!
//! note: two limits, because endpoints publish two kinds and neither implies the other.
//! [`Permits`] caps how many requests are *in flight*; [`Rate`] caps how many are *started* in a
//! window, which is how a free tier words it, and no count of things in flight can stand in for
//! that - eight at once against a fast endpoint is eighty a second. A rate needs a clock, so this
//! module uses `tokio::time`; that picks an executor for the caller, and it is only defensible
//! because [`nachalnik`] already picked the same one for its `JoinSet` and its `broadcast`.
//!
//! note: what is still not here is what to do when a limit is exceeded anyway. A `429` and its
//! `Retry-After` are answered in whichever [`Provider`](nachalnik::Provider) the caller supplied,
//! because that is the layer that knows the wire format they arrived in. These two are about not
//! provoking one in the first place.

use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Duration,
};

use async_trait::async_trait;
use nachalnik::{BoxError, DeltaSink, ModelInfo, ModelRequest, ModelResponse, Provider};
use parking_lot::Mutex;
// note: tokio's clock rather than `std::time::Instant`, and it has to be the same clock the sleep
// below is measured on. With `std`'s, a test running on a paused clock ages nothing out of the
// window - the sleeps return instantly on virtual time while the timestamps advance on real time
// - and `admit` waits for room that never appears. Outside a test the two are the same clock, so
// this costs nothing and buys a rate limiter that can be tested at all.
use tokio::time::Instant;

/// How many things may be in flight at once.
///
/// note: a counting semaphore, and nothing more. It is not `Clone` and is meant to be held by
/// reference from one place per run, because two of these are two ceilings and the whole reason
/// to have one is that it is collective: nine experiments sharing one endpoint have to share one
/// budget, or each of them politely limits itself to eight and the endpoint sees seventy-two.
#[derive(Debug)]
pub struct Permits {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    free: usize,
    /// note: a queue rather than a set, so that a future that has been waiting longest is woken
    /// first. Not for fairness between requests, which nobody would notice, but because the
    /// alternative can starve one indefinitely and a run that never finishes is the failure this
    /// module is supposed to prevent rather than cause.
    waiting: VecDeque<Waker>,
}

impl Permits {
    /// A ceiling of `at_once`, which is raised to one if it was zero.
    ///
    /// note: zero would be a ceiling that admits nothing, which is a deadlock rather than a
    /// setting. Treated as one, because the caller asking for "no concurrency" means sequential.
    pub fn new(at_once: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                free: at_once.max(1),
                waiting: VecDeque::new(),
            }),
        }
    }

    /// A ceiling high enough that nothing ever waits on it.
    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Waits for a permit and hands back the guard that returns it.
    pub fn acquire(&self) -> Acquiring<'_> {
        Acquiring { permits: self }
    }

    /// How many are free right now; for tests and for a caller that wants to say so.
    pub fn free(&self) -> usize {
        self.inner.lock().free
    }

    fn release(&self) {
        let mut inner = self.inner.lock();
        inner.free += 1;
        if let Some(waker) = inner.waiting.pop_front() {
            // dropped before waking, so that the woken future does not immediately block on the
            // lock this thread is still holding
            drop(inner);
            waker.wake();
        }
    }
}

/// The future [`Permits::acquire`] hands back.
///
/// note: exported rather than left `pub` inside a private module, which is what it was. Nothing
/// could name it from outside the crate, so `acquire` could only ever be awaited where it was
/// called - a caller wanting to hold one, put it in a struct or select over it had a type it was
/// not allowed to write down. Every other `pub` item in here is re-exported and this one was
/// missed, which is a thing a compiler has no reason to mention: an unnameable return type is
/// legal and only shows up as a page that is not in the documentation.
#[derive(Debug)]
pub struct Acquiring<'a> {
    permits: &'a Permits,
}

impl<'a> Future for Acquiring<'a> {
    type Output = Permit<'a>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.permits.inner.lock();
        if inner.free > 0 {
            inner.free -= 1;
            return Poll::Ready(Permit {
                permits: self.permits,
            });
        }

        // note: replaced rather than pushed when this future has already registered, or a future
        // polled repeatedly while it waits leaves a queue entry behind on every poll and the
        // queue grows without bound
        if !inner.waiting.iter().any(|seen| seen.will_wake(cx.waker())) {
            inner.waiting.push_back(cx.waker().clone());
        }

        Poll::Pending
    }
}

/// A permit, returned to the [`Permits`] it came from when dropped.
#[derive(Debug)]
pub struct Permit<'a> {
    permits: &'a Permits,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.permits.release();
    }
}

/// How fast a run is allowed to go: how many requests at once, and how many per interval.
///
/// note: two knobs because endpoints publish two different kinds of limit and neither implies the
/// other. A concurrency limit is answered by [`Pace::at_once`]; a published rate - "20 requests
/// per minute", which is what a free tier usually says - is answered by [`Pace::per_minute`], and
/// a count of requests in flight cannot substitute for it. The two only coincide through latency:
/// eight in flight against a one-second endpoint happens to be about eight a second, and against
/// a fast one it is eighty.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    at_once: usize,
    per: Option<(usize, Duration)>,
}

impl Default for Pace {
    /// One at a time and no rate limit, which is to say sequential: what [`evaluate`] does.
    ///
    /// [`evaluate`]: crate::evaluate
    fn default() -> Self {
        Self {
            at_once: 1,
            per: None,
        }
    }
}

impl Pace {
    /// At most this many requests in flight at once.
    #[must_use]
    pub fn at_once(at_once: usize) -> Self {
        Self {
            at_once: at_once.max(1),
            per: None,
        }
    }

    /// And at most this many started in any sixty seconds.
    #[must_use]
    pub fn per_minute(self, requests: usize) -> Self {
        self.per(requests, Duration::from_secs(60))
    }

    /// And at most this many started in any window of that length.
    #[must_use]
    pub fn per(mut self, requests: usize, window: Duration) -> Self {
        self.per = (requests > 0).then_some((requests, window));
        self
    }

    /// How many may be in flight.
    pub fn width(&self) -> usize {
        self.at_once
    }

    /// The rate, if one was set.
    pub fn rate(&self) -> Option<(usize, Duration)> {
        self.per
    }
}

/// A sliding window over when requests were let through.
///
/// note: a sliding window rather than a token bucket, because the limit being obeyed is worded as
/// a sliding window - "20 per minute" refuses the twenty-first request that arrives within sixty
/// seconds of the first, whatever the shape of the traffic. A bucket refilled at a steady rate
/// permits a burst of its whole capacity after any quiet spell, which is the thing an endpoint
/// counting requests in a window will reject.
///
/// note: and a minimum gap between admissions on top of the window, because the window alone
/// permits the whole allowance at once - see [`Rate::spacing`]. The two are both enforced and
/// neither subsumes the other: the window is the limit as the endpoint words it, the spacing is
/// what stops a run from spending the whole allowance in a second and then idling.
///
/// note: what is recorded is when a request was *admitted*, not when it finished. A limit on how
/// many may be started in a minute is not a limit on how many may be outstanding, and the two
/// come apart badly on an endpoint that sometimes takes ten minutes to answer - which is the case
/// this was written after.
#[derive(Debug)]
struct Rate {
    allowed: usize,
    window: Duration,
    admitted: Mutex<VecDeque<Instant>>,
}

impl Rate {
    fn new(allowed: usize, window: Duration) -> Self {
        Self {
            allowed: allowed.max(1),
            window,
            admitted: Mutex::new(VecDeque::new()),
        }
    }

    /// The least time that may pass between two admissions.
    ///
    /// note: what makes the limit a pace rather than a quota. A window alone is obeyed perfectly
    /// by firing every request it allows in the window's first instant and then sitting out the
    /// rest, which is exactly the burst an endpoint's own limiter sees and rejects - measured, a
    /// run at eight in flight took a small free endpoint down inside a minute while staying well
    /// inside eighteen a minute. Spacing them is the difference between not exceeding a limit on
    /// average and not exceeding it at any moment.
    fn spacing(&self) -> Duration {
        self.window / self.allowed as u32
    }

    /// Waits until letting one more through would break neither the window nor the spacing, then
    /// records it.
    async fn admit(&self) {
        loop {
            let wait = {
                let mut admitted = self.admitted.lock();
                let now = Instant::now();
                while admitted
                    .front()
                    .is_some_and(|at| now.duration_since(*at) >= self.window)
                {
                    admitted.pop_front();
                }

                // the oldest one in the window is the one whose leaving makes room, so this is
                // exactly how long there is to wait rather than a guess at it
                let room = match admitted.len() < self.allowed {
                    true => Duration::ZERO,
                    false => admitted
                        .front()
                        .map(|at| self.window.saturating_sub(now.duration_since(*at)))
                        .unwrap_or_default(),
                };
                // and this is how long since the last one went, which is the half that keeps the
                // pace steady rather than merely legal
                let gap = admitted
                    .back()
                    .map(|at| self.spacing().saturating_sub(now.duration_since(*at)))
                    .unwrap_or_default();

                let wait = room.max(gap);
                if wait.is_zero() {
                    admitted.push_back(now);
                    return;
                }

                wait
            };

            // note: re-checked after sleeping rather than admitted on waking, because every waiter
            // wakes on the same departure and only one of them may have the place it left
            tokio::time::sleep(wait).await;
        }
    }
}

/// Any [`Provider`], with a ceiling on how many requests it may have in flight.
///
/// note: the ceiling goes here rather than around the loops that fan out, because this is the one
/// place every request in a run passes through. A ceiling applied at a loop only governs that
/// loop: the first version of this governed the ablation sweep, and a run of nine experiments
/// promptly put nine live probes on the wire underneath it, which the test
/// `a_paced_run_goes_abreast_but_no_wider_than_it_was_told` caught. Wrapping the provider means an
/// experiment cannot exceed the ceiling by making a request some other way, including an
/// experiment this crate has never seen.
///
/// note: it wraps rather than replaces, and forwards [`Provider::info`] untouched, so a subject
/// still reports the model it is actually talking to. The copies an
/// [`Ablation`](crate::Ablation) resumes inherit the wrapped provider from the subject they were
/// forked from, which is what makes one of these cover the ablations as well as the conversation.
pub struct Paced {
    inner: Arc<dyn Provider>,
    permits: Arc<Permits>,
    rate: Option<Arc<Rate>>,
}

impl Paced {
    /// Wraps a provider so that no more than `permits` allows are ever in flight.
    pub fn new(inner: Arc<dyn Provider>, permits: Arc<Permits>) -> Self {
        Self {
            inner,
            permits,
            rate: None,
        }
    }

    /// And no more than a rate allows are started, sharing one window with everything else that
    /// was given the same [`Rate`].
    fn limited(mut self, rate: Option<Arc<Rate>>) -> Self {
        self.rate = rate;
        self
    }
}

/// The shared state one run's ceiling and rate are enforced through.
///
/// note: built once by [`evaluate_with`](crate::evaluate_with) and cloned onto every subject, so
/// that nine experiments share one window and one count rather than nine of each. This is the
/// whole reason the type exists instead of each caller assembling the parts.
#[derive(Debug, Clone)]
pub struct Governor {
    permits: Arc<Permits>,
    rate: Option<Arc<Rate>>,
}

impl Governor {
    /// The shared state for a run at this pace.
    pub fn new(pace: Pace) -> Self {
        Self {
            permits: Arc::new(Permits::new(pace.width())),
            rate: pace
                .rate()
                .map(|(allowed, window)| Arc::new(Rate::new(allowed, window))),
        }
    }

    /// Wraps a provider so that it runs under this run's ceiling and rate.
    pub fn over(&self, inner: Arc<dyn Provider>) -> Paced {
        Paced::new(inner, self.permits.clone()).limited(self.rate.clone())
    }
}

impl fmt::Debug for Paced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paced")
            .field("free", &self.permits.free())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Provider for Paced {
    fn info(&self) -> ModelInfo {
        self.inner.info()
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        // note: the permit first and the rate second, so that whoever is waiting out a window is
        // one of the few holding a permit rather than all of them holding none. The other order
        // admits the whole run to the rate queue at once and makes the ceiling decorative.
        let _permit = self.permits.acquire().await;
        if let Some(rate) = &self.rate {
            rate.admit().await;
        }

        self.inner.respond(request, deltas).await
    }
}

/// Runs every future at once and hands back what they produced, in the order they were given.
///
/// note: the order is the input's, not the order they finished in. A report whose rows move
/// depending on which request happened to come back first is a report nobody can diff against
/// last week's, and the whole crate is built so that two runs are comparable.
///
/// note: no permit handling in here. A caller that wants a ceiling wraps each future in one -
/// `async { let _permit = permits.acquire().await; work().await }` - which keeps this combinator
/// about polling and keeps the ceiling somewhere a caller can see it.
pub async fn together<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: Future,
{
    All {
        running: futures
            .into_iter()
            .map(|future| Some(Box::pin(future)))
            .collect(),
        done: Vec::new(),
    }
    .await
}

struct All<F: Future> {
    /// `None` once that slot has produced its output.
    running: Vec<Option<Pin<Box<F>>>>,
    done: Vec<Option<F::Output>>,
}

// note: sound because nothing here projects a pin into a field, which is also why this can be
// written safely rather than reached for with `get_unchecked_mut` - the crate denies unsafe. Each
// child future is already pinned in its own box and stays there for its whole life; what moving
// an `All` moves is two `Vec` headers. Stated rather than derived because `F::Output` is not
// known to be `Unpin`, and an output this combinator only ever stores and hands back does not
// need to be.
impl<F: Future> Unpin for All<F> {}

impl<F: Future> Future for All<F> {
    type Output = Vec<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // safe because nothing below moves out of `this`: the futures stay pinned in their boxes
        // and only the `Option`s around them are replaced
        let this = self.get_mut();
        this.done.resize_with(this.running.len(), || None);

        let mut pending = false;
        for (slot, future) in this.running.iter_mut().enumerate() {
            let Some(running) = future else {
                continue;
            };
            match running.as_mut().poll(cx) {
                Poll::Ready(output) => {
                    this.done[slot] = Some(output);
                    // dropped as soon as it is finished rather than at the end, so that whatever
                    // it was holding - a permit, a connection - is released now
                    *future = None;
                }
                Poll::Pending => pending = true,
            }
        }

        match pending {
            true => Poll::Pending,
            false => Poll::Ready(
                this.done
                    .drain(..)
                    .map(|output| output.expect("every slot is filled once nothing is pending"))
                    .collect(),
            ),
        }
    }
}
