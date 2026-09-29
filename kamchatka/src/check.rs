//! `--check`: reading a session's record the way somebody who is not this program would, and
//! saying what does not add up.
//!
//! note: a reader of the files and nothing else. No kernel is built and nothing is resumed, so
//! what this says about a record is what the record says about itself - the claim a record kept
//! for somebody else to read has to hold without this program in the room. It works on the text of
//! the two files rather than on paths, so a test hands it a log as a string.
//!
//! note: what it finds is said, not repaired, and a finding is not always a fault. A call that was
//! asked for and never finished is what a killed run leaves, and is worth knowing about for that
//! reason; a record numbered twice, or an item the log never added, is a record that something
//! other than this program has changed.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use nachalnik::{ContextId, ContextState, Event, FORMAT, Record, Snapshot};
use serde_json::Value;

/// The log and the snapshot a path names: `NAME.jsonl` and `NAME.json`, whether it was given as
/// either of those or as `NAME`.
///
/// note: the spellings `/load` takes, so a path that loads is a path that checks.
pub fn pair(path: &str) -> (PathBuf, PathBuf) {
    let stem = crate::app::without_suffix(path);

    (
        PathBuf::from(format!("{stem}.jsonl")),
        PathBuf::from(format!("{stem}.json")),
    )
}

/// What a check read, and what it found.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Checked {
    /// How many lines of the log read as records.
    pub records: usize,
    /// What does not add up, one sentence each, in the order it was met.
    pub findings: Vec<String>,
}

/// Checks a log, a snapshot, or the pair.
///
/// note: the snapshot is compared with the log only where both are given, and read on its own
/// for [`Snapshot::problems`] where it is the only one.
pub fn check(log: Option<&str>, snapshot: Option<&str>) -> Checked {
    let mut checked = Checked::default();

    let records = log.map(|log| read(log, &mut checked)).unwrap_or_default();
    let snapshot = snapshot.and_then(|text| match serde_json::from_str::<Snapshot>(text) {
        Ok(snapshot) => Some(snapshot),
        Err(e) => {
            checked
                .findings
                .push(format!("the snapshot is not a session: {e}"));
            None
        }
    });

    if log.is_some() {
        numbering(&records, &mut checked.findings);
        calls(&records, &mut checked.findings);
    }
    if let Some(snapshot) = &snapshot {
        checked.findings.extend(
            snapshot
                .problems()
                .into_iter()
                .map(|problem| format!("the snapshot: {problem}")),
        );
        if log.is_some() {
            agrees(&records, snapshot, &mut checked.findings);
        }
    }

    checked
}

/// Every line of the log that reads as a record, with the line it was on.
///
/// note: a line that does not read is named and passed over rather than ending the walk, the way
/// `App::recall` reads one. The last line of a log a killed run left is the one most likely to be
/// half a record, and the ones before it are fine.
fn read(log: &str, checked: &mut Checked) -> Vec<(usize, Record)> {
    let mut records = Vec::new();
    for (at, line) in log.lines().enumerate() {
        let line_no = at + 1;
        if line.trim().is_empty() {
            continue;
        }
        let record = match serde_json::from_str::<Record>(line) {
            Ok(record) => record,
            Err(e) => {
                checked
                    .findings
                    .push(format!("line {line_no} is not a record: {e}"));
                continue;
            }
        };
        // the name read off the line itself, since what an unknown event was called is the one
        // thing `Event::Unknown` does not keep
        if record.event == Event::Unknown {
            let named = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value["event"]["event"].as_str().map(str::to_owned))
                .unwrap_or_default();
            checked.findings.push(format!(
                "record {} is an event this version does not know: `{named}`",
                record.seq
            ));
        }
        if let Some(format) = record.format.filter(|format| *format > FORMAT) {
            checked.findings.push(format!(
                "record {} begins a session written in format {format}, and this reads format \
                 {FORMAT} and earlier",
                record.seq
            ));
        }
        checked.records += 1;
        records.push((line_no, record));
    }

    records
}

/// Records numbered out of step with the one before them.
///
/// note: one after another, and nothing about where the first starts. A log begun by a resume
/// starts past where its snapshot was taken, and a log whose early records were drained starts
/// wherever the drain left it; neither is a gap. A gap is between two records.
fn numbering(records: &[(usize, Record)], findings: &mut Vec<String>) {
    for pair in records.windows(2) {
        let ((was, before), (line, after)) = (&pair[0], &pair[1]);
        match after.seq.checked_sub(before.seq) {
            Some(1) => {}
            Some(0) | None => findings.push(format!(
                "record {} on line {line} comes after record {}: two records are numbered the \
                 same, or the numbers went back{}",
                after.seq,
                before.seq,
                match after.event {
                    Event::SessionResumed { .. } => {
                        ", at a resume from a snapshot taken before the end of this log"
                    }
                    _ => "",
                }
            )),
            Some(2) => findings.push(format!(
                "record {} is missing, between line {was} and line {line}",
                before.seq + 1
            )),
            Some(skipped) => findings.push(format!(
                "records {} to {} are missing, between line {was} and line {line}",
                before.seq + 1,
                before.seq + skipped - 1
            )),
        }
    }
}

/// Whether the log begins where a session does, so that what came before its first record is
/// nothing rather than something it does not say.
///
/// note: a caller may drain a kernel's log and keep only what came after, and that log starts in
/// the middle of a session. Read as a whole one, every item and every call from before the cut
/// would be named as something the log never added or never asked for.
fn begins(records: &[(usize, Record)]) -> bool {
    records.first().is_none_or(|(_, record)| {
        matches!(
            record.event,
            Event::SessionStarted { .. } | Event::SessionResumed { .. }
        )
    })
}

/// Calls asked for and never finished, and calls finished that were never asked for.
///
/// note: a session at a time. Nothing waiting is carried across a resume - a resumed kernel starts
/// with no calls pending - so a call left open when the next session begins stays open for good,
/// and that is where it is said.
fn calls(records: &[(usize, Record)], findings: &mut Vec<String>) {
    let mut open: Vec<(String, String, u64)> = Vec::new();
    let mut asked: HashSet<String> = HashSet::new();
    // whether a call finished here could have been asked for before the log begins
    let mut cut = !begins(records);

    let close = |open: &mut Vec<(String, String, u64)>, findings: &mut Vec<String>, why: &str| {
        for (call, tool, seq) in open.drain(..) {
            findings.push(format!(
                "the call `{call}` to `{tool}`, asked for at record {seq}, never finished: {why}"
            ));
        }
    };

    for (_, record) in records {
        match &record.event {
            Event::SessionStarted { .. } | Event::SessionResumed { .. } => {
                close(&mut open, findings, "another session begins after it");
                asked.clear();
                cut = false;
            }
            Event::ToolRequested { call, tool, .. } => {
                if !asked.insert(call.0.clone()) {
                    findings.push(format!(
                        "record {} asks for the call `{}` a second time",
                        record.seq, call.0
                    ));
                }
                open.push((call.0.clone(), tool.clone(), record.seq));
            }
            Event::ToolFinished { call, .. } => {
                match open.iter().position(|(open, ..)| *open == call.0) {
                    Some(at) => {
                        open.remove(at);
                    }
                    None if cut && !asked.contains(&call.0) => {}
                    None => findings.push(format!(
                        "record {} finishes the call `{}`, which {}",
                        record.seq,
                        call.0,
                        match asked.contains(&call.0) {
                            true => "had already finished",
                            false => "this session never asked for",
                        }
                    )),
                }
            }
            _ => {}
        }
    }

    close(&mut open, findings, "the log ends before it does");
}

/// Where the snapshot and the log, read up to the record the snapshot was taken at, disagree
/// about which items there are and what state each is in.
///
/// note: what the log can say about an item is less than what a snapshot holds, on purpose - it
/// names items rather than copying them - and this compares only what the log says. A session
/// begun by a resume starts from items the log does not name, so there the count is compared and
/// each item the log does name; an undo or a redo puts an item back into a state the log does not
/// repeat, so the state of an item one touched is not compared.
fn agrees(records: &[(usize, Record)], snapshot: &Snapshot, findings: &mut Vec<String>) {
    if snapshot.last_seq == 0 {
        findings.push(
            "the snapshot does not say which record it was taken at, so it is not compared with \
             the log"
                .to_owned(),
        );
        return;
    }
    let Some(taken) = records
        .iter()
        .rposition(|(_, record)| record.seq == snapshot.last_seq)
    else {
        let last = records.last().map_or(0, |(_, record)| record.seq);
        findings.push(match last < snapshot.last_seq {
            true => format!(
                "the snapshot was taken at record {}, and the log ends at record {last}",
                snapshot.last_seq
            ),
            false => format!(
                "the snapshot was taken at record {}, and the log has no record numbered that",
                snapshot.last_seq
            ),
        });
        return;
    };

    // `None` for an item whose state the log does not say
    let mut items: HashMap<ContextId, Option<ContextState>> = HashMap::new();
    // how many items there are, where a resume began from some the log does not name
    let mut resumed: Option<usize> = None;
    // where the log begins part way through a session, what came before its first record is
    // not known, and the count is not either
    let mut cut = !begins(&records[..=taken]);
    let count = |resumed: &mut Option<usize>, by: isize| {
        if let Some(count) = resumed {
            *count = count.saturating_add_signed(by);
        }
    };
    for (_, record) in &records[..=taken] {
        match &record.event {
            Event::SessionStarted { .. } => {
                items.clear();
                resumed = None;
                cut = false;
            }
            Event::SessionResumed { items: brought, .. } => {
                items.clear();
                resumed = Some(*brought);
                cut = false;
            }
            Event::ContextAdded { id, .. } => {
                items.insert(*id, Some(ContextState::Active));
                count(&mut resumed, 1);
            }
            Event::ContextChanged { id, from, to, .. } => {
                match items.get(id) {
                    Some(Some(was)) if was != from => findings.push(format!(
                        "record {} changes item {id} from {from:?}, where the log had it {was:?}",
                        record.seq
                    )),
                    None if resumed.is_none() && !cut => findings.push(format!(
                        "record {} changes item {id}, which the log never added",
                        record.seq
                    )),
                    _ => {}
                }
                items.insert(*id, Some(*to));
            }
            Event::ContextUndone {
                removed, changed, ..
            } => {
                for id in removed {
                    items.remove(id);
                    count(&mut resumed, -1);
                }
                for id in changed {
                    items.insert(*id, None);
                }
            }
            Event::ContextRedone {
                restored, changed, ..
            } => {
                for id in restored.iter().chain(changed) {
                    items.insert(*id, None);
                }
                count(&mut resumed, restored.len() as isize);
            }
            _ => {}
        }
    }

    let held: HashSet<ContextId> = snapshot.items.iter().map(|item| item.id).collect();
    for item in &snapshot.items {
        match items.get(&item.id) {
            Some(Some(state)) if *state != item.state => findings.push(format!(
                "the snapshot has item {} {:?}, and the log leaves it {state:?}",
                item.id, item.state
            )),
            None if resumed.is_none() && !cut => findings.push(format!(
                "the snapshot has item {}, which the log never added",
                item.id
            )),
            _ => {}
        }
    }
    let mut gone: Vec<&ContextId> = items.keys().filter(|id| !held.contains(id)).collect();
    gone.sort();
    for id in gone {
        findings.push(format!(
            "the log leaves item {id} in the context, and the snapshot does not have it"
        ));
    }
    if let Some(counted) = resumed
        && counted != snapshot.items.len()
    {
        findings.push(format!(
            "the log leaves {counted} items in the context, and the snapshot has {}",
            snapshot.items.len()
        ));
    }
}
