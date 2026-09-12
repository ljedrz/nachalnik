//! The combinator the suite runs its independent work on, and the ceiling it runs it under.
//!
//! note: no model and no network. What is being checked is that the thing polls what it was
//! given, hands the answers back in the order they were asked for rather than the order they
//! arrived, and never lets more run at once than it was told to - three properties a run's
//! correctness rests on and none of which need a provider to demonstrate.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use nachalnik_eval::{Acquiring, Permit, Permits, together};

/// Counts what is in flight and remembers the most there ever was.
#[derive(Default)]
struct Watermark {
    now: AtomicUsize,
    most: AtomicUsize,
}

impl Watermark {
    fn entered(&self) {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
    }

    fn left(&self) {
        self.now.fetch_sub(1, Ordering::SeqCst);
    }

    fn most(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }
}

/// Enough yields that every other future has had a chance to be polled in between.
async fn dawdle(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn results_come_back_in_the_order_they_were_given() {
    // the first dawdles longest, so the order they *finish* in is the reverse of the order they
    // were asked for - which is the whole point: a report whose rows moved with the weather
    // could not be diffed against last week's
    let answers = together((0..5).map(|n| async move {
        dawdle(5 - n).await;
        n
    }))
    .await;

    assert_eq!(answers, vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn the_work_actually_runs_at_the_same_time() {
    let watermark = Arc::new(Watermark::default());

    let answers = together((0..8).map(|n| {
        let watermark = watermark.clone();
        async move {
            watermark.entered();
            dawdle(4).await;
            watermark.left();
            n
        }
    }))
    .await;

    assert_eq!(answers.len(), 8);
    // if this polled one future to completion before starting the next, the most ever in flight
    // would be one
    assert_eq!(watermark.most(), 8, "all eight should have been in flight");
}

#[tokio::test]
async fn permits_cap_how_many_run_at_once() {
    let permits = Permits::new(3);
    let watermark = Arc::new(Watermark::default());

    let answers = together((0..12).map(|n| {
        let watermark = watermark.clone();
        let permits = &permits;
        async move {
            let _permit = permits.acquire().await;
            watermark.entered();
            dawdle(4).await;
            watermark.left();
            n
        }
    }))
    .await;

    assert_eq!(answers, (0..12).collect::<Vec<_>>());
    assert!(
        watermark.most() <= 3,
        "the ceiling was 3, {} were in flight",
        watermark.most()
    );
    assert!(
        watermark.most() > 1,
        "a ceiling of 3 that only ever ran one is not a ceiling, it is a queue"
    );
    assert_eq!(permits.free(), 3, "every permit should have been returned");
}

#[tokio::test]
async fn a_permit_is_returned_when_its_holder_is_dropped() {
    let permits = Permits::new(1);
    assert_eq!(permits.free(), 1);

    {
        let _permit = permits.acquire().await;
        assert_eq!(permits.free(), 0);
    }

    assert_eq!(permits.free(), 1);
    // and the freed one is handed straight to whoever was waiting
    let _again = permits.acquire().await;
    assert_eq!(permits.free(), 0);
}

#[tokio::test]
async fn a_ceiling_of_none_is_a_ceiling_of_one() {
    // zero would admit nothing, which is a deadlock rather than a setting
    let permits = Permits::new(0);

    let _permit = permits.acquire().await;
    assert_eq!(permits.free(), 0);
}

#[tokio::test]
async fn nothing_to_run_is_not_an_error() {
    let answers = together(Vec::<std::future::Ready<u8>>::new()).await;

    assert!(answers.is_empty());
}

/// The types `Permits` hands out can be written down from outside the crate.
///
/// note: a compile-time test and nothing else - it names them, which is the whole assertion.
/// `Acquiring` was `pub` in a private module and left out of the re-export, so `acquire` could be
/// awaited where it was called and nowhere else: holding one, keeping it in a struct or selecting
/// over it needed a type nobody outside was allowed to write. Nothing reports that. An unnameable
/// return type is legal, the crate builds, and the only trace is a page missing from the
/// documentation - so the guard has to be a caller naming it, which is what this is.
#[tokio::test]
async fn what_acquire_hands_back_can_be_named_by_a_caller() {
    let permits = Permits::new(1);

    let waiting: Acquiring<'_> = permits.acquire();
    let held: Permit<'_> = waiting.await;
    assert_eq!(permits.free(), 0, "the permit is out");

    drop(held);
    assert_eq!(permits.free(), 1, "and back");
}
