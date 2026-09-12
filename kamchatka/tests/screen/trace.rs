//! The trace tab: every event the session log is made of, as it happens.
//!
//! note: the screen's copy of the paper trail, so what is checked is that it is a copy - the same
//! events under the same names, in the order they were applied, with nothing invented and nothing
//! a chatty tool can push off the end.

use std::{sync::Arc, time::Duration};

use kamchatka::app::Tab;
use nachalnik::{
    Event, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::harness::Harness;

#[tokio::test]
async fn a_chatty_tool_does_not_wipe_out_the_trace() {
    let mut harness = Harness::new([]);

    harness.app.on_event(Event::ToolStarted {
        call: nachalnik::ToolCallId("c1".to_owned()),
        tool: "shell".to_owned(),
    });
    // `cat` of a thousand lines really did erase the whole trace, one `tool.output` at a time
    for _ in 0..900 {
        harness.app.on_event(Event::ToolOutput {
            call: nachalnik::ToolCallId("c1".to_owned()),
            tool: "shell".to_owned(),
            chunk: "a line of it\n".to_owned(),
        });
    }

    assert_eq!(
        harness.app.trace.len(),
        2,
        "the started, and one line counting the output up"
    );
    harness.tab(Tab::Trace);
    let screen = harness.screen();
    assert!(screen.contains("tool.started"), "{screen}");
    assert!(screen.contains("11,700 bytes so far"), "{screen}");
}

#[tokio::test]
async fn the_trace_shows_the_events_the_session_log_is_made_of() {
    let mut harness = Harness::new([ModelResponse::text("done")]);

    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    let screen = harness.sized(120, 30);
    assert!(screen.contains("model.requested"), "{screen}");
    assert!(screen.contains("model.finished"), "{screen}");
    assert!(screen.contains("state.changed"), "{screen}");

    // nothing is cut off with an ellipsis in the middle of the part worth reading, which is what
    // the pane did while it was sharing forty columns with the context
    assert!(
        screen.contains("requesting → finished"),
        "a state change is truncated: {screen}"
    );
    assert!(
        screen.contains("/save keeps them all"),
        "the pane should say where the rest of them are: {screen}"
    );
}

#[tokio::test]
async fn every_event_the_session_recorded_is_on_the_trace_tab() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "look", json!({}))]),
        ModelResponse::text("and there it was"),
    ]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "a thing")));

    harness.send("look").await;
    harness.settle().await;
    // the recount at the end of a turn is itself an event, and it is emitted by the outcome the
    // line above has just handed over
    harness.drain();
    harness.tab(Tab::Trace);
    let screen = harness.sized(120, 40);

    // the claim is the whole log, not a selection of it: whatever the runtime recorded, this tab
    // draws under the same name. Two exceptions, both of them nameable. The streaming fragments
    // are coalesced into one counting line and are not in the log either by default
    // (`Config::record_progress`), and the chat tab is where they are read; and `session.started`
    // is emitted by the kernel's constructor, before a subscriber to it can exist at all
    let recorded: std::collections::BTreeSet<String> = harness.app.kernel.with_history(|session| {
        session
            .records()
            .map(|record| record.event.name().to_owned())
            .collect()
    });
    assert!(recorded.len() > 8, "a turn records more than {recorded:?}");
    for name in recorded.iter().filter(|name| *name != "session.started") {
        assert!(
            screen.contains(name.as_str()),
            "`{name}` is not on the trace: {screen}"
        );
    }
}

#[tokio::test]
async fn the_trace_says_what_each_event_carries_and_which_step_was_slow() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("a bone"),
    ]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("dig", "a bone")));
    harness.send("dig").await;
    harness.settle().await;

    // the one event that carries content, and the only place the old text survives once the undo
    // window closes. It printed its own name against an empty line
    let id = harness.app.kernel.items()[0].id;
    harness
        .app
        .kernel
        .replace(id, "dig, please")
        .expect("the item is there");
    assert!(harness.app.kernel.undo());
    harness.drain();

    harness.tab(Tab::Trace);
    let screen = harness.sized(110, 30);
    assert!(screen.contains("context.replaced"), "{screen}");
    assert!(screen.contains("it said: dig"), "{screen}");
    assert!(screen.contains("context.undone"), "{screen}");
    assert!(screen.contains("put back as they were"), "{screen}");

    // and nothing in the pane is a name with nothing beside it
    for line in screen.lines().filter(|line| line.contains('.')) {
        let Some(name) = line.split_whitespace().find(|word| {
            word.contains('.') && word.chars().all(|c| c.is_ascii_lowercase() || c == '.')
        }) else {
            continue;
        };
        let after = line.split_once(name).map(|(_, rest)| rest).unwrap_or("");
        assert!(
            after.trim_end_matches(['│', ' ', '█']).trim().len() > 1,
            "`{name}` says nothing: {line}"
        );
    }

    // the gap column: blank for the frame-to-frame majority, and there for the few that waited
    harness.tab(Tab::Trace);
    let last = harness.app.trace.len() - 1;
    harness.app.trace[last].at = std::time::Instant::now()
        .checked_add(Duration::from_secs(4))
        .expect("a clock");
    let screen = harness.sized(110, 30);
    assert!(screen.contains("+4.0s"), "{screen}");

    // and a window with no room for it spends its columns on what happened instead
    let narrow = harness.sized(46, 30);
    assert!(!narrow.contains("+4.0s"), "{narrow}");
}

#[tokio::test]
async fn every_line_says_when_it_happened_and_not_only_how_long_it_took() {
    /// `HH:MM:SS` anywhere in a line - the pane is drawn inside a border, so it does not start
    /// in the first column.
    fn stamped(line: &str) -> bool {
        let glyphs: Vec<char> = line.chars().collect();
        glyphs.windows(8).any(|at| {
            at[2] == ':'
                && at[5] == ':'
                && [0, 1, 3, 4, 6, 7].iter().all(|n| at[*n].is_ascii_digit())
        })
    }

    let mut harness = Harness::new([ModelResponse::text("done")]);

    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    // wide enough for both columns: the time of day answers "when", the gap answers "which step
    // was slow", and neither can be got from the other
    let screen = harness.sized(120, 30);
    let count = screen.lines().filter(|line| stamped(line)).count();
    assert!(
        count >= 3,
        "the trace should carry a time of day on every event, found {count}: {screen}"
    );

    // and a window too narrow for it spends its columns on what happened rather than on when
    let narrow = harness.sized(48, 30);
    assert!(
        narrow.contains("model.requested"),
        "the names survive a narrow window: {narrow}"
    );
    assert!(
        !narrow.lines().any(stamped),
        "a narrow window should drop the clock, not the event: {narrow}"
    );
}

#[tokio::test]
async fn a_run_that_outlasts_a_day_says_which_day_each_line_is_on() {
    use std::time::{Duration, SystemTime};

    use kamchatka::app::Traced;

    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;

    // the shape of run this clock is for: one that was still going the next morning. Without the
    // date these two are both `00:00:01` and nothing on the screen tells them apart
    let midnight = SystemTime::UNIX_EPOCH + Duration::from_secs(1_789_171_201);
    harness.app.trace.clear();
    for (n, name) in ["tool.started", "tool.finished"].into_iter().enumerate() {
        harness.app.trace.push_back(Traced {
            name: name.to_owned(),
            detail: "the long one".to_owned(),
            at: std::time::Instant::now(),
            wall: midnight + Duration::from_secs(86_400 * n as u64),
            after_a_person: false,
        });
    }

    harness.tab(Tab::Trace);
    let screen = harness.sized(120, 30);

    let dates: Vec<&str> = screen
        .lines()
        .filter(|line| line.contains("──") && line.contains("-"))
        .collect();
    assert!(
        dates.len() >= 2,
        "two days should be marked, found {}: {screen}",
        dates.len()
    );
    assert_ne!(
        dates[0], dates[1],
        "the second day should be a different date: {screen}"
    );
}

/// The gap column answers "which step was slow", and a session spends most of its wall time in
/// two places where nothing is stepping: a question nobody has answered yet, and the wait for the
/// next message. `+11.0s permission.decided` is a person reading, and it was the biggest figure
/// in the column - so the one number nobody should act on was the one the eye went to first.
#[tokio::test]
async fn the_gap_column_says_nothing_about_how_long_a_person_took() {
    use std::time::{Duration, Instant, SystemTime};

    use kamchatka::app::Traced;

    let mut harness = Harness::new([ModelResponse::text("done")]);

    // eleven seconds of somebody reading a question, and then four of the program doing the thing
    // they allowed. Both are real waits; only one of them is a step
    let start = Instant::now();
    let wall = SystemTime::now();
    harness.app.trace.clear();
    for (name, after, theirs) in [
        ("permission.requested", 0, false),
        ("permission.decided", 11_000, true),
        ("tool.started", 11_010, false),
        ("tool.finished", 15_010, false),
    ] {
        harness.app.trace.push_back(Traced {
            name: name.to_owned(),
            detail: "shell".to_owned(),
            at: start + Duration::from_millis(after),
            wall: wall + Duration::from_millis(after),
            after_a_person: theirs,
        });
    }

    harness.tab(Tab::Trace);
    let screen = harness.sized(120, 30);

    // the eleven seconds somebody spent deciding is drawn nowhere
    assert!(
        !screen.contains("11.0s"),
        "how long a person took is not a step: {screen}"
    );

    // and the four the tool spent is drawn, on the line it belongs to
    let gapped: Vec<&str> = screen
        .lines()
        .filter(|line| line.contains("+4.0s"))
        .collect();
    assert_eq!(
        gapped.len(),
        1,
        "the program's wait is still drawn: {screen}"
    );
    assert!(
        gapped[0].contains("tool.finished"),
        "beside the line it ended on: {screen}"
    );

    // the clock stays on the answered question, because *when* it was answered is a real question
    // - it is only *how long* that is nobody's business
    let decided = screen
        .lines()
        .find(|line| line.contains("permission.decided"))
        .unwrap_or_else(|| panic!("the line is drawn: {screen}"));
    assert!(
        decided.chars().filter(|c| *c == ':').count() >= 2,
        "the wall clock stays: {decided}"
    );
}
