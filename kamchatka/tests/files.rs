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

use std::path::Path;

use common::scratch;
use kamchatka::tools::Limits;
use nachalnik::{OutputSink, ToolCall, test::call};
use serde_json::{Value, json};

/// Calls `fs` the way a session does, and hands back what the model would read.
async fn ask(dir: &Path, action: &str, args: Value) -> String {
    ask_within(dir, Limits::default(), action, args).await
}

/// [`ask`], with the output limits a session has after `/limit`.
async fn ask_within(dir: &Path, limits: Limits, action: &str, args: Value) -> String {
    answered(dir, limits, true, action, args).await
}

/// [`ask`], in a session run with `--no-sandbox`.
async fn ask_unconfined(dir: &Path, action: &str, args: Value) -> String {
    answered(dir, Limits::default(), false, action, args).await
}

async fn answered(dir: &Path, limits: Limits, confined: bool, action: &str, args: Value) -> String {
    outcome(dir, limits, confined, action, args)
        .await
        .content
        .to_text()
        .into_owned()
}

/// [`answered`], whole: the text and whether the model is shown it as an error.
async fn outcome(
    dir: &Path,
    limits: Limits,
    confined: bool,
    action: &str,
    mut args: Value,
) -> nachalnik::ToolOutput {
    let tools = common::builtin(dir, confined, limits);
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
    assert!(
        said.contains("to add text"),
        "and says how to add some: {said}"
    );
    assert_eq!(
        held(&dir, "a.rs"),
        "fn go() {}\n",
        "and nothing was written"
    );
}

/// An edit with a `new` and no `old` says how to add text, and writes nothing.
#[tokio::test]
async fn an_edit_with_nothing_to_replace_says_how_to_add_text() {
    let dir = scratch("files-edit-no-old");
    std::fs::write(dir.join("a.rs"), "fn go() {}\n").expect("a file");

    let said = ask(&dir, "edit", json!({ "path": "a.rs", "new": "// oh\n" })).await;

    assert!(said.contains("the `old` argument is required"), "{said}");
    assert!(said.contains("to add text"), "{said}");
    assert_eq!(held(&dir, "a.rs"), "fn go() {}\n");
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

/// A file with one byte in it that is not text is said to be one, where it is, and what else to
/// do with it - rather than answering with the operating system's sentence about a stream.
///
/// note: `stream did not contain valid UTF-8` is what a byte outside UTF-8 gets, and it is
/// neither the line nor the byte: a model that cannot tell a file with one Latin-1 byte in it from
/// a binary one has nothing to search and reaches for `shell` to find out. And an `edit` that
/// refused this way left nothing to corrupt, which is the half that had to hold: `edit` writes
/// the whole file back, so a whole read that gave up on the bad byte would have written a file
/// with that byte gone.
#[tokio::test]
async fn a_file_that_is_not_text_says_which_byte_and_what_to_do() {
    let dir = scratch("files-not-text");
    // a line of text, a byte that is not UTF-8, and another line of it
    let before = b"fn go() {}\n\xff\nfn stop() {}\n";
    std::fs::write(dir.join("latin.rs"), before).expect("a file");

    for (action, args, where_) in [
        (
            "read",
            json!({ "path": "latin.rs" }),
            // a `read` is counting lines and says which line
            "byte 0 of line 2 of it is not UTF-8",
        ),
        (
            "edit",
            json!({ "path": "latin.rs", "old": "go", "new": "walk" }),
            // and an `edit` is not, and says which byte
            "byte 11 of it is not UTF-8",
        ),
    ] {
        let said = ask(&dir, action, args).await;
        assert!(said.contains(where_), "{action} says where: {said}");
        assert!(
            said.contains("`grep` searches a file whatever it holds"),
            "{action} says what to do: {said}"
        );
        assert!(!said.contains("valid UTF-8"), "{action}: {said}");
    }
    assert_eq!(
        std::fs::read(dir.join("latin.rs"))
            .expect("it is there")
            .as_slice(),
        before,
        "an edit that could not read the file did not write one"
    );

    // and a file of bytes that are all valid UTF-8 code points, and so text, is read as one
    std::fs::write(dir.join("bytes.bin"), b"\x00\x01\x02").expect("a file");
    let said = ask(&dir, "read", json!({ "path": "bytes.bin" })).await;
    assert_eq!(said, "\u{0}\u{1}\u{2}", "a byte below 0x80 is a character");

    // and one whose first byte is not, is said so
    std::fs::write(dir.join("latin1.bin"), b"\xff\x01\x02").expect("a file");
    let said = ask(&dir, "read", json!({ "path": "latin1.bin" })).await;
    assert!(
        said.contains("byte 0 of line 1 of it is not UTF-8"),
        "[{said}]"
    );

    // and one that is not text part-way along a line says where in that line, which is the byte
    // after the last newline before it rather than the one the whole file is measured from
    let mut named = b"0123456789\n".to_vec();
    named.extend_from_slice(b"aaaaa");
    named.push(0xff);
    named.extend_from_slice(b"\nlast\n");
    std::fs::write(dir.join("midline.bin"), &named).expect("a file");
    let said = ask(&dir, "read", json!({ "path": "midline.bin" })).await;
    assert!(
        said.contains("byte 5 of line 2 of it is not UTF-8"),
        "[{said}]"
    );
}

/// A first line longer than the output limit that is not text is said to be, rather than shown
/// from its start as though it were a line of text cut short.
///
/// note: a line shown from its start is cut where the limit is, which can land inside a character,
/// and the cut is not the file. A byte that is not UTF-8 is not a cut: dropping the rest of the
/// line would answer with the start of a line that is not there, and say nothing about the byte a
/// model has to find with `grep`.
#[tokio::test]
async fn a_line_too_long_to_show_that_is_not_text_says_so() {
    let dir = scratch("files-wide-not-text");
    let mut before = vec![0xff];
    before.extend_from_slice("x".repeat(200).as_bytes());
    before.push(b'\n');
    before.extend_from_slice(b"next\n");
    std::fs::write(dir.join("wide.bin"), &before).expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 100);

    let said = ask_within(&dir, limits, "read", json!({ "path": "wide.bin" })).await;

    assert!(
        said.contains("byte 0 of line 1 of it is not UTF-8"),
        "the byte is named: {said}"
    );
    assert!(
        !said.contains("is longer than the output limit"),
        "and it is not answered as a line cut short: {said}"
    );
}

/// An `old` that is in the file but spelled for another line ending says so, rather than
/// answering that it is not there.
///
/// note: `old` is exact text, and a file written on another machine, or by a tool with its own
/// idea of a line ending, holds `\r\n` where a model writes `\n`. Told only "does not occur", the
/// model either gives up on the edit or rewrites the text and hits the same refusal. A file with
/// lines of both endings says nothing, since neither spelling is the one it holds.
#[tokio::test]
async fn an_edit_spelled_for_the_wrong_line_ending_says_so() {
    let dir = scratch("files-crlf");
    std::fs::write(dir.join("win.txt"), b"one\r\nfn go() {}\r\n").expect("a file");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "win.txt", "old": "one\nfn go() {}", "new": "two\nfn go() {}" }),
    )
    .await;

    assert!(said.contains("`old` does not occur"), "{said}");
    assert!(
        said.contains("the file ends its lines with CRLF, where `old` has LF"),
        "{said}"
    );
    assert_eq!(
        std::fs::read(dir.join("win.txt")).expect("it is there"),
        b"one\r\nfn go() {}\r\n",
        "and nothing was changed"
    );

    // spelled the file's way it goes through, which is the other half of the hint
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "win.txt", "old": "one\r\nfn go() {}", "new": "two\r\nfn go() {}" }),
    )
    .await;
    assert!(said.contains("replaced one occurrence"), "{said}");
    assert_eq!(
        std::fs::read(dir.join("win.txt")).expect("it is there"),
        b"two\r\nfn go() {}\r\n"
    );

    // and the same hint the other way round, which is the arm of it that says the file's ending
    // rather than `old`'s: a model handed a Unix file and an `old` written for a Windows one
    std::fs::write(dir.join("unix.txt"), b"one\nfn go() {}\n").expect("a file");
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "unix.txt", "old": "one\r\nfn go() {}", "new": "two\nfn go() {}" }),
    )
    .await;
    assert!(said.contains("`old` does not occur"), "{said}");
    assert!(
        said.contains("the file ends its lines with LF, where `old` has CRLF"),
        "{said}"
    );
    assert_eq!(
        std::fs::read(dir.join("unix.txt")).expect("it is there"),
        b"one\nfn go() {}\n",
        "and nothing was changed"
    );

    // and an `old` that is simply not there is not told about line endings
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "win.txt", "old": "three\nfn go() {}", "new": "x" }),
    )
    .await;
    assert!(said.contains("does not occur"), "{said}");
    assert!(!said.contains("CRLF"), "{said}");
}

/// A file with nothing in it read whole is a file with nothing in it, rather than a refusal about
/// the line to start from - which is the answer to a question about `from` this call did not ask.
#[tokio::test]
async fn an_empty_file_read_whole_is_empty_rather_than_refused() {
    let dir = scratch("files-empty");
    std::fs::write(dir.join("empty.txt"), "").expect("a file");

    let read = outcome(
        &dir,
        Limits::default(),
        true,
        "read",
        json!({ "path": "empty.txt" }),
    )
    .await;
    let said = read.content.to_text();
    assert!(said.contains("the file is empty"), "{said}");
    assert!(!said.contains("no line to start from"), "{said}");
    assert!(
        !read.is_error,
        "an empty file read is not a failed read: {said}"
    );

    // and a range that names no line of one is still about the range
    let said = ask(&dir, "read", json!({ "path": "empty.txt", "from": 2 })).await;
    assert!(said.contains("no line to start from"), "{said}");
}

/// A directory is refused by every operation that would open it, each naming the tool to use
/// instead - `glob` for a read, and nothing for a write, where the name of the directory is what
/// a caller needs and `shell` reads nothing that is being written.
#[tokio::test]
async fn a_directory_is_refused_by_what_it_is_being_asked_for() {
    let dir = scratch("files-directory");
    std::fs::create_dir(dir.join("sub")).expect("a directory");
    std::fs::write(dir.join("sub/inner.txt"), "x\n").expect("a file in it");

    for action in ["read", "write", "edit"] {
        let said = match action {
            "read" => ask(&dir, action, json!({ "path": "sub" })).await,
            "write" => ask(&dir, action, json!({ "path": "sub", "content": "x" })).await,
            _ => {
                ask(
                    &dir,
                    action,
                    json!({ "path": "sub", "old": "x", "new": "y" }),
                )
                .await
            }
        };
        assert!(said.contains("not a regular file"), "{action}: {said}");
    }

    // a read names the tool that answers for a directory
    let read = ask(&dir, "read", json!({ "path": "sub" })).await;
    assert!(
        read.contains("`glob` lists what a directory holds"),
        "{read}"
    );

    // a write does not: the name of the directory is what a caller needs, and there is nothing
    // for `shell` to read
    let write = ask(&dir, "write", json!({ "path": "sub", "content": "x" })).await;
    assert!(write.contains("nothing was written"), "{write}");
    assert!(write.contains("`fs` makes no directories"), "{write}");
    assert!(!write.contains("`shell` can read"), "{write}");
}

/// A path ending in a separator is a directory, and a `write` or an `edit` refuses it rather than
/// making a file of the name without one.
#[tokio::test]
async fn a_path_ending_in_a_separator_is_refused_rather_than_trimmed() {
    let dir = scratch("files-trail");
    std::fs::create_dir(dir.join("sub")).expect("a directory");

    for (action, args) in [
        ("write", json!({ "path": "sub/", "content": "x" })),
        ("edit", json!({ "path": "sub/", "old": "x", "new": "y" })),
    ] {
        let said = ask(&dir, action, args).await;
        assert!(
            said.contains("ends in a separator, so it is a directory"),
            "{action}: {said}"
        );
        assert!(said.contains("`sub`"), "{action} names the file: {said}");
    }

    // a file that does not exist, called with a trailing separator, is refused the same way and
    // not made under the name without it
    let said = ask(&dir, "write", json!({ "path": "trail/", "content": "x" })).await;
    assert!(said.contains("ends in a separator"), "{said}");
    assert!(!dir.join("trail").exists(), "nothing was made: {said}");

    // and one inside a directory, which the resolver would have shortened the same way
    let said = ask(
        &dir,
        "write",
        json!({ "path": "sub/inner/", "content": "x" }),
    )
    .await;
    assert!(said.contains("ends in a separator"), "{said}");
    assert!(
        !dir.join("sub/inner").exists(),
        "nothing was made under the name without it: {said}"
    );

    // a path a `read` is handed is not refused here: it is a path, and what is behind it is
    // refused by name
    let said = ask(&dir, "read", json!({ "path": "sub/" })).await;
    assert!(said.contains("not a regular file"), "{said}");
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

/// A link to a file a rule refuses says the file is refused by its own name too, rather than
/// offering that name to be asked about.
#[tokio::test]
async fn a_link_to_a_file_a_rule_refuses_says_so() {
    use kamchatka::tools::{Careful, Subject};
    use nachalnik::Verdict;

    let dir = scratch("files-link-past-refused");
    std::fs::create_dir(dir.join("secret")).expect("a directory");
    std::fs::write(dir.join("secret/plan.txt"), "the plan\n").expect("a file");
    std::os::unix::fs::symlink("secret", dir.join("s_link")).expect("a link");
    let policy = std::sync::Arc::new(Careful::new());
    policy.set(&Subject::Path("secret/".to_owned()), Verdict::Deny);

    let tools = common::builtin_under(&dir, true, Limits::default(), policy);
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let said = fs
        .invoke(
            &call(
                "c1",
                "fs",
                json!({ "action": "read", "path": "s_link/plan.txt" }),
            ),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned();

    assert!(said.contains("leads to `secret/plan.txt`"), "{said}");
    assert!(said.contains("refused as well"), "{said}");
    assert!(!said.contains("asked about"), "{said}");
    assert!(!said.contains("the plan"), "{said}");
}

/// And under `--no-sandbox`, where the reach is not held but the path rules still are.
///
/// note: named in full, so that what is under test is the link and not which directory a relative
/// path is joined onto.
#[tokio::test]
async fn a_link_past_a_rule_is_refused_without_the_sandbox_too() {
    let dir = scratch("files-link-past-unconfined");
    std::fs::write(dir.join(".env"), "TOKEN=secret\n").expect("a file");
    std::os::unix::fs::symlink(".env", dir.join("alias")).expect("a link");
    let alias = dir.join("alias").display().to_string();

    for (action, args) in [
        ("read", json!({ "path": alias })),
        ("write", json!({ "path": alias, "content": "x" })),
        (
            "edit",
            json!({ "path": alias, "old": "secret", "new": "x" }),
        ),
    ] {
        let said = ask_unconfined(&dir, action, args).await;
        assert!(said.contains("leads to `.env`"), "{action}: {said}");
        assert!(!said.contains("secret"), "{action} read through it: {said}");
    }
    assert_eq!(
        held(&dir, ".env"),
        "TOKEN=secret\n",
        "and nothing was written through it"
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

/// A limit smaller than the room kept for the header still shows lines, rather than calling a
/// short first line longer than the limit and showing none of it.
#[tokio::test]
async fn a_small_limit_still_shows_the_lines_that_fit() {
    let dir = scratch("files-small-limit");
    std::fs::write(dir.join("long.txt"), numbered(100)).expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 200);

    let said = ask_within(&dir, limits, "read", json!({ "path": "long.txt" })).await;
    assert!(
        said.len() <= 200,
        "{} bytes past the limit: {said}",
        said.len()
    );
    let (header, body) = said.split_once('\n').expect("a header, then the lines");
    assert!(header.starts_with("[lines 1-"), "{header}");
    assert!(body.starts_with("line 1\nline 2\n"), "{said}");
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
    let lines = kamchatka::tools::COUNTED as usize / 16 + 1_000;
    let file: String = (1..=lines).map(|n| format!("{n:>15}\n")).collect();
    assert!(file.len() as u64 > kamchatka::tools::COUNTED);
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

/// A file past what a tool keeps is still counted, so a read that is cut says how many lines there
/// are and where the last of them is.
///
/// note: counting was bounded by what a tool keeps, so a 9 MB log said which lines it showed and
/// not how many it had. A live model asked for its last line paged forward 32 KB a request until
/// `--requests` stopped the turn; told the total, it reads the end in one.
#[tokio::test]
async fn a_file_past_what_is_kept_says_how_many_lines_it_has() {
    let dir = scratch("files-counted");
    let lines = kamchatka::tools::KEPT / 16 + 1_000;
    let file: String = (1..=lines).map(|n| format!("{n:>15}\n")).collect();
    assert!(file.len() > kamchatka::tools::KEPT);
    std::fs::write(dir.join("big.log"), &file).expect("a file");

    let start = ask(&dir, "read", json!({ "path": "big.log" })).await;
    let (header, _) = start.split_once('\n').expect("a header");
    assert!(header.contains(&format!(" of {lines}:")), "{header}");

    let end = ask(&dir, "read", json!({ "path": "big.log", "from": lines })).await;
    assert_eq!(
        end,
        format!("[lines {lines}-{lines} of {lines}]\n{lines:>15}\n")
    );
}

/// A file past what `fs` edits at once says how big it is, and not only that it is large.
///
/// note: the size is what the file says about itself, and the read stops one byte past the
/// ceiling because a log is larger by the time it has been read. The figure is the one number the
/// `sed -i` this sends the model to cannot do without, and `larger` is not a number.
#[tokio::test]
async fn a_file_too_big_to_edit_says_how_big_it_is() {
    let dir = scratch("files-too-big");
    let kept = kamchatka::tools::KEPT;
    let big = "x".repeat(kept + 1_000);
    std::fs::write(dir.join("big.log"), &big).expect("a file");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "big.log", "old": "x", "new": "y" }),
    )
    .await;

    assert!(
        said.contains(&format!(
            "{} bytes, more than `fs` edits at once",
            big.len()
        )),
        "it says how big the file is: {said}"
    );
    assert_eq!(held(&dir, "big.log"), big, "and nothing was changed");

    // and the boundary the ceiling draws: a file of exactly it is one `fs` will edit, and only
    // one byte past it is refused. The read stops one byte past, so a file of exactly the ceiling
    // has to be held to for the `>` to be the one that refuses
    let one_past = format!("head\n{}", "x".repeat(kept - 4));
    std::fs::write(dir.join("one-past.log"), &one_past).expect("a file");
    assert_eq!(one_past.len(), kept + 1, "one byte past the ceiling");
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "one-past.log", "old": "head", "new": "lead" }),
    )
    .await;
    assert!(
        said.contains(&format!(
            "{} bytes, more than `fs` edits at once",
            one_past.len()
        )),
        "one byte past the ceiling is refused: {said}"
    );
    assert_eq!(
        held(&dir, "one-past.log"),
        one_past,
        "and nothing was changed"
    );

    let exact = format!("head\n{}", "x".repeat(kept - 5));
    std::fs::write(dir.join("exact.log"), &exact).expect("a file");
    assert_eq!(exact.len(), kept, "and this one is the ceiling exactly");
    let said = ask(
        &dir,
        "edit",
        json!({ "path": "exact.log", "old": "head", "new": "lead" }),
    )
    .await;
    assert!(said.contains("replaced one occurrence"), "{said}");
    assert_eq!(
        held(&dir, "exact.log"),
        exact.replacen("head", "lead", 1),
        "a file of exactly the ceiling is edited"
    );
}

/// A file past what `fs` edits at once that reports no size of its own says `larger`, and not the
/// zero it reported.
///
/// note: `/proc` reports a size of nothing for the files that are larger than anything in memory,
/// and the read stops one byte past the ceiling precisely because a size is what a file says about
/// itself. Where it says nothing, `larger` is the only honest figure; the guard exists to keep
/// the zero out of it, and without it the refusal reads "0 bytes, more than `fs` edits at once".
#[tokio::test]
async fn a_file_too_big_that_reports_no_size_is_only_larger() {
    let dir = scratch("files-too-big-unknown");
    let kallsyms = Path::new("/proc/kallsyms");
    let reads_past = std::fs::metadata(kallsyms).is_ok_and(|meta| meta.len() == 0)
        && std::fs::read(kallsyms)
            .map(|read| read.len() > kamchatka::tools::KEPT)
            .unwrap_or(false);
    if !reads_past {
        eprintln!("skipped: no file here that reports no size and reads past the ceiling");
        return;
    }

    let said = ask_unconfined(
        &dir,
        "edit",
        json!({ "path": kallsyms, "old": "x", "new": "y" }),
    )
    .await;

    assert!(
        said.contains(&format!(
            "larger, more than `fs` edits at once ({} bytes)",
            kamchatka::tools::KEPT
        )),
        "it says only that it is larger: {said}"
    );
    assert!(
        !said.contains("0 bytes, more than"),
        "and not the size it reported: {said}"
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

/// A file whose group is not the writer's is written where it is, because a new file could only
/// carry the writer's own.
///
/// note: the mode is carried across by copying it onto the new file, and the owner and group
/// cannot be - only root could set them on a file it did not create, so a rename over a file
/// belonging to another group silently gives it this process's group. The write would land and
/// read as having worked; what it changed is the file's standing in a directory that is shared
/// with that group for a reason. Skipped where there is no group this process may give a file,
/// which is a single-group machine or root, where the new file would match anyway.
#[tokio::test]
async fn a_file_whose_group_is_not_the_writers_is_written_where_it_is() {
    use std::os::unix::fs::MetadataExt;

    let Some(other) = rustix::process::getgroups()
        .expect("the groups of this process")
        .into_iter()
        .find(|group| *group != rustix::process::getgid())
    else {
        eprintln!("skipped: no group this process may give a file");
        return;
    };

    let dir = scratch("files-grouped");
    let file = dir.join("shared.txt");
    std::fs::write(&file, "old\n").expect("a file");
    rustix::fs::chown(&file, None, Some(other)).expect("a group this process belongs to");
    let before = std::fs::metadata(&file).expect("it is there");
    assert_eq!(before.gid(), other.as_raw(), "it was given that group");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "shared.txt", "content": "new\n" }),
    )
    .await;
    assert!(said.contains("wrote 4 bytes"), "{said}");
    assert_eq!(held(&dir, "shared.txt"), "new\n", "and the write landed");

    let after = std::fs::metadata(&file).expect("it is there");
    assert_eq!(after.gid(), other.as_raw(), "under the group it had");
    assert_eq!(after.ino(), before.ino(), "and as the same file");
}

/// A name the temporary would carry is one it steps over rather than takes, so a file left behind
/// by a run that was killed keeps what it holds.
///
/// note: the temporary is named after the file, the process and a counter, so a run killed between
/// making one and renaming it leaves a file saying whose it was. `O_EXCL` is what makes the next
/// run walk past that name to one of its own rather than open it - and an open that finds a file
/// there without `O_TRUNC` is not a truncation, so the new contents go over the front of the old
/// ones and what the caller reads back has the tail of the previous run left in it.
#[tokio::test]
async fn a_temporary_a_left_over_file_already_holds_is_stepped_over() {
    let dir = scratch("files-left-behind");

    // every name the counter could carry while this suite runs, so that whichever one it is handed
    // is one that is already there
    let pid = std::process::id();
    let decoys: Vec<String> = (0..=256)
        .map(|count| format!(".notes.txt.{pid}.{count}.kamchatka"))
        .collect();
    for decoy in &decoys {
        std::fs::write(dir.join(decoy), "from a run that was killed\n").expect("a left-over");
    }

    let said = ask(
        &dir,
        "write",
        json!({ "path": "notes.txt", "content": "mine\n" }),
    )
    .await;
    assert!(said.contains("wrote 5 bytes"), "{said}");
    assert_eq!(
        held(&dir, "notes.txt"),
        "mine\n",
        "the file holds what was written and nothing of what was there before it"
    );
    let taken: Vec<&String> = decoys
        .iter()
        .filter(|name| !dir.join(name).exists())
        .collect();
    assert!(
        taken.is_empty(),
        "a left-over that says whose it was is not this run's to take: {taken:?}"
    );
}

/// A write that cannot stand a new file in the file's place leaves nothing beside it either.
///
/// note: the other half of the temporary's life, which `Reach::replace` reaches when the rename
/// would change more than the contents - here the owner and group, because the new file is made
/// with the group's the directory carries and only root could give it the old one's. The write
/// lands all the same, written over the file that is there, and the file made for the rename is
/// taken away again; a caller looking at the directory afterwards should find the file it asked
/// for and nothing else.
///
/// note: a setgid directory and a second group, which a machine with one group does not have, so
/// this stands down rather than fails on one.
#[tokio::test]
async fn a_write_that_cannot_rename_over_the_file_leaves_nothing_beside_it() {
    let dir = scratch("files-not-renamed");
    let mine = rustix::process::getgid();
    let Some(other) = rustix::process::getgroups()
        .unwrap()
        .into_iter()
        .find(|group| *group != mine)
    else {
        eprintln!("skipped: no second group for a setgid directory to hand a new file");
        return;
    };
    rustix::fs::chown(&dir, None, Some(other)).expect("the directory is the group's");
    rustix::fs::chmod(&dir, rustix::fs::Mode::from_raw_mode(0o2775)).expect("and setgid");
    let file = dir.join("run.sh");
    std::fs::write(&file, "echo one\n").expect("a file");
    rustix::fs::chown(&file, None, Some(mine)).expect("in the group this process is in");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "run.sh", "content": "echo two\n" }),
    )
    .await;
    assert!(said.contains("wrote 9 bytes"), "{said}");
    assert_eq!(held(&dir, "run.sh"), "echo two\n");

    let entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(entries, ["run.sh"], "nothing is left beside it");
}

/// A file another hard link shares is written where it is, so both names still show one file.
///
/// note: a rename would give this name a new file and leave the other name holding the old
/// contents, which reads as the write not having happened to whoever looks there.
///
/// note: the shorter of the two writes is the half that says the file was emptied first. Being
/// written where it is means an open that truncates, and an open that only wrote would leave the
/// tail of what was there behind the new contents - visible here because the second write is
/// shorter than what it replaced.
#[tokio::test]
async fn a_file_with_another_link_is_written_where_it_is() {
    use std::os::unix::fs::MetadataExt;

    let dir = scratch("files-linked");
    std::fs::write(dir.join("a.txt"), "old and longer\n").expect("a file");
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
        held(&dir, "a.txt"),
        "new\n",
        "and holds what was written, with nothing of what it replaced left after it"
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

/// Edits of one file made at once each land, as a parallel batch makes them.
///
/// note: an edit reads the whole file and renames a changed copy over it, so two at once each read
/// the old contents and the file kept whichever renamed last, with both answering that they had
/// replaced one occurrence. Through one `fs`, as a session holds one, and under two names for the
/// file, since a link is the same file to change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edits_of_one_file_at_once_all_land() {
    let dir = scratch("files-edit-at-once");
    let words: Vec<String> = (0..16).map(|n| format!("word{n}")).collect();
    std::fs::write(dir.join("a.txt"), words.join("\n") + "\n").expect("a file");
    std::os::unix::fs::symlink("a.txt", dir.join("b.txt")).expect("a second name for it");

    let tools = common::builtin(&dir, true, Limits::default());
    let fs = tools
        .into_iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let edits: Vec<_> = words
        .iter()
        .enumerate()
        .map(|(n, word)| {
            let (fs, name) = (fs.clone(), ["a.txt", "b.txt"][n % 2]);
            let args = json!({
                "action": "edit",
                "path": name,
                "old": format!("{word}\n"),
                "new": format!("{}\n", word.to_uppercase()),
            });
            tokio::spawn(async move {
                fs.invoke(&call("c1", "fs", args), OutputSink::disconnected())
                    .await
                    .expect("the tool answers the call either way")
                    .content
                    .to_text()
                    .into_owned()
            })
        })
        .collect();
    for edit in edits {
        let said = edit.await.expect("the edit ran");
        assert!(said.contains("replaced one occurrence"), "{said}");
    }

    let upper: Vec<String> = words.iter().map(|word| word.to_uppercase()).collect();
    assert_eq!(held(&dir, "a.txt"), upper.join("\n") + "\n");
}

/// A write into a directory that is not there names the directory and how to make it.
///
/// note: `fs` makes no directories, and the system's `No such file or directory` is about the
/// file, which reads as though the file were the missing part.
#[tokio::test]
async fn a_write_into_a_missing_directory_names_it() {
    let dir = scratch("files-write-unmade");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "src/util/helpers.rs", "content": "x" }),
    )
    .await;
    let missing = dir.canonicalize().expect("it is there").join("src");
    assert!(
        said.contains(&format!("the directory {} is not there", missing.display())),
        "{said}"
    );
    assert!(said.contains("`mkdir -p "), "{said}");
    assert!(!dir.join("src").exists());
}

/// A write refused for anything other than the directory being missing says so, and is not told
/// to make a directory that is there.
///
/// note: `unmade` is only true where the path really is not there, and the kernel's word for a
/// directory this session cannot search is `Permission denied` - not `No such file or directory`.
/// Asked anyway, the answer claimed a directory that exists was missing and sent the model to
/// `mkdir -p` over a directory it is already standing in.
#[tokio::test]
async fn a_write_refused_for_anything_else_is_not_told_to_make_a_directory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("files-write-refused");
    // a directory there, which its owner has made unsearchable
    let outer = dir.join("outer");
    std::fs::create_dir_all(outer.join("inner")).expect("directories");
    std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o000)).expect("its mode");

    let said = ask(
        &dir,
        "write",
        json!({ "path": "outer/inner/x.txt", "content": "x" }),
    )
    .await;

    // the last two restore the mode whatever the assertions do, so a failure is a directory this
    // suite can remove rather than one it cannot
    std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o755)).expect("its mode");

    assert!(
        outer.join("inner").is_dir(),
        "the directory is there: {said}"
    );
    assert!(
        !said.contains("is not there") && !said.contains("`mkdir"),
        "so it is not said to be missing: {said}"
    );
    assert!(
        said.contains("Permission denied"),
        "the refusal is the kernel's: {said}"
    );
    assert!(
        !outer.join("inner/x.txt").exists(),
        "and nothing was written"
    );
}

/// An edit of a file that is not there says nothing was changed and what makes a new one.
///
/// note: the system's `No such file or directory` was the whole answer, and it names neither.
#[tokio::test]
async fn an_edit_of_a_missing_file_names_write() {
    let dir = scratch("files-edit-missing");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "missing.py", "old": "a", "new": "b" }),
    )
    .await;
    assert!(
        said.contains("is not there, so nothing was changed"),
        "{said}"
    );
    assert!(said.contains("`write` makes a new one"), "{said}");
    assert!(!dir.join("missing.py").exists());
}

/// An edit of a file whose directory is not there either says so, since the `write` it names
/// would be refused for it next.
#[tokio::test]
async fn an_edit_of_a_file_in_a_missing_directory_names_the_directory() {
    let dir = scratch("files-edit-missing-dir");

    let said = ask(
        &dir,
        "edit",
        json!({ "path": "notes/todo.md", "old": "a", "new": "b" }),
    )
    .await;
    assert!(said.contains("`write` makes a new one"), "{said}");
    assert!(
        said.contains(&format!(
            "the directory {} is not there",
            dir.join("notes").display()
        )),
        "{said}"
    );
    assert!(said.contains("`fs` makes no directories"), "{said}");
    assert!(!dir.join("notes").exists());
}

/// An edit whose arguments arrived nested inside `new` is told so, and not that `old` is missing.
///
/// note: the shape a model sent twice in one session, each time told `old` was required and how
/// to add text - true of the call and no help with it.
#[tokio::test]
async fn an_edit_nested_inside_new_is_told_it_is_nested() {
    let dir = scratch("files-edit-nested");
    std::fs::write(dir.join("a.py"), "x = 1\n").expect("the scratch file is written");

    // refused before the tool has a result to give, which the kernel hands the model as an error
    // result like any other
    let tools = common::builtin(&dir, true, Limits::default());
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let nested = call(
        "c1",
        "fs",
        json!({ "action": "edit", "path": "a.py", "new": { "old": "x = 1", "new": "x = 2" } }),
    );
    let said = match fs.invoke(&nested, OutputSink::disconnected()).await {
        Ok(output) => output.content.to_text().into_owned(),
        Err(refused) => refused.to_string(),
    };
    assert!(said.contains("an object holding `new`, `old`"), "{said}");
    assert!(!said.contains("to add text"), "{said}");
    assert_eq!(held(&dir, "a.py"), "x = 1\n");
}

/// A path ending in a separator, where `shell` is refused, is not answered with a `mkdir` to run.
///
/// note: the same stance the missing-directory answer reads, so a model is not sent to a tool the
/// session has just refused it.
#[tokio::test]
async fn a_path_ending_in_a_separator_with_no_shell_offers_no_mkdir() {
    use kamchatka::tools::{Careful, Subject};
    use nachalnik::{Capability, Verdict};

    let dir = scratch("files-trail-refused");
    let policy = std::sync::Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

    let tools = common::builtin_under(&dir, true, Limits::default(), policy);
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let said = fs
        .invoke(
            &call(
                "c1",
                "fs",
                json!({ "action": "write", "path": "trail/", "content": "x" }),
            ),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned();

    assert!(said.contains("ends in a separator"), "{said}");
    assert!(said.contains("`trail`"), "{said}");
    assert!(!said.contains("mkdir"), "{said}");
    assert!(said.contains("is not available in this session"), "{said}");
}

/// And the other half of that answer, where `shell` is refused, reads as one sentence.
///
/// note: this arm was reflowed to the source column and the inter-word spaces came with it, so
/// the model - which reads this one line and has to choose what to do next - was told to `write`
/// followed by 27 spaces and then `it`. `cargo fmt` cannot see inside a string literal, so nothing
/// but a test catches it, and this is the only message in the two tools that carries one.
#[tokio::test]
async fn a_write_into_a_missing_directory_with_no_shell_is_one_sentence() {
    use kamchatka::tools::{Careful, Subject};
    use nachalnik::{Capability, Verdict};

    let dir = scratch("files-write-unmade-refused");
    let policy = std::sync::Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

    let tools = common::builtin_under(&dir, true, Limits::default(), policy);
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let said = fs
        .invoke(
            &call(
                "c1",
                "fs",
                json!({ "action": "write", "path": "src/x.txt", "content": "x" }),
            ),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned();

    assert!(said.contains("is not available in this session"), "{said}");
    assert!(
        said.contains("so write it in a directory that is there"),
        "{said}"
    );
    assert!(
        !said.contains("  "),
        "a message the model reads once carried a run of spaces: {said:?}"
    );
}

/// `fs` sends the model to `shell`, or to an operation of its own, only where it could use it now:
/// not with `shell` turned off, and not where nobody is there to answer the question a call to it
/// would be.
#[tokio::test]
async fn fs_sends_the_model_only_where_it_could_go() {
    use kamchatka::tools::Careful;
    use nachalnik::Verdict;

    let dir = scratch("files-reachable");
    std::fs::write(dir.join("latin.txt"), b"caf\xe9\n").expect("written");
    let policy = std::sync::Arc::new(Careful::new());
    let tools = common::builtin_under(&dir, true, Limits::default(), policy.clone());
    for tool in &tools {
        policy.offers(tool.spec());
    }
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` should be one of the built-in tools");
    let ask = |args: serde_json::Value| async {
        fs.invoke(&call("c1", "fs", args), OutputSink::disconnected())
            .await
            .expect("the tool answers the call either way")
            .content
            .to_text()
            .into_owned()
    };
    let edit = json!({ "action": "edit", "path": "notes/todo.md", "old": "a", "new": "b" });
    let text = json!({ "action": "read", "path": "latin.txt" });

    let said = ask(edit.clone()).await;
    assert!(said.contains("mkdir"), "with `shell` on offer: {said}");
    let said = ask(text.clone()).await;
    assert!(said.contains("`shell` can read it as bytes"), "{said}");

    policy.withdraw("shell", true);
    let said = ask(edit.clone()).await;
    assert!(
        !said.contains("mkdir") && !said.contains("Make it"),
        "{said}"
    );
    let said = ask(text.clone()).await;
    assert!(!said.contains("`shell`"), "{said}");
    assert!(said.contains("`grep` searches a file"), "{said}");
    policy.withdraw("shell", false);

    // a headless run that answers nobody's questions refuses every call nothing allowed
    policy.unanswered(Some(Verdict::Deny));
    let said = ask(edit).await;
    assert!(!said.contains("`write`"), "{said}");
    let said = ask(text).await;
    assert!(
        !said.contains("`shell`") && !said.contains("`grep`"),
        "{said}"
    );
}
