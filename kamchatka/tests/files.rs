//! `read`, `write` and `edit`, against real files.
//!
//! note: the three `fs` does that are not walks had no suite of their own - the walks have
//! `search.rs` and these were reached only by the smoke test in `introspect/shared.rs`, which calls each
//! tool once to see that it answers. What that misses is the answer: `edit` replaced the first of
//! however many matches there were and said `replaced one occurrence`, and nothing anywhere
//! compared that sentence with what the file then held.
//!
//! note: through `tools::builtin` for the reason `search.rs` gives: these are not public types,
//! and what is under test includes the wiring that hands them the session's reach.

mod common;

use std::{path::Path, sync::Arc};

use common::scratch;
use kamchatka::{
    sandbox::Reach,
    tools::{Careful, Limits, Shell},
};
use nachalnik::{OutputSink, ToolCall, test::call};
use serde_json::{Value, json};

/// Calls `fs` the way a session does, and hands back what the model would read.
async fn ask(dir: &Path, action: &str, mut args: Value) -> String {
    let tools = kamchatka::tools::builtin(
        Shell {
            workdir: dir.to_path_buf(),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        Reach {
            workdir: dir.to_path_buf(),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        },
        Limits::default(),
    );
    let found = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");

    args["action"] = Value::String(action.to_owned());
    let call: ToolCall = call("c1", "fs", args);
    found
        .invoke(&call, OutputSink::disconnected())
        .await
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned()
}

/// What a file holds now.
fn held(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("the file is there")
}

/// An `old` that names two places changes neither, and says how many it named.
///
/// note: the argument asks for enough of the surrounding lines to make it the only match, and
/// nothing checked. The first was replaced, the second stayed, and the answer read as a finished
/// edit - which is the half nobody goes back for, because a model told its change landed does not
/// read the file again.
#[tokio::test]
async fn an_edit_that_could_mean_two_places_changes_neither() {
    let dir = scratch("files-edit-twice");
    std::fs::write(dir.join("a.rs"), "let x = 1;\nlet y = 2;\nlet x = 1;\n").expect("a file");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "a.rs", "old": "let x = 1;", "new": "let x = 9;" }),
    )
    .await;

    assert!(said.contains("occurs 2 times"), "it says how many: {said}");
    assert!(
        said.contains("include enough of the lines"),
        "and what to do about it: {said}"
    );
    assert_eq!(
        held(&dir, "a.rs"),
        "let x = 1;\nlet y = 2;\nlet x = 1;\n",
        "nothing was changed"
    );

    // named with enough around it, it is one place and it is changed
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "a.rs", "old": "let y = 2;\nlet x = 1;", "new": "let y = 2;\nlet x = 9;" }),
    )
    .await;
    assert!(said.contains("replaced one occurrence"), "{said}");
    assert_eq!(held(&dir, "a.rs"), "let x = 1;\nlet y = 2;\nlet x = 9;\n");
}

/// An empty `old` names no text, rather than the front of the file.
#[tokio::test]
async fn an_empty_edit_is_refused_rather_than_written_at_the_front() {
    let dir = scratch("files-edit-empty");
    std::fs::write(dir.join("a.rs"), "fn go() {}\n").expect("a file");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "a.rs", "old": "", "new": "// oh\n" }),
    )
    .await;

    assert!(said.contains("names no text"), "{said}");
    assert_eq!(
        held(&dir, "a.rs"),
        "fn go() {}\n",
        "and nothing was written"
    );
}

/// An `old` that is not there is said to be not there, and the file is left alone.
#[tokio::test]
async fn an_edit_that_matches_nothing_says_so() {
    let dir = scratch("files-edit-absent");
    std::fs::write(dir.join("a.rs"), "fn go() {}\n").expect("a file");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "a.rs", "old": "fn stop() {}", "new": "" }),
    )
    .await;

    assert!(said.contains("does not occur"), "{said}");
    assert_eq!(held(&dir, "a.rs"), "fn go() {}\n");
}

/// `write` puts the whole file there, and `read` reads it back.
///
/// note: a directory that is not there is not made on the way, and the answer is the operating
/// system's own sentence about it. That is the honest half of writing a file nobody asked for.
#[tokio::test]
async fn what_write_put_there_is_what_read_hands_back() {
    let dir = scratch("files-round-trip");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "new.md", "content": "one\ntwo\n" }),
    )
    .await;
    assert!(said.contains("wrote 8 bytes"), "it says how much: {said}");
    assert_eq!(held(&dir, "new.md"), "one\ntwo\n");

    let read = ask(&dir, "read", json!({ "path": "new.md" })).await;
    assert_eq!(read, "one\ntwo\n");

    let missing = ask(
        &dir,
        "write",
        json!({ "path": "notes/new.md", "content": "one\n" }),
    )
    .await;
    // the name a component at a time, because the answer carries the path as the operating system
    // spells it: `notes\new.md` under a `\\?\D:\...` prefix on Windows, where this asked for
    // `notes/new.md` and failed on the separator rather than on anything it is about
    for part in ["notes", "new.md"] {
        assert!(
            missing.contains(part),
            "it names the path it could not write: {missing}"
        );
    }
    assert!(
        !dir.join("notes").exists(),
        "the directory above a file is not made for it"
    );
}
