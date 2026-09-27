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

/// A limit under the size of a header still shows what fits, rather than the marker and nothing.
///
/// note: 256 bytes were kept for the line naming which lines these are, out of a limit a person
/// sets with `/limit fs:read` - so a limit of a few hundred bytes left no room for any of the
/// file, and every read answered with the line saying the limit stops it there and nothing under
/// it. The room is half the limit below that, so the lines and the line naming them each have as
/// much as the other and neither is cut at nothing.
#[tokio::test]
async fn a_limit_smaller_than_the_header_still_shows_what_fits() {
    let dir = scratch("files-small-limit");
    let file: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    std::fs::write(dir.join("small.txt"), &file).expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 250);

    let said = ask_within(&dir, limits.clone(), "read", json!({ "path": "small.txt" })).await;

    assert!(said.len() <= 250, "{} bytes past the limit", said.len());
    assert!(
        said.contains("line 1\n") && said.contains("read on with `from: "),
        "what fits is shown, and where reading on starts: {said}"
    );
    let (header, body) = said.split_once('\n').expect("a header, then the lines");
    assert!(header.starts_with("[lines 1-"), "{header}");
    assert!(
        body.lines().all(|line| line.starts_with("line ")),
        "the cut is at a whole line: {said}"
    );

    // and one that fits is still the file, with nothing added
    std::fs::write(dir.join("tiny.txt"), "just this\n").expect("a file");
    let said = ask_within(&dir, limits, "read", json!({ "path": "tiny.txt" })).await;
    assert_eq!(said, "just this\n");

    // the case this was reported for: a limit at the header's own size, and a file of a few
    // dozen bytes, which was answered with the line saying the limit stops it there and nothing
    let file = "a file small enough to be worth reading whole\n";
    assert!(file.len() < 256, "{} bytes is not the case", file.len());
    let whole = scratch("files-small-limit-whole");
    std::fs::write(whole.join("small.txt"), file).expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 256);

    let said = ask_within(&whole, limits, "read", json!({ "path": "small.txt" })).await;
    assert_eq!(said, file, "the whole of a file that fits is the file");
}

/// A limit too small to hold the line naming the limit answers with a whole sentence saying so,
/// rather than a fragment of that line and none of the file - and the sentence is itself no
/// longer than the limit it is answering for.
///
/// note: the header is the answer's first line and the output limit cuts from the end, so a
/// `/limit fs:read` below what the header takes leaves the model the first few words of it. It
/// reads that as the file's own first line - `[line` is a plausible way for a file to begin, and
/// the model that is shown it has no way to tell it from a cut. Nothing in it says a limit did
/// it, and the only move left to the model is another tool, which is often a permission this
/// session was never given.
///
/// note: the sentence naming the limit is the one answer the kernel is about to cut as well, and
/// it was longer than every limit below 124 bytes - so the model was shown the first few words
/// of the very sentence meant to tell it a limit had stopped the read, and a refusal is the one
/// answer with nowhere to page on from. Each limit now gets the longest whole sentence it holds.
#[tokio::test]
async fn a_limit_smaller_than_the_header_says_so_whole_and_within_it() {
    let dir = scratch("files-limit-under-header");
    // long enough that no limit here holds the whole of it, so every answer has a header
    std::fs::write(dir.join("lines.txt"), numbered(200)).expect("a file");

    for bytes in MIN..=200usize {
        let limits = Limits::default();
        limits.set("fs:read", bytes);

        let said = ask_within(&dir, limits, "read", json!({ "path": "lines.txt" })).await;
        // what the kernel leaves of it, which is what the model reads
        let mut shown = nachalnik::Content::text(said);
        assert!(
            shown.truncate_to(bytes).is_none(),
            "{bytes} bytes: the answer was cut by the kernel: {:?}",
            shown.to_text()
        );
        let shown = shown.to_text();

        // either a whole sentence about the limit, or lines under a whole header naming them -
        // and never one without the other
        match shown.split_once('\n') {
            Some((header, body)) => {
                assert!(
                    header.starts_with('[') && header.ends_with(']'),
                    "{bytes}: {header:?}"
                );
                assert!(
                    !body.is_empty(),
                    "{bytes} bytes: no content under the header"
                );
                assert!(header.contains("read on with `from: "), "{bytes}: {header}");
            }
            None => {
                assert!(
                    shown.starts_with('[') && shown.ends_with(']'),
                    "{bytes}: {shown:?}"
                );
                assert!(
                    shown.contains(&format!("{bytes}")) && shown.contains("line"),
                    "{bytes} bytes does not name the limit that did it: {shown}"
                );
            }
        }
    }

    // and where the long sentence fits, it is the one that is given - a limit below the header
    // for 200 lines, and above what the sentence naming the limit takes
    let said = {
        let limits = Limits::default();
        limits.set("fs:read", 124);
        ask_within(&dir, limits, "read", json!({ "path": "lines.txt" })).await
    };
    assert!(
        said.contains("too little to show a line of this file"),
        "124 bytes: {said}"
    );
    assert!(
        said.contains("raise it with `/limit fs:read`"),
        "124 bytes says how to change it: {said}"
    );

    // and a limit the header does fit under is the ordinary answer again: a whole file with
    // nothing added, and a file too long for the limit stopped at a whole line
    let small = scratch("files-limit-under-header-fits");
    std::fs::write(small.join("one-line.txt"), "just this\n").expect("a file");
    let limits = Limits::default();
    limits.set("fs:read", 300);
    let said = ask_within(
        &small,
        limits.clone(),
        "read",
        json!({ "path": "one-line.txt" }),
    )
    .await;
    assert_eq!(said, "just this\n", "nothing is added to a file that fits");

    std::fs::write(small.join("long.txt"), numbered(200)).expect("a file");
    let said = ask_within(&small, limits, "read", json!({ "path": "long.txt" })).await;
    let (header, body) = said.split_once('\n').expect("a header, then the lines");
    assert!(header.starts_with("[lines 1-"), "{header}");
    assert!(header.contains("read on with `from: "), "{header}");
    assert!(
        body.lines().all(|line| line.starts_with("line ")),
        "the cut is at a whole line: {said}"
    );
}

/// The smallest `/limit fs:read` there is a whole answer for. Below it the model is shown
/// nothing at all, which is the one case where a limit has no answer under it rather than a
/// short one.
///
/// note: the shortest of the three sentences, and a limit under it holds no sentence and no part
/// of one. `/limit` refuses zero for the same reason it is not a limit anybody means, and a
/// person who sets one this small is asking what the tool does with almost nothing - which is
/// that it says so in as many words as fit, and where even one bracketed sentence does not fit,
/// says nothing at all rather than half a word.
const MIN: usize = 19;

/// Every limit, every kind of file: what the model is shown is a whole answer the limit paid
/// for, or the file's own lines, and never a fragment of either.
///
/// note: what the tool returns and what the model is shown are not the same string, and that is
/// how this was missed. `fs:read` is one number doing two jobs - what `read` shapes its answer to
/// and what the kernel cuts that answer to, `Fs::limit` handing back the same row for both - so
/// the tool's own answer was cut a second time, from the end, by the limit it was written to
/// answer for. A header is the one line a model reads as the file's own first line, and a
/// sentence about the limit is the one answer with nowhere to page on from, so the two worth
/// protecting are the two that say something about the read rather than showing the file.
///
/// note: the cut is `Content::truncate_to`, which is what `record_output` calls on the way in,
/// rather than a second copy of the rule: a test that reimplemented it would pass against a limit
/// the kernel does not cut at, and would miss one it does.
#[tokio::test]
async fn every_limit_answers_whole_and_shows_what_fits() {
    let dir = scratch("files-every-limit");
    // a file with nothing in it, one short line, one line wider than any limit here, and one of
    // many lines - the four shapes the answer is built differently for
    let files = [
        ("empty.txt", "".to_owned()),
        ("one.txt", "just this\n".to_owned()),
        ("wide.txt", format!("{}\nafter\n", "w".repeat(1_000))),
        ("many.txt", numbered(200)),
    ];
    for (name, content) in &files {
        std::fs::write(dir.join(name), content).expect("a file");
    }

    for bytes in 1..=400usize {
        let limits = Limits::default();
        limits.set("fs:read", bytes);

        for (name, content) in &files {
            let said = ask_within(&dir, limits.clone(), "read", json!({ "path": name })).await;
            // what the kernel leaves of it, which is what the model reads
            let mut shown = nachalnik::Content::text(said);
            let cut = shown.truncate_to(bytes);
            let label = format!("{name} at {bytes} bytes");
            let shown = shown.to_text();

            assert!(cut.is_none(), "{label} was cut by the kernel: {shown:?}");
            // whatever comes back under any limit is a whole answer: a line in brackets and
            // the file's lines under it, or one bracketed sentence saying the limit is what
            // stopped it. Never one without the other, and never a header cut at a byte.
            match shown.split_once('\n') {
                // the file itself, which is an answer too and carries no line naming it
                _ if content.starts_with(shown.as_ref()) => {}
                Some((header, body)) => {
                    assert!(header.starts_with('['), "{label} does not start: {shown:?}");
                    assert!(header.ends_with(']'), "{label} cuts the header: {header:?}");
                    assert!(shown.len() <= bytes, "{label} is {} bytes", shown.len());
                    assert!(!body.is_empty(), "{label} has no content: {shown:?}");
                    assert!(
                        body.ends_with('\n') || header.contains("is longer than"),
                        "{label} stops inside a line: {shown:?}"
                    );
                    // and where it names lines, it names the lines it is showing
                    if let Some(first) = header
                        .strip_prefix("[lines ")
                        .and_then(|rest| rest.split('-').next())
                        .and_then(|first| first.parse::<usize>().ok())
                    {
                        assert_eq!(
                            first + body.lines().count() - 1,
                            span_through(header),
                            "{label} does not name the lines it shows: {shown:?}"
                        );
                    }
                }
                // no content under a line, so the whole answer is either one bracketed
                // sentence, or nothing at all where the limit holds neither
                None => assert!(
                    shown.starts_with('[') && shown.ends_with(']')
                        || (shown.is_empty() && bytes < MIN),
                    "{label} is a whole sentence: {shown:?}"
                ),
            }
        }
    }
}

/// The last line a header says it is showing: the second number of the span it names.
fn span_through(header: &str) -> usize {
    let span = header
        .strip_prefix("[lines ")
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit() && c != '-').next())
        .expect("a header naming lines names the span it shows");
    span.split('-')
        .nth(1)
        .expect("a span has a last line")
        .parse()
        .expect("a line number")
}

/// The band of limits where the header fitted on its own and the answer did not.
///
/// note: the guard was on the header alone, and `room` was half the limit below twice the
/// header, so for a band of limits the header fitted, the guard declined to act, and the kernel
/// cut the answer at a byte from the end - inside the header, which is the one place `read` was
/// written never to be cut. A model shown `[lines 1-6 of 200: the output limit (90 by` reads it
/// as the file's own first line; the cut marker that would have said otherwise was cut too.
#[tokio::test]
async fn a_limit_where_only_the_header_fitted_still_leaves_a_whole_answer() {
    let dir = scratch("files-limit-band");
    std::fs::write(dir.join("long.txt"), numbered(200)).expect("a file");

    // the band the finding names, and both sides of it
    for bytes in [60, 80, 88, 90, 95, 100, 104, 110, 122, 150] {
        let limits = Limits::default();
        limits.set("fs:read", bytes);

        let said = ask_within(&dir, limits, "read", json!({ "path": "long.txt" })).await;
        let mut shown = nachalnik::Content::text(said);
        assert!(
            shown.truncate_to(bytes).is_none(),
            "{bytes} bytes: cut by the kernel: {:?}",
            shown.to_text()
        );
        let shown = shown.to_text();

        match shown.split_once('\n') {
            Some((header, body)) => {
                assert!(
                    header.ends_with(']'),
                    "{bytes} bytes cuts the header: {header:?}"
                );
                assert!(
                    !body.is_empty(),
                    "{bytes} bytes: no content under the header"
                );
                assert!(
                    header.contains("read on with `from: "),
                    "{bytes} bytes does not say where reading on starts: {header}"
                );
                assert!(
                    body.ends_with('\n') && body.lines().all(|l| l.starts_with("line ")),
                    "{bytes} bytes stops inside a line: {shown:?}"
                );
            }
            None => assert!(
                shown.starts_with('[') && shown.ends_with(']'),
                "{bytes} bytes is not a whole answer: {shown:?}"
            ),
        }
    }
}

/// What fits under a limit is shown, and the whole of the answer is inside it: the lines that
/// stop it are whole lines of the file, and the header naming them fits alongside them.
#[tokio::test]
async fn what_fits_under_a_limit_is_shown_and_the_answer_is_inside_it() {
    let dir = scratch("files-limit-fits");
    let file = numbered(200);
    std::fs::write(dir.join("many.txt"), &file).expect("a file");

    // a limit with room for the file shows all of it, with nothing added - a limit is what is
    // shown of a call, and the room a header would take is only spent where there is a header
    let limits = Limits::default();
    limits.set("fs:read", 4_000);
    let said = ask_within(&dir, limits, "read", json!({ "path": "many.txt" })).await;
    assert_eq!(said, file, "a file that fits is the file");

    // and a limit it does not fit under shows the lines that do, stopped at a whole one, with a
    // header that names where reading on starts and fits under the limit along with them
    for bytes in [200, 260, 300, 400, 1_000] {
        let limits = Limits::default();
        limits.set("fs:read", bytes);
        let said = ask_within(&dir, limits, "read", json!({ "path": "many.txt" })).await;
        assert!(said.len() <= bytes, "{bytes} bytes: {} shown", said.len());
        let (header, body) = said.split_once('\n').expect("a header, then the lines");
        assert!(header.starts_with("[lines 1-"), "{bytes} bytes: {header}");
        assert!(
            header.ends_with(']'),
            "{bytes} bytes cuts the header: {header}"
        );
        assert!(
            header.contains("read on with `from: "),
            "{bytes} bytes says where reading on starts: {header}"
        );
        assert!(
            body.ends_with('\n') && body.lines().all(|line| line.starts_with("line ")),
            "{bytes} bytes cuts inside a line: {said}"
        );
        assert!(
            file.starts_with(body),
            "{bytes} bytes shows lines that are not the file's: {body}"
        );
    }
}

/// A line too long for the limit is shown from its start and said to be one, at every limit -
/// which is the case with no whole line to fall back on, and so the only one where the answer
/// is cut inside a line on purpose.
#[tokio::test]
async fn a_wide_line_is_answered_for_at_every_limit() {
    let dir = scratch("files-limit-wide");
    let file = format!("{}\nafter\n", "w".repeat(300));
    std::fs::write(dir.join("wide.txt"), &file).expect("a file");

    for bytes in MIN..=400usize {
        let limits = Limits::default();
        limits.set("fs:read", bytes);
        let said = ask_within(&dir, limits, "read", json!({ "path": "wide.txt" })).await;
        assert!(said.len() <= bytes, "{bytes} bytes: {} shown", said.len());
        match said.split_once('\n') {
            // room for the header that names it, and the start of the line under it
            Some((header, body)) => {
                assert!(
                    header.ends_with(']'),
                    "{bytes} bytes cuts the header: {header}"
                );
                assert!(
                    header.contains("is longer than the output limit"),
                    "{bytes}: {header}"
                );
                assert!(body.chars().all(|c| c == 'w'), "{bytes} bytes: {body}");
            }
            // too little for a header and a start: the sentence about the limit, whole
            None => {
                assert!(
                    said.starts_with('[') && said.ends_with(']'),
                    "{bytes} bytes: {said}"
                );
                assert!(said.contains(&format!("{bytes}")), "{bytes} bytes: {said}");
            }
        }
    }
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

    assert!(said.contains("is refused in this session"), "{said}");
    assert!(
        said.contains("so write it in a directory that is there"),
        "{said}"
    );
    assert!(
        !said.contains("  "),
        "a message the model reads once carried a run of spaces: {said:?}"
    );
}
