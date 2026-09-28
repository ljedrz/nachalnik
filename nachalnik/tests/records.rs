//! What the log and the snapshot look like on disk, held to the code in both directions.
//!
//! note: the fixtures under `tests/records/` are the schema a reader who is not this crate builds
//! against - a `jq` filter, a dashboard, a checker in another language - and these tests are what
//! make them one. Every past format's files still read, so a change that would misread a log
//! somebody already has is caught here. The current format's files are what this version writes,
//! byte for byte once parsed, so a fixture cannot drift from the types. And every event this
//! crate defines has one, so a new event arrives with its example.
//!
//! note: a directory per format, and a past one is never edited. A new event or a new field gets
//! its fixture in the current format's directory; a change that moves [`nachalnik::FORMAT`] starts
//! a new directory, and leaves the old one for these tests to keep reading.

use std::path::{Path, PathBuf};

use nachalnik::{Event, FORMAT, Record, Snapshot};
use serde_json::Value;

fn records() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/records")
}

/// Every file in one format's directory, as its name without `.json` and its parsed contents.
fn fixtures(format: u32) -> Vec<(String, Value)> {
    let dir = records().join(format!("format-{format}"));
    let mut found: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|it| it == "json"))
        .map(|path| {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).expect("a fixture");
            let value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
            (stem, value)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Every format this crate has ever written, which is every one it must still read.
fn formats() -> Vec<u32> {
    let mut found: Vec<u32> = std::fs::read_dir(records())
        .expect("the fixtures")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_prefix("format-")?.parse().ok()
        })
        .collect();
    found.sort_unstable();
    found
}

/// Every past format's fixtures still read, and read as the events they were written as.
#[test]
fn every_format_ever_written_still_reads() {
    let formats = formats();
    assert_eq!(
        formats.last(),
        Some(&FORMAT),
        "the current format has no fixtures"
    );

    for format in formats {
        for (name, value) in fixtures(format) {
            if name == "snapshot" {
                let snapshot: Snapshot = serde_json::from_value(value)
                    .unwrap_or_else(|e| panic!("format {format}'s snapshot: {e}"));
                assert!(
                    snapshot.problems().is_empty(),
                    "format {format}'s snapshot: {:?}",
                    snapshot.problems()
                );
                continue;
            }
            let record: Record = serde_json::from_value(value)
                .unwrap_or_else(|e| panic!("format {format}'s {name}: {e}"));
            assert_eq!(record.event.name(), name, "format {format}");
        }
    }
}

/// The current format's fixtures are exactly what this version writes.
///
/// note: compared as parsed JSON, so the fixtures can be laid out for a person to read. A field
/// in a fixture the types do not have, or one the types write and the fixture lacks, is a
/// difference here; the first of those would read without complaint and never be noticed.
#[test]
fn the_current_fixtures_are_what_this_version_writes() {
    for (name, value) in fixtures(FORMAT) {
        let written = match name.as_str() {
            "snapshot" => serde_json::to_value(
                serde_json::from_value::<Snapshot>(value.clone()).expect("a snapshot"),
            ),
            _ => serde_json::to_value(
                serde_json::from_value::<Record>(value.clone()).expect("a record"),
            ),
        }
        .expect("it writes");
        assert_eq!(written, value, "{name} reads back as something else");
    }
}

/// Every event this crate defines has a fixture in the current format.
///
/// note: read off the source, because `Event` is `#[non_exhaustive]` and a test outside the crate
/// cannot list its variants. The names are the `serde(rename)` on each, which is what a log calls
/// them; [`Event::Unknown`] is what a reader makes of a name it does not know and is written by
/// nothing, so it has none.
#[test]
fn every_event_has_a_fixture() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/event.rs"))
            .expect("the source");
    let from = source.find("pub enum Event {").expect("the enum");
    let to = from + source[from..].find("\n}\n").expect("its end");
    let defined: Vec<&str> = source[from..to]
        .split("#[serde(rename = \"")
        .skip(1)
        .filter_map(|rest| rest.split_once("\")]").map(|(name, _)| name))
        .collect();
    assert!(defined.len() > 30, "the names were not found: {defined:?}");

    let fixtures: Vec<String> = fixtures(FORMAT).into_iter().map(|(name, _)| name).collect();
    let missing: Vec<&&str> = defined
        .iter()
        .filter(|name| !fixtures.iter().any(|it| it == *name))
        .collect();
    assert!(
        missing.is_empty(),
        "no fixture in tests/records/format-{FORMAT} for {missing:?}"
    );
}

/// A log from a later version reads, one record at a time: an event this one does not know is
/// [`Event::Unknown`], and a field it does not know is passed over.
///
/// note: but an event it does know, missing what it needs, is an error. That is a log that is
/// wrong rather than newer, and reading it as unknown would hide it.
#[test]
fn a_later_log_reads_and_a_wrong_one_does_not() {
    let newer: Record = serde_json::from_value(serde_json::json!({
        "seq": 9,
        "at": 1,
        "event": { "event": "tool.teleported", "call": "c1", "to": "mars" },
    }))
    .expect("an unknown event reads");
    assert_eq!(newer.event, Event::Unknown);

    let widened: Record = serde_json::from_value(serde_json::json!({
        "seq": 9,
        "at": 1,
        "signed": "abc",
        "event": { "event": "session.finished", "why": "done" },
    }))
    .expect("a field nobody knows is passed over");
    assert_eq!(widened.event, Event::SessionFinished);

    let wrong = serde_json::from_value::<Record>(serde_json::json!({
        "seq": 9,
        "at": 1,
        "event": { "event": "tool.finished", "call": "c1" },
    }));
    assert!(wrong.is_err(), "{wrong:?}");
}

/// The record a session begins with says the format, and no other record does.
#[test]
fn the_format_is_on_the_record_a_session_begins_with() {
    let kernel = nachalnik::Kernel::new(nachalnik::Config::default());
    kernel.push(nachalnik::ContextItem::user("hello"));
    let resumed = nachalnik::Kernel::resume(nachalnik::Config::default(), kernel.snapshot());

    for history in [kernel.history(), resumed.history()] {
        let (first, rest) = history.split_first().expect("a record");
        assert!(
            matches!(
                first.event,
                Event::SessionStarted { .. } | Event::SessionResumed { .. }
            ),
            "{first:?}"
        );
        assert_eq!(first.format, Some(FORMAT));
        assert!(
            rest.iter().all(|record| record.format.is_none()),
            "{rest:?}"
        );
    }
}
