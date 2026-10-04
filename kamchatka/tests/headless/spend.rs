//! What a session may spend: the ceiling, what is charged against it and when, and what is said
//! when an endpoint does not report what it charged.

use std::sync::Arc;

use kamchatka::{
    headless::Headless,
    wiring::{Setup, Wired},
};
use nachalnik::{
    Capability, Event, Grant, ModelResponse, StopReason, Usage,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;

use crate::{Slow, capped, priced, run, run_capped};

/// A ceiling stops the session once the provider has charged past it, and the next message is
/// passed over.
///
/// note: the sibling of the deadline above, and the same kind of guard for a different thing a run
/// nobody is watching can spend. What it stops here is *between* turns: three lines were piped in,
/// the second answer crossed the line, and the third was not sent - which is the shape a script
/// driving a long session has.
#[tokio::test]
async fn a_ceiling_ends_the_run_once_the_bill_passes_it() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 200),
        priced(ModelResponse::text("two"), 400, 200),
        priced(ModelResponse::text("three"), 400, 200),
    ];
    let run = run_capped("first\nsecond\nthird\nfourth\n", script, 1000, |_| {}).await;

    // 1,200 rather than 1,000: a ceiling is a stopping rule and not a cap, because what a response
    // costs is known only once it has arrived. Both halves of the bill are in that figure
    assert!(
        run.prose.contains("spent 1,200 tokens of 1,000; stopping"),
        "{}",
        run.prose
    );
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        2,
        "a line was sent after the ceiling was reached"
    );
    // and passed over rather than put in the context, where the next turn a script paid for would
    // have carried it out unasked
    let asked = run
        .app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::UserMessage))
        .count();
    assert_eq!(asked, 2, "a message went into the context unsent");
    // said once, not a refusal a line for the rest of the script
    assert_eq!(
        run.prose.matches("passed over unsent").count(),
        1,
        "{}",
        run.prose
    );
    assert!(
        !run.prose.contains("nothing more is being sent"),
        "{}",
        run.prose
    );
    // and it left by the ordinary door, which is the whole difference between this and a kill: the
    // session is ended and the log says so
    assert_eq!(
        run.names().last().map(String::as_str),
        Some("session.finished")
    );
}

/// The way back the stop names is a line, and a script that sends it is heard.
///
/// note: the loop stopped reading at the ceiling, so the `/spend 0` the stop recommends - and
/// RUNNING.md calls the way back - was dropped with everything else a script sent after it.
#[tokio::test]
async fn a_script_can_raise_the_ceiling_it_ran_into() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 800),
        priced(ModelResponse::text("two"), 400, 800),
    ];
    let run = run_capped(
        "first\nnot sent\n/spend\n/spend 0\nsecond\n",
        script,
        1000,
        |_| {},
    )
    .await;

    assert!(
        run.prose
            .contains("this run has spent 1,200 tokens of 1,000"),
        "{}",
        run.prose
    );
    assert!(run.prose.contains("two"), "{}", run.prose);
    let asked: Vec<String> = run
        .app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::UserMessage))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(asked, ["first", "second"]);
}

/// It stops a turn that is in flight, rather than waiting for one to end.
///
/// note: this is the case it exists for. A model that has found a loop - a tool that fails the
/// same way every time, a question it keeps re-asking - spends its budget *inside* one turn, and a
/// guard that only looked between them would watch the whole of it go. The script here is a tool
/// call that would be answered and called again, and the ceiling is crossed on the first response.
#[tokio::test]
async fn a_ceiling_interrupts_the_turn_it_is_crossed_in() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
            900,
            300,
        ),
        priced(
            ModelResponse::tool_calls(vec![call("c2", "peek", json!({}))]),
            900,
            300,
        ),
        priced(ModelResponse::text("never reached"), 900, 300),
    ];
    let run = run_capped("go\n", script, 1000, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(run.names().contains(&"turn.interrupted".to_owned()));
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        1,
        "the turn asked again after the ceiling was reached"
    );
    assert!(!run.prose.contains("never reached"), "{}", run.prose);
}

/// It is charged where a live run charges it: off the events, with the turn still going.
///
/// note: the test above crosses the line in a turn whose every step was over before the loop looked
/// again, so what read the response was the drain that runs behind a finished turn. This one has a
/// tool that takes long enough for the loop to come round while the turn is still in flight, which
/// is where a real one spends nearly all of its time - and the response is read on the ordinary
/// event branch instead. Both are needed, and each of these fails if the other's is taken out.
#[tokio::test]
async fn a_ceiling_is_charged_while_the_turn_is_still_running() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
            900,
            300,
        ),
        priced(ModelResponse::text("never reached"), 900, 300),
    ];
    let run = run_capped("go\n", script, 1000, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    assert!(
        run.prose.contains("spent 1,200 tokens of 1,000; stopping"),
        "{}",
        run.prose
    );
    // the point of reading it there: the turn is stopped before it asks again, rather than after
    assert!(!run.prose.contains("never reached"), "{}", run.prose);
}

/// A response whose `model.finished` the broadcast dropped is still charged, and still stops the
/// turn before it asks again.
///
/// note: what a lag does to a subscriber, done on purpose: every event but that one is handed in.
/// The ceiling was counted off the event, so a subscriber that fell behind a fast stream missed the
/// response that crossed it and the session went on spending.
#[tokio::test]
async fn a_response_the_broadcast_dropped_is_still_charged() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
            900,
            300,
        ),
        priced(ModelResponse::text("never reached"), 900, 300),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(script, Some(1000));
    app.kernel.add_tool(Arc::new(Slow));

    app.submit("go").await;
    loop {
        tokio::select! {
            Ok(event) = events.recv() => {
                if !matches!(event, Event::ModelFinished { .. }) {
                    app.on_event(event);
                }
            }
            Some(outcome) = finished.recv() => {
                while let Ok(event) = events.try_recv() {
                    if !matches!(event, Event::ModelFinished { .. }) {
                        app.on_event(event);
                    }
                }
                app.on_outcome(outcome);
                break;
            }
        }
    }

    assert_eq!(app.spent(), 1200);
    assert!(app.overspent());
    let requests = app
        .kernel
        .history()
        .iter()
        .filter(|record| record.event.name() == "model.requested")
        .count();
    assert_eq!(requests, 1, "the turn asked again past the ceiling");
}

/// What a `fork` is charged counts against the ceiling, as the session's own requests do.
///
/// note: a fork is a kernel of its own, and the ceiling was kept off this session's events alone,
/// so a model drafting over and over spent a full-context request each time and none of it was
/// counted. The fork's response here is the one that crosses the line; the turn's own requests
/// cost almost nothing, and the last is priced at nothing so that the figure is the same whether
/// the turn got as far as asking for it or not.
#[tokio::test]
async fn a_forks_request_counts_against_the_ceiling() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "fork", json!({ "action": "draft" }))]),
            100,
            100,
        ),
        priced(ModelResponse::text("what I would say"), 900, 300),
        priced(ModelResponse::text("done"), 0, 0),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        tools: Some(vec!["fork".to_owned()]),
        spend: Some(1000),
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    app.kernel
        .set_provider(Arc::new(ScriptedProvider::new(script)));

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Allow, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, &b"go\n"[..])
        .await
        .expect("the run failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert_eq!(app.spent(), 1400, "the fork's 1,200 were counted: {prose}");
    assert!(app.overspent());
    assert!(
        prose.contains("spent 1,400 tokens of 1,000; stopping"),
        "{prose}"
    );
}

/// Nothing else is sent afterwards, however it is asked for.
///
/// note: this is what makes the ceiling a bound rather than a report, and it is the reason the
/// counting lives on `App` rather than in the loop below. Stopping the turn that crossed the line
/// is half of it; a caller that hands in the next line - a script, a person, an embedder with a
/// loop of its own - would start spending again, and `start_turn` is where all three of them meet.
#[tokio::test]
async fn nothing_more_is_sent_once_the_ceiling_is_reached() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 800),
        ModelResponse::text("never reached"),
    ];
    let mut app = run_capped("first\n", script, 1000, |_| {}).await.app;

    // the run is over and the `App` is not: this is the embedder that carries on calling
    let reply = app.submit("second").await;
    assert!(!app.busy, "a turn started after the ceiling was reached");
    assert!(
        reply
            .said
            .iter()
            .any(|entry| entry.text.contains("nothing more is being sent")),
        "{:?}",
        reply.said
    );
}

/// And it can be raised from a line, which is the way back for whoever set it too low.
///
/// note: `/spend` rather than a public field, because raising the ceiling has to let a stopped
/// session go again - two fields moving together, which is exactly what a setter is for.
#[tokio::test]
async fn the_ceiling_can_be_raised_and_taken_away() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 800),
        ModelResponse::text("two"),
    ];
    let mut app = run_capped("first\n", script, 1000, |_| {}).await.app;

    let reply = app.submit("/spend").await;
    assert!(
        reply.said[0]
            .text
            .contains("this run has spent 1,200 tokens of 1,000"),
        "{:?}",
        reply.said
    );

    app.submit("/spend 5000").await;
    app.submit("second").await;
    assert!(app.busy, "raising the ceiling did not let it go again");

    app.submit("/spend 0").await;
    assert_eq!(app.spend(), None);
    assert_eq!(
        app.spent(),
        1200,
        "taking the ceiling away forgot the total"
    );

    // and one set under what has already gone stops the session there. That is a reasonable thing
    // to ask for - it is one way to say "enough" - and a poor thing to find out by being refused
    let reply = app.submit("/spend 100").await;
    assert!(app.overspent());
    assert!(
        reply.said[0].text.contains("nothing more will be sent"),
        "{:?}",
        reply.said
    );

    // and the other way round: a ceiling above what has gone leaves the session able to send, and
    // is reported as one rather than as the end of it. Both sentences carry the figure and the
    // ceiling, so what separates them is this half - and a reader told a running session has
    // stopped would stop typing at it
    let reply = app.submit("/spend 5000").await;
    assert!(!app.overspent(), "raising it lets the session go again");
    assert!(
        reply.said[0].text.contains("the ceiling is 5,000 tokens;"),
        "{:?}",
        reply.said
    );
    assert!(
        !reply
            .said
            .iter()
            .any(|said| said.text.contains("nothing more")),
        "and it did not say the session is over: {:?}",
        reply.said
    );

    // a count and the thing counted agree, which is the smallest number anybody sets it to
    let reply = app.submit("/spend 1").await;
    assert!(
        reply.said[0].text.contains("the ceiling is 1 token and"),
        "{:?}",
        reply.said
    );
}

/// The total is kept whether or not anything is watching it, so a ceiling set later means what it
/// says.
///
/// note: also found by reading a live run rather than by thinking. The counting was inside the
/// ceiling's own guard, so a session with no ceiling counted nothing - and `/spend` answered
/// `0 tokens spent` after a turn that had plainly cost some, which is the figure being wrong in
/// the one place somebody looks at it. Worse than wrong: `/spend N` half way through a session
/// would then have started from zero and given away everything spent up to that point.
#[tokio::test]
async fn the_total_is_counted_with_no_ceiling_to_count_it_against() {
    let script = vec![priced(ModelResponse::text("one"), 400, 200)];
    let mut app = run("first\n", script, |_| {}).await.app;

    assert_eq!(app.spent(), 600);
    let reply = app.submit("/spend").await;
    assert!(
        reply.said[0].text.contains("this run has spent 600 tokens"),
        "{:?}",
        reply.said
    );
}

/// An endpoint that reports no figures says so, rather than holding a ceiling nothing can reach.
///
/// note: the failure this is about is silence. A run given `--spend 50000` against a provider that
/// reports no usage would go on for ever having spent nothing as far as anybody here can tell, and
/// whoever set the number would read that run as bounded. It is said once, at the first response
/// that came with nothing on it, and `--deadline` is the guard that needs nobody's cooperation.
#[tokio::test]
async fn a_ceiling_over_an_endpoint_that_reports_nothing_says_so() {
    let script = vec![ModelResponse::text("no usage on this one")];
    let run = run_capped("go\n", script, 1000, |_| {}).await;

    assert!(run.prose.contains("reports no usage"), "{}", run.prose);
    assert!(!run.prose.contains("stopping"), "{}", run.prose);
    assert!(run.prose.contains("no usage on this one"), "{}", run.prose);
}

/// And again for the next model, which may be just as silent.
#[tokio::test]
async fn a_ceiling_over_the_next_endpoint_that_reports_nothing_says_so_again() {
    let script = vec![
        ModelResponse::text("no usage on this one"),
        ModelResponse::text("nor on this one"),
    ];
    let run = run_capped("go\n/model something-else\ngo\n", script, 1000, |_| {}).await;

    assert!(run.prose.contains("nor on this one"), "{}", run.prose);
    assert_eq!(
        run.prose.matches("reports no usage").count(),
        2,
        "{}",
        run.prose
    );
}

/// A response whose stream never finished carries no figures, and that is not the endpoint's
/// silence.
///
/// note: a stream cut short never reaches the chunk its usage rides on, so after a ctrl+c, a
/// `--deadline` or a connection dropped mid-answer an endpoint that reports usage on every other
/// response was being called one that reports none - telling whoever set the ceiling that it had
/// stopped holding when it had not, and saying it once, so the next endpoint that really is silent
/// went unmentioned.
#[tokio::test]
async fn an_unfinished_response_does_not_say_the_endpoint_reports_nothing() {
    for why in ["interrupted", "cut off"] {
        let script = vec![ModelResponse {
            stop: StopReason::Other(why.to_owned()),
            ..ModelResponse::text("cut sh")
        }];
        let run = run_capped("go\n", script, 1000, |_| {}).await;

        assert!(run.prose.contains("cut sh"), "{why}: {}", run.prose);
        assert!(
            !run.prose.contains("reports no usage"),
            "{why}: {}",
            run.prose
        );
    }
}

/// A usage with no figures in it is no usage, and one with half of them is said to be half.
#[tokio::test]
async fn a_usage_without_its_figures_says_so() {
    let empty = ModelResponse {
        usage: Some(Usage::default()),
        ..ModelResponse::text("nothing on this one")
    };
    let run = run_capped("go\n", vec![empty], 1000, |_| {}).await;
    assert!(run.prose.contains("reports no usage"), "{}", run.prose);

    let half = ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(400),
            ..Default::default()
        }),
        ..ModelResponse::text("half on this one")
    };
    let run = run_capped("go\n", vec![half], 1000, |_| {}).await;
    assert!(run.prose.contains("only half"), "{}", run.prose);
    assert_eq!(run.app.spent(), 400);
}

/// A figure no endpoint would send does not wrap the total round to nothing.
#[tokio::test]
async fn a_bill_past_counting_is_counted_as_everything() {
    let script = vec![priced(ModelResponse::text("dear"), u64::MAX, 1)];
    let run = run("go\n", script, |_| {}).await;

    assert_eq!(run.app.spent(), u64::MAX);
}

/// A ceiling set under what has already gone stops a turn that is running, as crossing it does.
#[tokio::test]
async fn a_ceiling_lowered_under_the_spend_stops_the_running_turn() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
            400,
            200,
        ),
        priced(ModelResponse::text("never reached"), 400, 200),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(script, None);
    app.kernel.add_tool(Arc::new(Slow));

    app.submit("go").await;
    loop {
        let event = events.recv().await.expect("the events stopped");
        let started = event.name() == "tool.started";
        app.on_event(event);
        if started {
            break;
        }
    }
    assert!(
        app.busy && app.spent() > 0,
        "the window is not where this test thinks it is"
    );
    app.submit("/spend 100").await;

    while app.busy {
        tokio::select! {
            outcome = finished.recv() => app.on_outcome(outcome.expect("the turn never ended")),
            event = events.recv() => app.on_event(event.expect("the events stopped")),
        }
    }
    assert!(
        !app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("never reached")),
        "the turn asked again after the ceiling was put under what it had spent"
    );
}
