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
async fn ask(dir: &Path, action: &str, args: Value) -> String {
    ask_within(dir, Limits::default(), action, args).await
}

/// [`ask`], with the output limits a session has after `/limit`.
async fn ask_within(dir: &Path, limits: Limits, action: &str, mut args: Value) -> String {
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
        limits,
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

    // and two places that overlap are two places: `\n\n` is in three newlines twice, which a
    // count of separate matches reads as once
    std::fs::write(dir.join("b.rs"), "a\n\n\nb").expect("a file");
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "b.rs", "old": "\n\n", "new": "\n" }),
    )
    .await;
    assert!(said.contains("occurs 2 times"), "{said}");
    assert_eq!(held(&dir, "b.rs"), "a\n\n\nb", "nothing was changed");
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
    // the name a component at a time, because the answer carries the path resolved, with the
    // working directory in front of it
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

/// A link to a file a path rule has not allowed is refused by every operation that would open it,
/// and names the file, so asking for it by that name is the next call; a link to anything else is
/// the file under a second name.
///
/// note: `.env*` is one of the rules a fresh policy holds, as `ask`. A rule is about a name and a
/// link is another one, so `alias -> .env` was read, written and edited under it because the name
/// was `alias`.
#[tokio::test]
async fn a_link_to_a_file_a_rule_asks_about_is_not_opened_through() {
    let dir = scratch("files-link-past");
    std::fs::write(dir.join(".env"), "TOKEN=secret\n").expect("a file");
    std::fs::write(dir.join("notes.txt"), "plain\n").expect("a file");
    std::os::unix::fs::symlink(".env", dir.join("alias")).expect("a link");
    std::os::unix::fs::symlink("notes.txt", dir.join("other")).expect("a link");
    // a second name the rule also matches, which the policy has already been asked about
    std::os::unix::fs::symlink(".env", dir.join(".env.local")).expect("a link");

    for (action, args, doing) in [
        ("read", json!({ "path": "alias" }), "nothing was read"),
        (
            "write",
            json!({ "path": "alias", "content": "x" }),
            "nothing was written",
        ),
        (
            "edit",
            json!({ "path": "alias", "old": "secret", "new": "x" }),
            "nothing was changed",
        ),
    ] {
        let said = ask(&dir, action, args).await;
        assert!(
            said.contains("leads to `.env`") && said.contains(doing),
            "{action}: {said}"
        );
        assert!(said.contains("name it as `.env`"), "{action}: {said}");
        assert!(!said.contains("secret"), "{action} read through it: {said}");
    }
    assert_eq!(
        held(&dir, ".env"),
        "TOKEN=secret\n",
        "and nothing was written through it"
    );

    assert_eq!(
        ask(&dir, "read", json!({ "path": "other" })).await,
        "plain\n"
    );
    assert_eq!(
        ask(&dir, "read", json!({ "path": ".env.local" })).await,
        "TOKEN=secret\n",
        "the rule matched the name it was asked for by, so it was already asked about"
    );
}

/// Lines numbered from 1, each saying which it is.
fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

/// A file longer than the output limit stops at the last whole line that fits and names the line
/// to read on from, and reading on from each one gives back the whole file.
///
/// note: the limit is `/limit`'s rather than the default, both so the row a person changes is the
/// one `read` obeys and so the file can be small enough to walk in a few calls.
#[tokio::test]
async fn a_long_file_stops_at_a_line_and_says_where_to_read_on() {
    let dir = scratch("files-long");
    let file = numbered(1_000);
    std::fs::write(dir.join("long.txt"), &file).expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 2_000);

    let (mut from, mut read, mut calls) = (1, String::new(), 0);
    loop {
        calls += 1;
        let said = ask_within(
            &dir,
            limits.clone(),
            "read",
            json!({ "path": "long.txt", "from": from }),
        )
        .await;
        assert!(said.len() <= 2_000, "{} bytes past the limit", said.len());
        let (header, body) = said.split_once('\n').expect("a header, then the lines");
        assert!(header.starts_with(&format!("[lines {from}-")), "{header}");
        assert!(header.contains("of 1000"), "{header}");
        read.push_str(body);
        match header.split_once("`from: ") {
            Some((_, next)) => {
                from = next.trim_end_matches("`]").parse().expect("a line number");
                assert!(body.ends_with('\n'), "it stopped part-way through a line");
            }
            None => break,
        }
    }

    assert_eq!(read, file, "the parts are not the file");
    assert!(calls > 1, "the file fitted, so nothing was stopped");
}

/// `from` and `lines` name the lines read, and the answer says which they are; a file read whole
/// with room to spare comes back as it is, with nothing added.
#[tokio::test]
async fn a_range_is_the_lines_it_names() {
    let dir = scratch("files-range");
    std::fs::write(dir.join("ten.txt"), numbered(10)).expect("a file");

    let whole = ask(&dir, "read", json!({ "path": "ten.txt" })).await;
    assert_eq!(whole, numbered(10));

    let some = ask(
        &dir,
        "read",
        json!({ "path": "ten.txt", "from": 3, "lines": 2 }),
    )
    .await;
    assert_eq!(some, "[lines 3-4 of 10]\nline 3\nline 4\n");
    // quoted, as models write numbers often enough
    let tail = ask(&dir, "read", json!({ "path": "ten.txt", "from": "9" })).await;
    assert_eq!(tail, "[lines 9-10 of 10]\nline 9\nline 10\n");
    // more asked for than there is is the rest, and says where it ended
    let past = ask(
        &dir,
        "read",
        json!({ "path": "ten.txt", "from": 8, "lines": 50 }),
    )
    .await;
    assert_eq!(past, "[lines 8-10 of 10]\nline 8\nline 9\nline 10\n");
}

/// A range that names no line is refused, saying why, and so is one that is not a number.
#[tokio::test]
async fn a_range_that_names_no_line_is_refused() {
    let dir = scratch("files-no-range");
    std::fs::write(dir.join("ten.txt"), numbered(10)).expect("a file");
    std::fs::write(dir.join("empty.txt"), "").expect("a file");

    for (args, says) in [
        (
            json!({ "path": "ten.txt", "from": 0 }),
            "counts lines from 1",
        ),
        (json!({ "path": "ten.txt", "lines": 0 }), "reads none"),
        (json!({ "path": "ten.txt", "from": 11 }), "has 10 line(s)"),
        (
            json!({ "path": "ten.txt", "from": "three" }),
            "whole number",
        ),
        (json!({ "path": "empty.txt", "from": 1 }), "is empty"),
    ] {
        let said = ask(&dir, "read", args.clone()).await;
        assert!(said.contains(says), "{args}: {said}");
        assert!(!said.contains("line 1\n"), "{args} read something: {said}");
    }
}

/// A file too large to count is read a part at a time all the same, and says more follows rather
/// than how much; read near its end, it says how many lines it has.
#[tokio::test]
async fn a_file_past_what_is_counted_is_read_a_part_at_a_time() {
    let dir = scratch("files-huge");
    let lines = kamchatka::tools::KEPT / 16 + 1_000;
    let file: String = (1..=lines).map(|n| format!("{n:>15}\n")).collect();
    assert!(file.len() > kamchatka::tools::KEPT);
    std::fs::write(dir.join("huge.log"), &file).expect("a file");

    let start = ask(&dir, "read", json!({ "path": "huge.log" })).await;
    let (header, body) = start.split_once('\n').expect("a header");
    assert!(header.starts_with("[lines 1-"), "{header}");
    assert!(
        !header.contains(" of "),
        "it was counted to the end: {header}"
    );
    assert!(header.contains("read on with `from: "), "{header}");
    assert!(body.starts_with(&format!("{:>15}\n", 1)));

    let end = ask(
        &dir,
        "read",
        json!({ "path": "huge.log", "from": lines - 1, "lines": 5 }),
    )
    .await;
    assert_eq!(
        end,
        format!(
            "[lines {}-{lines} of {lines}]\n{:>15}\n{lines:>15}\n",
            lines - 1,
            lines - 1
        )
    );

    let middle = ask(
        &dir,
        "read",
        json!({ "path": "huge.log", "from": 5, "lines": 2 }),
    )
    .await;
    assert_eq!(
        middle,
        format!("[lines 5-6, and more after them]\n{:>15}\n{:>15}\n", 5, 6)
    );
}

/// A line longer than the output limit on its own is shown from its start and said to be cut,
/// and the cut does not split a character.
#[tokio::test]
async fn a_line_longer_than_the_limit_is_shown_from_its_start() {
    let dir = scratch("files-wide");
    // three bytes a character, so a cut at an arbitrary byte lands inside one two times in three
    std::fs::write(
        dir.join("wide.txt"),
        format!("{}\nnext\n", "€".repeat(20_000)),
    )
    .expect("a file");

    let said = ask(&dir, "read", json!({ "path": "wide.txt" })).await;
    let (header, body) = said.split_once('\n').expect("a header");
    assert!(header.contains("line 1 of 2 is longer than"), "{header}");
    assert!(said.len() <= 32_000, "{} bytes", said.len());
    assert!(
        !body.is_empty() && body.chars().all(|c| c == '€'),
        "{}",
        &body[..30]
    );
    // and the line after it is where reading on starts
    let next = ask(&dir, "read", json!({ "path": "wide.txt", "from": 2 })).await;
    assert_eq!(next, "[lines 2-2 of 2]\nnext\n");
}

/// `write` and `edit` put a new file where the old one was rather than emptying it first, and the
/// new one has the old one's permissions; nothing is left beside it.
///
/// note: a new inode is what a rename leaves and an open that truncates does not, which is how
/// the test can see that the file was never short: a full disk or a killed process between the
/// truncation and the last byte is not something a test can arrange.
#[tokio::test]
async fn a_write_replaces_the_file_rather_than_emptying_it_first() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = scratch("files-replaced");
    let file = dir.join("run.sh");
    std::fs::write(&file, "echo one\n").expect("a file");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).expect("its mode");
    let before = std::fs::metadata(&file).expect("it is there").ino();

    let said = ask(
        &dir,
        "write",
        json!({ "path": "run.sh", "content": "echo two\n" }),
    )
    .await;
    assert!(said.contains("wrote 9 bytes"), "{said}");
    let written = std::fs::metadata(&file).expect("it is there");
    assert_ne!(written.ino(), before, "a new file stands at the path");
    assert_eq!(
        written.permissions().mode() & 0o777,
        0o750,
        "with the old one's mode"
    );
    assert_eq!(held(&dir, "run.sh"), "echo two\n");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "run.sh", "old": "two", "new": "three" }),
    )
    .await;
    assert!(said.contains("replaced one occurrence"), "{said}");
    assert_ne!(
        std::fs::metadata(&file).expect("it is there").ino(),
        written.ino(),
        "and an edit is the same"
    );
    assert_eq!(held(&dir, "run.sh"), "echo three\n");

    let left: Vec<_> = std::fs::read_dir(&dir)
        .expect("the directory")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(left, ["run.sh"], "nothing is left beside it");
}

/// A file another hard link shares is written where it is, so both names still show one file.
///
/// note: a rename would give this name a new file and leave the other name holding the old
/// contents, which reads as the write not having happened to whoever looks there.
#[tokio::test]
async fn a_file_with_another_link_is_written_where_it_is() {
    use std::os::unix::fs::MetadataExt;

    let dir = scratch("files-linked");
    std::fs::write(dir.join("a.txt"), "old\n").expect("a file");
    std::fs::hard_link(dir.join("a.txt"), dir.join("b.txt")).expect("a second name for it");
    let before = std::fs::metadata(dir.join("a.txt"))
        .expect("it is there")
        .ino();

    ask(
        &dir,
        "write",
        json!({ "path": "a.txt", "content": "new\n" }),
    )
    .await;

    assert_eq!(
        std::fs::metadata(dir.join("a.txt"))
            .expect("it is there")
            .ino(),
        before
    );
    assert_eq!(
        held(&dir, "b.txt"),
        "new\n",
        "the other name shows the write"
    );
}

/// A file its owner made read-only is refused, as the open always refused it, and one whose name
/// leaves no room for the temporary's suffix is still written.
///
/// note: both are what a rename changed. The new file is made in the directory, which the
/// read-only file does not protect, so it stepped round the refusal; and its name is longer than
/// the file's, which `NAME_MAX` can refuse where the file's own name was fine.
#[tokio::test]
async fn a_read_only_file_is_refused_and_a_long_name_is_written() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("files-protected");
    std::fs::write(dir.join("kept.txt"), "mine\n").expect("a file");
    std::fs::set_permissions(dir.join("kept.txt"), std::fs::Permissions::from_mode(0o444))
        .expect("made read-only");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "kept.txt", "content": "yours\n" }),
    )
    .await;
    assert!(said.contains("denied"), "{said}");
    assert_eq!(held(&dir, "kept.txt"), "mine\n");

    let long = format!("{}.txt", "n".repeat(240));
    let said = ask(&dir, "write", json!({ "path": long, "content": "one\n" })).await;
    assert!(said.contains("wrote 4 bytes"), "{said}");
    assert_eq!(held(&dir, &long), "one\n");
}

/// A pipe is refused by every operation that would open it, rather than waited on for ever.
///
/// note: an open of a pipe blocks until somebody writes to it, and the open is not a place the
/// interrupt reaches - so `mkfifo p` and `fs read p` was a turn nobody could stop. `grep` over a
/// directory holding one opened it the same way.
///
/// note: each call on a thread and a runtime of its own, because what is being tested for blocks a
/// thread rather than awaiting: a timeout on the same runtime would never get to fire, and the
/// failure would be a suite that hangs rather than a test that says so.
#[test]
fn a_pipe_is_refused_rather_than_waited_on() {
    let dir = scratch("files-pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(dir.join("p"))
        .status()
        .expect("`mkfifo` is on the path");
    assert!(made.success());
    std::fs::write(dir.join("a.txt"), "needle\n").expect("a file beside it");

    let asked = |action: &'static str, args: Value| {
        let (sent, heard) = std::sync::mpsc::channel();
        let dir = dir.clone();
        std::thread::spawn(move || {
            let said = tokio::runtime::Runtime::new()
                .expect("a runtime")
                .block_on(ask(&dir, action, args));
            let _ = sent.send(said);
        });
        heard
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("`{action}` waited on the pipe"))
    };

    for (action, args) in [
        ("read", json!({ "path": "p" })),
        ("write", json!({ "path": "p", "content": "x" })),
        ("edit", json!({ "path": "p", "old": "a", "new": "b" })),
    ] {
        let said = asked(action, args);
        assert!(said.contains("not a regular file"), "{action}: {said}");
    }
    let found = asked("grep", json!({ "pattern": "needle", "path": "." }));
    assert!(found.contains("a.txt"), "{found}");
}
