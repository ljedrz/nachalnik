//! Sessions written out and read back in.
//!
//! note: a resumed session has to be the one that was saved, which is more than the messages: the
//! tool call identifiers it already used, the figures on the scale they were counted on, and a
//! conversation that reads as it did. These write real files under a scratch directory and load
//! them again.

use std::{path::Path, sync::Arc};

use crossterm::event::KeyCode;
use kamchatka::{
    app::{App, Tab},
    tools::Subject,
};
use nachalnik::{
    Capability, Config, ContextItem, ContextState, Kernel, ModelResponse, Usage, Verdict,
    test::{ConstTool, ScriptedProvider, call},
};
use serde_json::json;

use crate::{common, harness::Harness};

/// A session is named for when it started, in a name that is also its two files.
///
/// note: it was `kamchatka-1788849917`, written into a directory called `kamchatka` - so half of
/// every filename repeated the directory and the other half said nothing to anybody reading a list
/// of them. The dates here are the ones a calendar gets wrong: a leap day, the day after one, the
/// first of March in a century that is not a leap year, and the epoch itself.
#[test]
fn a_session_is_named_for_when_it_started() {
    for (secs, expected) in [
        (0, "1970-01-01T00-00-00Z"),
        (1_788_849_917, "2026-09-08T06-45-17Z"),
        (951_782_400, "2000-02-29T00-00-00Z"),
        (951_868_800, "2000-03-01T00-00-00Z"),
        (4_107_542_400, "2100-03-01T00-00-00Z"),
        (1_583_020_800, "2020-03-01T00-00-00Z"),
    ] {
        assert_eq!(App::session_stamp(secs), expected, "at {secs}");
    }

    // sortable, which is most of what a directory of them is for
    let mut names = [
        App::session_stamp(1_788_849_917),
        App::session_stamp(0),
        App::session_stamp(951_782_400),
    ];
    names.sort();
    assert_eq!(names[0], "1970-01-01T00-00-00Z");
    assert_eq!(names[2], "2026-09-08T06-45-17Z");

    // and it says nothing about kamchatka, because the directory it goes in already does
    assert!(!App::session_stamp(0).contains("kamchatka"));
}

#[tokio::test]
async fn a_session_is_saved_to_a_path_and_comes_back_from_it() {
    let dir = common::scratch("save");
    // named `.jsonl` on purpose: the stem used to keep it, so this wrote `notes.jsonl.jsonl`
    let asked = dir.join("notes.jsonl");

    let mut harness = Harness::new([ModelResponse::text("4817, noted")]);
    harness.send("remember 4817").await;
    harness.settle().await;

    harness.send(&format!("/save {}", asked.display())).await;

    let log = dir.join("notes.jsonl");
    let state = dir.join("notes.json");
    assert!(log.exists(), "the log is at the path that was asked for");
    assert!(state.exists(), "and so is the snapshot");
    assert!(
        !dir.join("notes.jsonl.jsonl").exists(),
        "the extension should not have been doubled"
    );
    // packed, because the note carries the path it wrote to and a long enough temp directory
    // leaves the renderer no choice but to break the name across two rows
    assert!(
        harness.packed().contains("notes.json"),
        "{}",
        harness.screen()
    );

    // the log is one record per line, and every line is a record
    let written = std::fs::read_to_string(&log).unwrap();
    let records: Vec<&str> = written.lines().filter(|line| !line.is_empty()).collect();
    assert!(!records.is_empty());
    for line in &records {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }

    // and the snapshot rebuilds the context in a kernel that never saw any of it happen
    let snapshot: nachalnik::Snapshot =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).expect("a session");
    let carried = Kernel::resume(Config::default(), snapshot);

    let said: Vec<String> = carried
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(said.iter().any(|text| text.contains("remember 4817")));
    assert!(said.iter().any(|text| text.contains("4817, noted")));

    // saving again over the same files says so rather than replacing them in silence
    harness.send(&format!("/save {}", asked.display())).await;
    assert!(harness.flat().contains("replaced"), "{}", harness.screen());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The two files of a save describe one moment, and a save that cannot finish leaves the one
/// before it as it was.
///
/// note: `/save good` and `/load good` are the checkpoint `RUNNING.md` describes, and saving again
/// truncated both files before writing either - so a second save that ran out of room destroyed
/// the checkpoint it was replacing. A directory nothing may be written in is the failure a test can
/// arrange; the files in it can still be renamed over, which is why the temporaries are the part
/// that has to come first.
#[cfg(unix)]
#[tokio::test]
async fn a_save_that_cannot_finish_leaves_the_last_one_alone() {
    use std::os::unix::fs::PermissionsExt;

    let dir = common::scratch("save-kept");
    let stem = dir.join("good");
    let mut harness = Harness::new([ModelResponse::text("noted")]);
    harness.send("remember 4817").await;
    harness.settle().await;
    harness.send(&format!("/save {}", stem.display())).await;

    let (log, state) = (dir.join("good.jsonl"), dir.join("good.json"));
    let (logged, saved) = (std::fs::read(&log).unwrap(), std::fs::read(&state).unwrap());
    let last = String::from_utf8_lossy(&logged)
        .lines()
        .rfind(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<nachalnik::Record>(line).unwrap().seq);
    let snapshot: nachalnik::Snapshot = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        last,
        Some(snapshot.last_seq),
        "the log ends where the snapshot was taken"
    );

    harness.send("and 9001 as well").await;
    harness.settle().await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // somebody permissions do not bind has nothing to show here
    let binding = std::fs::File::create(dir.join("probe")).is_err();
    if binding {
        harness.send(&format!("/save {}", stem.display())).await;
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !binding {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    assert!(
        harness.flat().contains("could not write"),
        "{}",
        harness.screen()
    );
    assert_eq!(
        std::fs::read(&log).unwrap(),
        logged,
        "the log is the one saved before"
    );
    assert_eq!(
        std::fs::read(&state).unwrap(),
        saved,
        "and so is the snapshot"
    );
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "nothing is left beside them: {names:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `/load` takes every spelling `/save` does, which is the only way the pair round-trips.
///
/// note: a session is two files, so `/save notes.jsonl` writes `notes.json` beside the log it was
/// named after - and `/load notes.jsonl` took its argument at its word and went looking for
/// `notes.jsonl.json`, which nothing had ever written.
///
/// note: the *suffix* is the part case does not matter to, and the stem is not - `NOTES.JSON`
/// names a different file from `notes.json` on any filesystem that cares, and this one is on
/// Linux. What the suffix buys is the person who typed the extension in caps on a filesystem that
/// does not care, which is every one outside Linux.
///
/// note: driven through `/load`'s own answer rather than by calling the helper, because what the
/// pair has to agree about is the *file*, and only the command knows which one it opened. The
/// error names the path it tried, which is what makes a wrong one visible here at all.
#[tokio::test]
async fn load_takes_every_spelling_save_does() {
    let dir = common::scratch("naming");
    let stem = dir.join("notes");

    let mut harness = Harness::new([ModelResponse::text("4817, noted")]);
    harness.send("remember 4817").await;
    harness.settle().await;
    harness.send(&format!("/save {}", stem.display())).await;
    assert!(dir.join("notes.json").exists() && dir.join("notes.jsonl").exists());

    for spelling in [
        "notes",
        "notes.json",
        "notes.jsonl",
        "notes.JSON",
        "notes.JSONL",
    ] {
        let mut second = Harness::new([]);
        second
            .send(&format!("/load {}", dir.join(spelling).display()))
            .await;

        let said = second.flat();
        assert!(
            !said.contains("could not read"),
            "`/load {spelling}` named a file nothing wrote: {said}"
        );
        assert!(
            said.contains("4817"),
            "`/load {spelling}` did not bring the session back: {said}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory is a place to put a session, not a name to give it.
///
/// note: found by driving a headless run. `/save sessions/` took the whole argument as the stem
/// and wrote `sessions/.json` and `sessions/.jsonl` - two dotfiles, which `ls` does not show -
/// under a confirmation that prints the paths and so reads as though it had worked. The name a
/// session goes under in a directory is the one it already has, which is what this program uses
/// when it writes a session out on its own at the end of a run.
#[tokio::test]
async fn saving_into_a_directory_names_the_session_rather_than_writing_a_dotfile() {
    let dir = common::scratch("save-dir");

    let mut harness = Harness::new([ModelResponse::text("noted")]);
    harness.send("remember 4817").await;
    harness.settle().await;

    let stamp = harness.app.kernel.session_name();

    // with the separator, which is how somebody spells "in here"
    harness.send(&format!("/save {}/", dir.display())).await;
    assert!(
        dir.join(format!("{stamp}.json")).exists(),
        "the session goes in under its own name: {:?}",
        std::fs::read_dir(&dir).unwrap().flatten().count()
    );
    assert!(dir.join(format!("{stamp}.jsonl")).exists());
    assert!(
        !dir.join(".json").exists() && !dir.join(".jsonl").exists(),
        "and never as a dotfile, which is a file nobody is going to find"
    );

    // and without it, where the argument is an existing directory all the same
    let _ = std::fs::remove_file(dir.join(format!("{stamp}.json")));
    let _ = std::fs::remove_file(dir.join(format!("{stamp}.jsonl")));
    harness.send(&format!("/save {}", dir.display())).await;
    assert!(
        dir.join(format!("{stamp}.json")).exists(),
        "an existing directory is a directory whether or not it was spelled with a slash"
    );
    // and the sibling case still holds: a path that is not a directory is the name asked for
    assert!(
        !dir.with_extension("json").exists(),
        "the directory itself should not have gained an extension"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_saved_session_comes_back_into_a_running_one_without_losing_what_was_there() {
    let dir = common::scratch("load");
    let saved = dir.join("before.json");

    let mut first = Harness::new([ModelResponse::text("4817, noted")]);
    first.send("remember 4817").await;
    first.settle().await;
    first
        .send(&format!("/save {}", dir.join("before").display()))
        .await;
    assert!(saved.exists());

    // a second session, with a conversation of its own already in it, and something pinned
    let mut second = Harness::new([ModelResponse::text("nothing so far")]);
    let pinned = second
        .app
        .kernel
        .push(ContextItem::system("answer in Polish").pinned());
    second.send("what do you remember").await;
    second.settle().await;
    let mine: Vec<_> = second.app.kernel.items().iter().map(|i| i.id).collect();

    second.send(&format!("/load {}", saved.display())).await;

    // what was here is set aside rather than dropped: same numbers, same contents, not going
    for id in mine.iter().filter(|id| **id != pinned) {
        let item = second.app.kernel.item(*id).expect("still there");
        assert_eq!(item.state, ContextState::Archived, "[{id}] was dropped");
        assert!(!item.content.to_text().is_empty());
    }

    // a pin is the person saying this stays, and `--system` is pinned: a load that archived it
    // would answer a question about a saved conversation by revoking the session's instructions
    let kept = second.app.kernel.item(pinned).expect("still there");
    assert_eq!(kept.state, ContextState::Pinned, "the pin was overruled");

    // and the loaded conversation is the one the next request would carry
    let projected: Vec<String> = second
        .app
        .kernel
        .project()
        .messages
        .iter()
        .map(|message| {
            message
                .content
                .as_ref()
                .map(|c| c.to_text().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        projected.iter().any(|text| text.contains("remember 4817")),
        "{projected:?}"
    );
    assert!(
        !projected
            .iter()
            .any(|text| text.contains("what do you remember")),
        "{projected:?}"
    );
    assert!(
        projected
            .iter()
            .any(|text| text.contains("answer in Polish")),
        "{projected:?}"
    );

    // it is on the screen as the conversation it was, and it says what it did
    let screen = second.flat();
    assert!(screen.contains("remember 4817"), "{screen}");
    assert!(screen.contains("loaded"), "{screen}");

    // and nothing was destroyed to get here: two undos and it is as it was
    assert!(second.app.kernel.undo());
    assert!(second.app.kernel.undo());
    for id in mine.iter().filter(|id| **id != pinned) {
        let item = second.app.kernel.item(*id).expect("still there");
        assert_eq!(item.state, ContextState::Active, "[{id}] did not come back");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A loaded conversation brings tool call identifiers this kernel never issued, and the kernel has
/// to be told: `-r` gets them from `Kernel::resume`, and until this there was nothing that said
/// the same thing to a session already running. A provider that numbers its calls from zero every
/// turn - and they exist, which is why the repair exists - would hand one straight back, and the
/// next request would carry the same `tool_call_id` twice.
#[tokio::test]
async fn a_loaded_session_hands_over_the_identifiers_it_already_used() {
    let dir = common::scratch("load-calls");
    let saved = dir.join("worked.json");

    // a session with a real tool exchange in it, which is the only way an identifier gets used
    let first = Kernel::new(Config::default());
    first.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("call_0", "peek", json!({}))]),
        ModelResponse::text("done"),
    ])));
    first.set_policy(Arc::new(nachalnik::test::AllowAll));
    first.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    first.push(ContextItem::user("look"));
    first.turn().await.expect("the turn ran");
    std::fs::write(
        &saved,
        serde_json::to_vec(&first.snapshot()).expect("a session serializes"),
    )
    .expect("written");

    let mut second = Harness::new([
        ModelResponse::tool_calls(vec![call("call_0", "peek", json!({}))]),
        ModelResponse::text("done"),
    ]);
    second
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("peek", "ok")));
    second
        .app
        .policy
        .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    second.send(&format!("/load {}", saved.display())).await;

    // the loaded turn's identifier is now this kernel's own, and saying so is on the trace
    assert!(
        second
            .app
            .kernel
            .snapshot()
            .used_calls
            .iter()
            .any(|used| used.0 == "call_0"),
        "the loaded identifiers were dropped on the floor"
    );
    second.drain();
    second.tab(Tab::Trace);
    assert!(second.flat().contains("tool.reserved"), "{}", second.flat());

    // so when the provider offers it again, the kernel repairs it rather than letting the request
    // answer one call twice
    second.tab(Tab::Chat);
    second.send("and again").await;
    second.settle().await;

    let sent: Vec<String> = second
        .app
        .kernel
        .preview_request()
        .expect("a request")
        .messages
        .iter()
        .flat_map(|message| message.calls().map(|c| c.id.0.clone()).collect::<Vec<_>>())
        .collect();
    let mut unique = sent.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(sent.len(), unique.len(), "{sent:?}");
    assert!(sent.len() > 1, "both calls should be in it: {sent:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every figure on the screen has to be on one scale. A snapshot carries what its counter had
/// learnt, and reading it in moves the correction under everything already counted - so the load
/// has to count the items it brings *under* that correction and bring the rest onto it, or the
/// `held` column and the `sending` column beside it are answering in different units.
#[tokio::test]
async fn a_loaded_session_puts_every_figure_on_one_scale() {
    let dir = common::scratch("load-scale");
    let saved = dir.join("taught.json");

    // a session whose provider charged twice what was estimated, so its counter learnt a scale
    let first = Kernel::new(Config::default());
    first.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(4_000),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }])));
    first.push(ContextItem::file("big.rs", "x".repeat(8_000)));
    first.push(ContextItem::user("go"));
    first.turn().await.expect("the turn ran");

    let snapshot = first.snapshot();
    let learned = snapshot.calibration.expect("it learnt something");
    assert!(
        learned.scale > 1.5,
        "the fixture must move the scale: {learned:?}"
    );
    std::fs::write(
        &saved,
        serde_json::to_vec(&snapshot).expect("it serializes"),
    )
    .expect("written");

    // read into a session that has learnt nothing of its own
    let mut second = Harness::new([]);
    second
        .app
        .kernel
        .push(ContextItem::file("mine.rs", "y".repeat(400)));
    second.send(&format!("/load {}", saved.display())).await;

    // the figures on the screen are what a recount would make them, which is the definition of
    // their being on the current scale
    let shown = second
        .app
        .kernel
        .with_context(|c| (c.tokens(), c.tokens_withheld()));
    second.app.kernel.recount();
    let settled = second
        .app
        .kernel
        .with_context(|c| (c.tokens(), c.tokens_withheld()));
    assert_eq!(
        shown, settled,
        "a recount would move what the pane is showing"
    );

    // including the items that were here before the load and are now set aside: they were counted
    // on the old scale, and `held back` adds them to the loaded ones
    let mine = second
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| item.label == "mine.rs")
        .expect("still there");
    assert_eq!(mine.state, ContextState::Archived);
    assert!(
        mine.tokens as f64 > 100.0 * learned.scale * 0.9,
        "the archived item is still on the old scale: {} tokens",
        mine.tokens
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn loading_something_that_is_not_a_session_says_which_file_and_why() {
    let dir = common::scratch("not-a-session");
    let junk = dir.join("junk.json");
    std::fs::write(&junk, "{\"nope\": true}").expect("written");

    let mut harness = Harness::new([]);
    harness.send(&format!("/load {}", junk.display())).await;
    // flattened, because the message carries a path and a long enough one wraps the sentence
    let screen = harness.flat();
    assert!(screen.contains("is not a session"), "{screen}");
    // the name, from the view that survives a path the renderer had to break mid-token
    assert!(harness.packed().contains("junk.json"), "{screen}");

    harness
        .send(&format!("/load {}", dir.join("absent.json").display()))
        .await;
    let screen = harness.flat();
    assert!(screen.contains("could not read"), "{screen}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_resumed_session_is_read_back_as_the_conversation_it_was() {
    let mut first = Harness::new([ModelResponse::text("of course")]);
    first.send("hello there").await;
    first.settle().await;

    // what `-r` does: a fresh kernel from the snapshot, and the terminal reads the conversation
    // back off the context, because a resume arrives as one event rather than a thousand
    let carried = Kernel::resume(Config::default(), first.app.kernel.snapshot());
    let mut second = Harness::new([]);
    second.app.kernel = carried;
    second.app.replay();

    let screen = second.screen();
    assert!(screen.contains("hello there"), "{screen}");
    assert!(screen.contains("of course"), "{screen}");
    assert!(screen.contains("resumed session"), "{screen}");
}

#[tokio::test]
async fn a_session_can_be_written_without_anybody_having_asked() {
    // the failure this closes: the record existed only if somebody typed `/save`, which is the
    // wrong condition - a session that ended badly is the one worth reading, and it was the one
    // that left nothing behind. Nine runs against a provider that timed out left empty files, so
    // there was no way to see how far any of them had got
    let harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("something to keep"));

    let dir = common::scratch("files");
    let log = dir.join("s.jsonl").display().to_string();
    let state = dir.join("s.json").display().to_string();

    // the writer the program calls on its way out, where `say` has nowhere left to put a sentence
    let records = harness
        .app
        .write_session(&log, &state)
        .expect("it should have written");
    assert!(records > 0, "a session with a turn in it has records");

    let written = std::fs::read_to_string(&log).expect("the log is there");
    assert_eq!(
        written.lines().count(),
        records,
        "the count it reports is the count it wrote"
    );
    // and the snapshot is the thing `-r` takes, not merely a file that exists
    let snapshot: nachalnik::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("the snapshot is there"))
            .expect("the snapshot parses");
    let resumed = Kernel::resume(Config::default(), snapshot);
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("something to keep")),
        "what was in the session is in the session that comes back"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A resumed session reads back what its items used to say, off the record beside the snapshot.
///
/// note: a snapshot carries items and not events, and the text a rewrite replaced is an event -
/// `context.replaced`, the one that carries content. So resuming put every `v1` page back to
/// empty while the words were sitting in a file next to the one being read. `/save` writes the
/// two together under one name, which is what makes the log findable from the path `-r` was
/// handed.
#[tokio::test]
async fn a_resumed_session_reads_back_what_its_items_used_to_say() {
    let first = Harness::new([]);
    let id = first.app.kernel.push(ContextItem::user("the first draft"));
    first
        .app
        .kernel
        .replace(id, "the second draft")
        .expect("the item is there");

    let dir = common::scratch("recall");
    let log = dir.join("s.jsonl").display().to_string();
    let state = dir.join("s.json").display().to_string();
    first.app.write_session(&log, &state).expect("written");

    // a second session, resumed from the snapshot alone - which is all `-r` reads
    let snapshot: nachalnik::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("the snapshot is there"))
            .expect("the snapshot parses");
    let mut second = Harness::new([]);
    second.app.kernel = Kernel::resume(Config::default(), snapshot);

    assert_eq!(
        second.app.recall(Path::new(&state)),
        1,
        "the rewrite is in the log beside the snapshot"
    );
    second.app.replay();
    let screen = second.screen();
    assert!(
        screen.contains("earlier version(s)"),
        "a resume says what it picked up, including this: {screen}"
    );

    // and it is where it would have been if the session had never ended: a page under `enter`
    second.tab(Tab::Context);
    second.press(KeyCode::Home).await;
    second.press(KeyCode::Enter).await;
    let screen = second.screen();
    assert!(screen.contains("│ v1"), "{screen}");
    second.press(KeyCode::Right).await;
    let screen = second.screen();
    assert!(screen.contains("the first draft"), "{screen}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A record that is not there, or that ends mid-line, costs a page rather than a session.
///
/// note: the session has already resumed by the time the log is read, so nothing found there can
/// be worth failing over. The half-written line is the case that will actually happen: a run that
/// was killed wrote as far as it got, and the records before that point are fine.
#[tokio::test]
async fn a_session_resumed_without_its_record_is_still_a_session() {
    let first = Harness::new([]);
    let id = first.app.kernel.push(ContextItem::user("the first draft"));
    first
        .app
        .kernel
        .replace(id, "the second draft")
        .expect("the item is there");

    let dir = common::scratch("recall-torn");
    let log = dir.join("s.jsonl").display().to_string();
    let state = dir.join("s.json").display().to_string();
    first.app.write_session(&log, &state).expect("written");

    let resumed = |state: &str| {
        let snapshot: nachalnik::Snapshot =
            serde_json::from_str(&std::fs::read_to_string(state).expect("the snapshot is there"))
                .expect("the snapshot parses");
        let mut app = Harness::new([]);
        app.app.kernel = Kernel::resume(Config::default(), snapshot);

        app
    };

    // the log as it would be if the run had been killed part-way through writing its last record
    let whole = std::fs::read_to_string(&log).expect("the log is there");
    std::fs::write(&log, format!("{whole}{{\"seq\":99,\"at\":")).expect("a torn log");
    assert_eq!(
        resumed(&state).app.recall(Path::new(&state)),
        1,
        "the records before the torn one are still records"
    );

    // and with no log at all beside it, the session is the session and the pages are empty
    std::fs::remove_file(&log).expect("the log goes");
    let mut second = resumed(&state);
    assert_eq!(second.app.recall(Path::new(&state)), 0);
    second.tab(Tab::Context);
    second.press(KeyCode::Home).await;
    second.press(KeyCode::Enter).await;
    let screen = second.screen();
    assert!(screen.contains("the second draft"), "{screen}");
    assert!(!screen.contains("│ v1"), "{screen}");

    std::fs::remove_dir_all(&dir).ok();
}
