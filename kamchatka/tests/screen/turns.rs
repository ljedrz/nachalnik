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
    Capability, Config, Content, ContextItem, ContextState, Event, ModelInfo, ModelResponse,
    test::{ConstTool, call},
};
use ratatui::style::Color;
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
        ConstTool::new("scribble", "done").with_capabilities([Capability::fs("write")]),
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
    // a command rather than a key, because this line reaches a headless run too and there is no
    // keyboard there; `/request` opens the same page `ctrl+p` does
    assert!(screen.contains("`/request` says where"), "{screen}");
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

/// A tool result put back behind the call it answers is not news, and the request preview is where
/// it is said.
///
/// note: `context: note` writes its item while the call that writes it is still in flight, so the
/// item lands between the assistant turn and that call's result and the projector moves the result
/// back up. Nothing is lost by it - the request carries every byte it would have - and it stands
/// for as long as the note does, so every note a model took put `the request is repaired` in the
/// conversation for the rest of the session and every session resumed from it. Watched live, on
/// every one of them. The move is `Projection::reordered` now, and only the losses reach here.
#[tokio::test]
async fn a_result_put_back_behind_its_call_is_not_something_to_tell_anybody() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("write it down"));
    harness.app.kernel.push(ContextItem::assistant(
        "noting it",
        vec![call("c1", "context", json!({ "action": "note" }))],
    ));
    // the note itself, pushed by the tool before the tool's own result exists
    harness
        .app
        .kernel
        .push(ContextItem::memory("q1", "the scale factor is 7"));
    harness.app.kernel.push(ContextItem::tool_result(
        "c1".into(),
        "context",
        "written down",
        false,
    ));

    let projection = harness.app.kernel.project();
    assert_eq!(projection.reordered.len(), 1, "{projection:?}");
    assert!(
        projection.repairs.is_empty(),
        "a move takes nothing out: {:?}",
        projection.repairs
    );

    harness.send("and now?").await;
    harness.settle().await;
    assert!(
        !harness.flat().contains("repaired"),
        "nothing went wrong, so nothing is said: {}",
        harness.flat()
    );

    // and the page that answers "why is the order not my context's order?" says exactly that
    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;
    let screen = harness.sized(120, 40);
    assert!(
        screen.contains("reordered: moved item 4"),
        "the preview is where it belongs: {screen}"
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

/// The tools that read and change this session are tools like any other, and `/tools toggle` is
/// how one stops being offered.
///
/// note: this was `/introspect`, one command for these four. It switched them off by dropping the
/// handle they reach the kernel through, which also threw away what `context` was remembering - and
/// it left every other tool with `/tools drop`, which had no way back. One word for every tool
/// replaced both, so what this now checks is that these four are not special.
#[tokio::test]
async fn the_tools_that_read_this_session_go_off_and_on_like_any_other() {
    let mut harness = Harness::new([]);
    assert!(harness.app.kernel.tool_ids().is_empty());
    let before = harness.app.undecided();

    harness.app.introspect = Some(kamchatka::introspect::install(
        &harness.app.kernel,
        harness.app.policy.clone(),
        harness.app.limits.clone(),
    ));
    let offered = harness.app.kernel.tool_ids();
    assert_eq!(offered, ["context", "fork", "log", "setup"]);
    // the policy has one more subject to ask about per *operation* these tools offer, because
    // the tab reads what the registered tools declare and what they declare is what they do.
    // Reading your own items, reading the record beside them and rewriting one are different
    // questions, and each is answerable on its own row - which is the whole point of a subject
    // being `context:look` rather than the name of whichever tool happened to serve it
    // context's twelve, fork's two, setup's four and log's one
    let operations = 12 + 2 + 4 + 1;
    harness.tab(Tab::Permissions);
    assert_eq!(harness.app.undecided(), before + operations);
    let screen = harness.screen();
    assert!(
        screen.contains(&format!("{} more it will ask about", before + operations)),
        "{screen}"
    );

    harness.tab(Tab::Chat);
    for tool in &offered {
        harness.send(&format!("/tools toggle {tool}")).await;
        let screen = harness.screen();
        assert!(
            screen.contains(&format!("`{tool}` is no longer offered")),
            "the withdrawal does not name `{tool}`: {screen}"
        );
    }
    assert!(harness.app.kernel.tool_ids().is_empty());
    // and the handle they reach the kernel through has *not* gone with them, which is the
    // difference this replaced: a tool that is not offered is still the same tool, so what
    // `context` pinned is still pinned and what it could walk back it still can
    assert!(harness.app.introspect.is_some());

    // and back, which is the half there was no way to do before
    for tool in &offered {
        harness.send(&format!("/tools toggle {tool}")).await;
    }
    assert_eq!(harness.app.kernel.tool_ids(), offered);
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

/// `/step` with a message, typed into a running turn, puts nothing into the context.
///
/// note: the text was pushed before `start_step` had said whether it could step at all, so it
/// went into a turn that was already running - which is the shape `App::submit` refuses a message
/// for: an item landing between a call and its result is one most of these APIs reject outright.
/// The step was then declined in silence, so the whole call did one thing and said nothing.
#[tokio::test]
async fn a_step_with_a_message_waits_for_the_turn_the_way_a_message_does() {
    let mut harness = Harness::new([ModelResponse::text("still going")]);

    harness.send("the first question").await;
    harness.app.busy = true;
    let before = harness.app.kernel.items().len();

    harness.send("/step and this too").await;

    assert_eq!(
        harness.app.kernel.items().len(),
        before,
        "a message went into a running turn"
    );
    let screen = harness.screen();
    assert!(
        screen.contains("this message is not going in"),
        "and it says so rather than declining in silence: {screen}"
    );

    // note: read off `App::loose` rather than the screen, because the screen wraps and a wrapped
    // line cannot say what the sentence is. What it was: a string continuation written without
    // the `\`, so twenty-six spaces of source indentation sat in the middle of what somebody
    // reads. `cargo fmt` does not rewrap the inside of a literal and the assertion above passes
    // either way, so nothing here had an opinion about it
    let said = harness
        .app
        .loose
        .iter()
        .find(|entry| entry.text.contains("this message is not going in"))
        .expect("the line is in the conversation");
    assert!(
        !said.text.contains("  "),
        "the sentence has a hole in it: {:?}",
        said.text
    );
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

    harness.send("/tools toggle shell").await;

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

    // and the same word puts it back, which is what the tool being kept rather than dropped is
    // for. A `ConstTool` could be built again; an MCP server's could not
    harness.send("/tools toggle shell").await;

    assert!(
        harness
            .app
            .kernel
            .preview_request()
            .unwrap()
            .tools
            .iter()
            .any(|spec| spec.id == "shell"),
        "the next request should offer it again"
    );
    assert!(harness.screen().contains("offered again"));
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

/// A picture in the context is named in the previews rather than printed into them.
///
/// note: `/request`, `/payload` and `/raw` are the three places this program shows raw JSON, and
/// a `Content::Blob` in any of them is several megabytes of `AAAAAAAA` where somebody was looking
/// for the shape of a request. The record keeps the whole of it - these are views. What a view
/// owes is the two things the context tab's own row says: that it is there, and how big it is.
///
/// note: the elision is by shape and not by length, and the second half of this checks that: a
/// long tool result is a thing somebody opened `/request` to *read*, and cutting it would be
/// solving the wrong problem.
#[tokio::test]
async fn a_picture_is_named_in_the_previews_rather_than_printed_into_them() {
    // a real provider, because `/payload` is the provider's own rendering and a scripted one has
    // none - and because the two previews carry the blob in two different shapes: the kernel's
    // `{ media_type, data }` in `/request`, and this dialect's `data:` URI in `/payload`
    let mut harness = Harness::served_by(
        [],
        Arc::new(nachalnik_providers::OpenAiCompatible::new(
            "m",
            "http://127.0.0.1:1",
            "",
        )),
    );
    let payload = "A".repeat(4_000);
    harness
        .app
        .kernel
        .push(ContextItem::user(Content::blob("image/png", &*payload)));
    harness.drain();

    for command in ["/request", "/payload"] {
        harness.send(command).await;
        let screen = harness.flat();
        assert!(
            screen.contains("base64 blob") && screen.contains("image/png"),
            "{command} should name it: {screen}"
        );
        assert!(
            !screen.contains("AAAAAAAAAA"),
            "{command} should not print it: {screen}"
        );
        harness.press(KeyCode::Esc).await;
    }

    // and a long *result* is not touched, because that is what somebody opened this to read
    harness
        .app
        .kernel
        .push(ContextItem::user("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"));
    harness.drain();
    harness.send("/request").await;
    let screen = harness.flat();
    assert!(
        screen.contains("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
        "{screen}"
    );
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
    let _offered = kamchatka::introspect::install(
        &harness.app.kernel,
        harness.app.policy.clone(),
        Limits::default(),
    );

    for spec in harness.app.kernel.tool_specs() {
        assert!(
            !spec.description.trim().is_empty(),
            "{} says nothing",
            spec.id
        );
        // long enough to be useful, short enough to be read. `context` is twelve operations
        // over one object and earns its length - it is shorter than the two descriptions it
        // replaced, which came to 2,700 characters between them and spent a good deal of that
        // saying which of the two the other one was. A file tool that needed this much would be
        // describing something it should not be doing
        assert!(
            spec.description.len() < 2_500,
            "{} is {} chars",
            spec.id,
            spec.description.len()
        );

        // every argument says what it is for. A bare `{"type": "string"}` leaves a model to
        // guess whether a path is absolute, what `old` has to match, what a `select` accepts -
        // and a guess costs a turn each time
        //
        // note: walked into the branches rather than over the top level, which since the schema
        // grew a `call` wrapper is one property and would pass this vacuously. An argument belongs
        // to the operation that reads it now, and that is where it has to say what it is
        let inside = &spec.schema["properties"]["call"];
        let branches = match inside["anyOf"].as_array() {
            Some(several) => several.clone(),
            None => vec![inside.clone()],
        };
        let mut seen = 0;
        for branch in &branches {
            let properties = branch["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} has no object schema: {}", spec.id, spec.schema));
            for (name, field) in properties {
                seen += 1;
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
        }
        assert!(
            seen >= branches.len(),
            "`{}` declares no arguments at all, so this checked nothing",
            spec.id
        );

        // every keyword in it is one both wire formats accept. The narrower is Google's, whose
        // `Schema` is a closed set of fields rather than a JSON Schema document; this is that set
        // intersected with what OpenAI documents. One schema goes to both dialects, so a keyword
        // outside it is a 400 from one endpoint and a silent drop from the other - and neither is
        // something a person would find without reading the bytes
        const BOTH: [&str; 22] = [
            "type",
            "format",
            "title",
            "description",
            "nullable",
            "enum",
            "items",
            "maxItems",
            "minItems",
            "properties",
            "required",
            "minProperties",
            "maxProperties",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "pattern",
            "example",
            "anyOf",
            "propertyOrdering",
            "default",
        ];
        fn keywords(node: &serde_json::Value, at: &str, found: &mut Vec<String>) {
            match node {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        // the keys of `properties` are argument names, not keywords
                        match at {
                            "properties" => keywords(value, key, found),
                            _ => {
                                found.push(key.clone());
                                keywords(value, key, found);
                            }
                        }
                    }
                }
                serde_json::Value::Array(items) => {
                    items.iter().for_each(|it| keywords(it, at, found))
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        keywords(&spec.schema, "", &mut found);
        for keyword in &found {
            assert!(
                BOTH.contains(&keyword.as_str()),
                "`{}` uses `{keyword}`, which one of the two dialects does not take",
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

/// The second failure is news too, even when it is word for word the first.
///
/// note: found in a live session, in the shape that hides best. A model with one canned refusal
/// failed three times; the first was red on the screen and the other two were silent, so a person
/// typed twice into what looked like a working session and got no answer and no reason. The
/// dedup that swallowed them is real and worth keeping - one failure arrives twice, as the event
/// the kernel emitted and as the end the turn came to, the second wrapping the first - but it was
/// asking whether the *last loose line* was this error, and a loose line outlives its turn.
/// Nothing said between two turns leaves one, because a message and an answer are both drawn from
/// the context, so the first red line stays the last loose line for the rest of the session.
///
/// note: an empty script is the cheapest way to have a provider fail twice with the same words:
/// `ScriptedProvider` answers a request it has no response for with "the script ran out of
/// responses", every time.
#[tokio::test]
async fn a_repeated_failure_is_said_every_time_it_happens() {
    let mut harness = Harness::new([]);

    harness.send("what have you got?").await;
    harness.settle().await;
    let once = harness.flat().matches("ran out of responses").count();
    assert_eq!(once, 1, "one failure, said once: {}", harness.flat());

    // the same failure, a turn later, with a message of somebody's in between - which is where
    // this went wrong, because pushing an item says nothing out loud
    harness.send("try again").await;
    harness.settle().await;
    assert_eq!(
        harness.flat().matches("ran out of responses").count(),
        2,
        "the second failure is a second piece of news: {}",
        harness.flat()
    );

    // and the two reports of one failure are still one line: the turn's own end wraps the event
    // the kernel emitted, and saying both would be the same news twice
    harness.send("and again").await;
    harness.settle().await;
    assert_eq!(
        harness.flat().matches("ran out of responses").count(),
        3,
        "three failures, three lines, not six: {}",
        harness.flat()
    );
}

/// An edit's two arguments are told apart by colour rather than by reading them.
///
/// note: both shapes `readable` draws are here, because an edit is where they meet: a value with
/// newlines in it is indented under its name, and a short one shares the line with it. The names
/// stay uncoloured in either shape, so that what is green is exactly what the file would end up
/// holding.
#[tokio::test]
async fn what_an_edit_takes_out_and_what_it_puts_in_are_a_diff_s_two_colours() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "edit",
        json!({
            "path": "greet.py",
            "old": "def main():\n    print(\"ancient\")\n",
            "new": "print(\"modern\")",
        }),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("edit", "done").with_capabilities([Capability::fs("edit")]),
    ));

    harness.send("change it").await;
    harness.settle().await;

    // the JSON on the transcript above writes its quotes as `\"`, so these needles are the
    // question's own lines and nothing else
    assert_eq!(harness.style_of("print(\"ancient\")").0, Color::Red);
    assert_eq!(harness.style_of("print(\"modern\")").0, Color::Green);
    assert_eq!(harness.style_of("old:").0, Color::Reset);
    assert_eq!(harness.style_of("path: greet.py").0, Color::Reset);
}
