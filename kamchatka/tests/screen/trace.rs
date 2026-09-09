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
