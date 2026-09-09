//! Driving the loop from the keyboard: stepping, the tools on offer, and the next request.
//!
//! note: the seam between a key press and a `Kernel::step`. What is checked is that the screen's
//! account of the loop is the loop's - that stepping stops where a turn would walk through, that
//! a turn out of requests says so rather than looking finished, and that what the screen shows of
//! the next request is what would go out.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{app::Tab, tools::Limits};
use nachalnik::{
    Capability, Config, ContextItem, ContextState, Event, ModelInfo, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::harness::Harness;

#[tokio::test]
async fn what_a_tool_wants_to_write_is_shown_as_the_lines_it_would_write() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "scribble",
        json!({ "path": "greet.py", "content": "def main():\n    print(\"hi\")\n" }),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("scribble", "done").with_capabilities([Capability::Write]),
    ));

    harness.send("write something").await;
    harness.settle().await;

    // this is the moment somebody decides, so the argument they have to read is put back into
    // the lines it is, rather than left as `\n` in the middle of a JSON string. The one-line
    // summary in the transcript is still a one-line summary, which is why this looks at how the
    // *question* renders rather than at the whole screen
    let screen = harness.screen();
    assert!(screen.contains("  def main():"), "{screen}");
    assert!(screen.contains("      print(\"hi\")"), "{screen}");
    assert!(
        screen.contains("path: greet.py"),
        "a short argument should read as a plain line, not as JSON: {screen}"
    );
    // and nothing is hidden by making it readable
    assert!(screen.contains("the exact JSON"), "{screen}");
}

#[tokio::test]
async fn several_repairs_at_once_are_one_line_rather_than_a_wall_of_them() {
    let mut harness = Harness::new([]);

    // one compaction pass can orphan a handful of calls, and a notice each would push the answer
    // off the screen to say one thing
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: vec![
            "dropped the call `a`".to_owned(),
            "dropped the call `b`".to_owned(),
            "dropped the call `c`".to_owned(),
        ],
    });

    let screen = harness.screen();
    assert_eq!(screen.matches("dropped the call").count(), 0, "{screen}");
    assert!(screen.contains("repaired in 3 places"), "{screen}");
    assert!(screen.contains("ctrl+p says where"), "{screen}");
}

/// A repair that stands is said once, not after every message.
///
/// note: a repair is a property of the context rather than news about a turn. The projection is
/// built afresh for every request, so the projector re-does the repair and reports it again -
/// which is right of the projector and wrong of the conversation: one tool result taken out once
/// put a line about its orphaned call after every message for the rest of a session. The trace is
/// the other way round and stays that way, because `model.requested` really did carry it each
/// time, and a log that hid a repeated entry would be the wrong thing entirely.
#[tokio::test]
async fn a_repair_that_stands_is_said_once_rather_than_every_turn() {
    let mut harness = Harness::new([]);
    let requested = |repairs: Vec<String>| Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs,
    };
    let orphan = "dropped the call `c1` (shell) from item 4: its result is not in the projection";

    harness.app.on_event(requested(vec![orphan.to_owned()]));
    let first = harness.flat();
    assert!(first.contains("dropped the call `c1`"), "{first}");
    assert!(
        first.contains("will be while this stands"),
        "the past tense reads as something this turn did: {first}"
    );

    // three more requests with the same context, and the same repair every time
    for _ in 0..3 {
        harness.app.on_event(requested(vec![orphan.to_owned()]));
    }
    assert_eq!(
        harness.flat().matches("will be while this stands").count(),
        1,
        "said once: {}",
        harness.flat()
    );

    // the trace has every one of them, because that is what a log is
    harness.tab(Tab::Trace);
    assert_eq!(
        harness.flat().matches("repaired: dropped the call").count(),
        4,
        "the log keeps them all: {}",
        harness.flat()
    );

    // a second thing going wrong is news, and so is the first one coming back
    harness.tab(Tab::Chat);
    harness.app.on_event(requested(vec![
        orphan.to_owned(),
        "flattened item 9".to_owned(),
    ]));
    assert!(
        harness.flat().contains("repaired in 2 places"),
        "{}",
        harness.flat()
    );

    harness.app.on_event(requested(Vec::new()));
    harness.app.on_event(requested(vec![orphan.to_owned()]));
    assert_eq!(
        harness.flat().matches("will be while this stands").count(),
        2,
        "it stopped standing and started again: {}",
        harness.flat()
    );
}

#[tokio::test]
async fn what_the_screen_shows_of_the_next_request_is_the_next_request() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the only thing in here"));

    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;

    let screen = harness.screen();
    assert!(screen.contains("the only thing in here"), "{screen}");
    // it is the kernel's own rendering, not a description of it
    assert!(screen.contains("\"role\""), "{screen}");
}

#[tokio::test]
async fn introspect_offers_the_two_tools_and_takes_them_away_again() {
    let mut harness = Harness::new([]);
    assert!(harness.app.kernel.tool_ids().is_empty());
    let before = harness.app.undecided();

    harness.send("/introspect").await;
    assert_eq!(harness.app.kernel.tool_ids(), ["amend", "introspect"]);
    assert!(harness.app.introspect.is_some());
    // the policy has two more subjects to ask about without being told anything, because the tab
    // reads what the registered tools declare. Two, not one: looking at your own context and
    // rewriting it are different questions, which is the whole reason there are two tools
    harness.tab(Tab::Permissions);
    assert_eq!(harness.app.undecided(), before + 2);
    let screen = harness.screen();
    assert!(
        screen.contains(&format!("{} more it will ask about", before + 2)),
        "{screen}"
    );

    harness.tab(Tab::Chat);
    harness.send("/introspect").await;
    assert!(harness.app.kernel.tool_ids().is_empty());
    // and the handle they reached the kernel through has gone with them
    assert!(harness.app.introspect.is_none());
}

#[tokio::test]
async fn a_turn_that_runs_out_of_requests_says_so_instead_of_looking_finished() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "look", json!({}))]),
            ModelResponse::text("and there it was"),
        ],
        Config {
            // one request per turn, so that carrying on is somebody's decision rather than
            // something that happens
            max_requests_per_turn: Some(1),
            ..Default::default()
        },
    );
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "nothing")));

    harness.send("look around").await;
    harness.settle().await;

    // the tool ran, and then the turn stopped without an answer; a screen that said nothing here
    // would look exactly like one where the model had finished
    let screen = harness.screen();
    assert!(screen.contains("paused after 1 requests"), "{screen}");
    assert!(screen.contains("/continue"), "{screen}");

    harness.send("/continue").await;
    harness.settle().await;
    assert!(harness.screen().contains("and there it was"));
}

#[tokio::test]
async fn stepping_stops_where_a_turn_walks_straight_through() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "look",
        json!({"at": "the state machine"}),
    )])]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "nothing to see")));

    // one transition: the request goes, the model asks for a tool, and the kernel comes to rest
    // in `Ready` - which a whole turn passes through without ever being visible. The message has
    // to come with it, because sending one on its own runs the turn and leaves nothing to step
    harness.send("/step look around").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("step \u{2192} ready"), "{screen}");
    // and it says what is about to happen, which is the only reason to stand here
    assert!(screen.contains("look"), "{screen}");
    assert!(screen.contains("the state machine"), "{screen}");
    assert_eq!(
        harness.app.kernel.pending_calls().len(),
        1,
        "the call is decided and waiting, not run"
    );
    assert!(
        harness
            .app
            .kernel
            .items()
            .iter()
            .all(|item| !matches!(item.kind, nachalnik::ContextKind::ToolResult { .. })),
        "nothing has run yet, so there is no result"
    );

    // the next transition runs it
    harness.app.start_step();
    harness.settle().await;
    // a fresh snapshot: printing the one captured before the step would show `step -> ready` at
    // exactly the moment somebody needs to see why the tool did not run
    let after = harness.screen();
    assert!(after.contains("nothing to see"), "{after}");
}

#[tokio::test]
async fn a_tool_can_stop_being_offered_without_restarting() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("shell", "ran")));
    harness.app.kernel.push(ContextItem::user("hello"));

    assert!(
        harness
            .app
            .kernel
            .preview_request()
            .unwrap()
            .tools
            .iter()
            .any(|spec| spec.id == "shell")
    );

    harness.send("/tools drop shell").await;

    assert!(
        harness
            .app
            .kernel
            .preview_request()
            .unwrap()
            .tools
            .is_empty(),
        "the next request should not offer it"
    );
    assert!(harness.screen().contains("no longer offered"));
}

#[tokio::test]
async fn the_next_request_says_why_there_is_none_rather_than_reporting_a_fault() {
    let mut harness = Harness::new([]);

    // nothing has been said yet, and `the context projects to an empty request` is the runtime's
    // sentence for a rule it is enforcing correctly - it reads as a malfunction to somebody who
    // has simply not typed anything
    harness.send("/request").await;
    let screen = harness.screen();
    assert!(screen.contains("nothing yet"), "{screen}");
    assert!(!screen.contains("projects to an empty request"), "{screen}");
    harness.press(KeyCode::Esc).await;

    // a context that is not empty and still sends nothing is a different answer, and it is the
    // one moment the list of what was left out is worth most - which is when it used to be
    // dropped, because the error returned before the list was built
    harness
        .app
        .kernel
        .push(ContextItem::user("what about this"));
    let id = harness.app.kernel.items()[0].id;
    harness.app.kernel.set_state(
        [id],
        ContextState::Excluded,
        Some("thought better of it".into()),
    );
    harness.drain();

    harness.send("/request").await;
    let screen = harness.screen();
    assert!(
        screen.contains("excluded: thought better of it"),
        "{screen}"
    );
    assert!(screen.contains("not one of the 1 item(s)"), "{screen}");
}

#[tokio::test]
async fn every_tool_says_what_it_is_and_what_each_argument_is_for() {
    let harness = Harness::new([]);
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: harness.app.policy.clone(),
            confiner: Some(std::path::PathBuf::from("/self")),
            limits: Limits::default(),
        },
        kamchatka::sandbox::Reach {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        },
        Limits::default(),
    ) {
        harness.app.kernel.add_tool(tool);
    }
    let _offered = kamchatka::introspect::install(&harness.app.kernel, Limits::default());

    for spec in harness.app.kernel.tool_specs() {
        assert!(
            !spec.description.trim().is_empty(),
            "{} says nothing",
            spec.id
        );
        // long enough to be useful, short enough to be read: the two that manage a context are
        // five actions each and earn their length; a file tool that needed this much would be
        // describing something it should not be doing
        assert!(
            spec.description.len() < 1_500,
            "{} is {} chars",
            spec.id,
            spec.description.len()
        );

        // every argument says what it is for. A bare `{"type": "string"}` leaves a model to
        // guess whether a path is absolute, what `old` has to match, what a `select` accepts -
        // and a guess costs a turn each time
        let properties = spec.schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} has no object schema", spec.id));
        for (name, field) in properties {
            let said = field
                .get("description")
                .and_then(|text| text.as_str())
                .is_some_and(|text| !text.trim().is_empty());
            assert!(
                said || field.get("enum").is_some(),
                "`{}`'s `{name}` has nothing to say for itself",
                spec.id
            );
        }

        // and none of it is written in this program's own vocabulary. What reads these has never
        // heard of the program, the crates it is built from, or a terminal somebody is sitting
        // at; a word like that reads to a model as a thing it is supposed to recognise
        let text = format!("{} {}", spec.description, spec.schema);
        for insider in ["at the terminal", "nachalnik", "kamchatka", "the kernel"] {
            assert!(
                !text.contains(insider),
                "`{}` says `{insider}` to a reader who has never heard of it",
                spec.id
            );
        }
    }
}
