//! `kamchatka reconcile`: several hard forks of one session folded into one session to carry on
//! from.
//!
//! note: nothing in the runtime knows what a reconcile is, as nothing in it knows what a fork is.
//! It is a fold over [`Snapshot`]s into one more, and the session it makes is the runtime's own:
//! [`Kernel::resume`] of the shared part, and one [`Kernel::push_all`] of what the forks bring to
//! it. So the fresh log begins with `session.resumed` and names everything taken in the
//! `context.added` records after it, provenance included, and the forks' own logs are read and
//! never merged. Two append-only logs interleaved are a record of something no observer saw.
//!
//! note: it carries findings, not transcripts. The shared part is kept whole, and from each fork
//! only what the agent wrote down for itself: the `Reference` items whose source is `agent`. The
//! shared part plus every fork's turns would be a conversation in which the work was done twice in
//! an order that never happened, and a turn's signed reasoning is only valid on the turn and the
//! model that made it.
//!
//! note: two notes that contradict each other are both carried. A contradiction the model can
//! see is what `conflict` in `nachalnik-eval` measures, and leaving one note out would be choosing
//! a side for the model. Which fork a note came from is in its metadata and in the manifest, and
//! not in its label: the label is the one field of an item a model reads without `look`, and
//! `a/plan` would leave `label:plan` naming neither note.

use std::collections::{BTreeMap, HashSet};

use nachalnik::{
    Calibration, Config, Content, ContextId, ContextItem, ContextKind, ContextState, Event, Kernel,
    Record, Snapshot,
};
use serde_json::{Value, json};

use crate::app::{App, beside};

/// One fork as it was read: what it was called on the command line, its snapshot, and the log
/// beside it where there was one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Fork {
    /// The name the fork is known by in what this says, which is the path it was given as.
    pub name: String,
    /// Where the fork ended up.
    pub snapshot: Snapshot,
    /// The fork's log, as far as it read; empty where there was none.
    pub log: Vec<Record>,
}

impl Fork {
    /// A fork from what is already in hand.
    pub fn new(name: impl Into<String>, snapshot: Snapshot, log: Vec<Record>) -> Self {
        Self {
            name: name.into(),
            snapshot,
            log,
        }
    }

    /// Reads the pair `path` names, in the spellings `/load` and `--check` take.
    ///
    /// note: a snapshot with [`Snapshot::problems`] is refused, as `-r` refuses one. A session
    /// made out of a record that had to be repaired to be read would say something its record no
    /// longer does. The log is optional: without one, a fork's model is not known and nothing it
    /// overwrote can be read back.
    pub fn read(path: &str) -> Result<Self, String> {
        let (log, state) = crate::check::pair(path);
        let snapshot: Snapshot = serde_json::from_slice(
            &std::fs::read(&state)
                .map_err(|e| format!("could not read {}: {e}", state.display()))?,
        )
        .map_err(|e| format!("{} is not a session: {e}", state.display()))?;
        let problems = snapshot.problems();
        if !problems.is_empty() {
            return Err(format!(
                "{} is a session this will not reconcile: {}",
                state.display(),
                problems.join("; ")
            ));
        }
        let log = match std::fs::read_to_string(&log) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str::<Record>(line).ok())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("could not read {}: {e}", log.display())),
        };

        Ok(Self::new(path, snapshot, log))
    }

    /// The model the fork's log last says it was talking to.
    fn model(&self) -> Option<String> {
        crate::args::talked_to(&self.log)
            .map(|info| info.model)
            .filter(|model| !model.is_empty())
    }

    /// Whether the fork's log says item `id` once said `content`, before something overwrote it.
    fn overwrote(&self, id: ContextId, content: &Content) -> bool {
        self.log.iter().any(|record| {
            matches!(&record.event, Event::ContextReplaced { id: was_id, was, .. }
                if *was_id == id && was == content)
        })
    }
}

/// What a reconcile made: the session, its log, and what a person should be told about it.
#[derive(Debug)]
#[non_exhaustive]
pub struct Reconciled {
    /// The session to carry on from.
    pub snapshot: Snapshot,
    /// Its log, which begins at the resume of the shared part.
    pub records: Vec<Record>,
    /// The last item every fork shares.
    pub shared_through: ContextId,
    /// What was decided on the way, one sentence each, for the person who asked.
    pub said: Vec<String>,
    /// The model every fork was last talking to, where they agree and their logs say.
    pub model: Option<String>,
}

/// An item of the shared part, as the forks hold it between them.
struct Shared {
    item: ContextItem,
    /// The fork whose revision of the item is the one kept, where one fork changed it.
    revised_in: Option<usize>,
    /// Whether the forks hold it in different states.
    states_differ: bool,
}

/// Whether two forks' items at one place are the same item, whatever each has since said in it.
///
/// note: everything that does not change once an item is added. Content is the one exception that
/// can, by a revision, and is compared by the caller; state, note, metadata and the counts are
/// what a fork does to an item, not what the item is.
fn same_item(a: &ContextItem, b: &ContextItem) -> bool {
    a.id == b.id
        && a.kind == b.kind
        && a.source == b.source
        && a.label == b.label
        && a.included_because == b.included_because
}

/// How much of an item reaches the model, for choosing between the states the forks left it in.
///
/// note: the most included wins. Nothing is destroyed by it - an item moved out of a request is
/// moved back in, and a compactor elides it again if the session needs the room - and a pin is a
/// promise somebody made in one of the forks that the others had no reason to break.
fn reach(state: ContextState) -> u8 {
    match state {
        ContextState::Pinned => 3,
        ContextState::Active => 2,
        ContextState::Elided => 1,
        _ => 0,
    }
}

/// Folds `forks` into one session named `session`.
///
/// note: the shared part is the longest run of items every fork holds alike, compared place by
/// place. Identifiers come from one counter that never hands one out twice, so forks of one
/// ancestor hold the same items under the same numbers and then each numbers its own next item
/// the same. If the first items already differ these are not forks of one session, and it is
/// refused rather than concatenated.
///
/// note: an item a fork *revised* is the one conflict. It is the same item with different
/// content, and only something besides the content says which: the item after it agreeing in every
/// fork, or a fork's log holding the overwrite. With neither, two forks' next items that happen to
/// share a kind and a label - two user messages - could be taken for one revised, so the forks are
/// taken to part there. Where the item is shared, one fork's revision is kept; two forks revising
/// it differently is refused, naming both.
///
/// note: the carried notes are numbered past every identifier any fork handed out, so no number in
/// this session ever names something else in one of the forks. The manifest is numbered first of
/// them and pinned: it says what the session is made of, which the session cannot work out.
///
/// note: the calibration is summed where every fork was talking to the same model, which is what
/// it learned about, and dropped otherwise - a counter's correction for one model is meaningless
/// for another, and nothing learned is a state a snapshot already holds.
pub fn reconcile(forks: &[Fork], session: &str) -> Result<Reconciled, String> {
    let names: Vec<&str> = forks.iter().map(|fork| fork.name.as_str()).collect();
    if forks.len() < 2 {
        return Err("a reconcile takes two forks or more".to_owned());
    }

    let mut shared: Vec<Shared> = Vec::new();
    for at in 0.. {
        let Some(versions) = forks
            .iter()
            .map(|fork| fork.snapshot.items.get(at))
            .collect::<Option<Vec<&ContextItem>>>()
        else {
            break;
        };
        let first = versions[0];
        if !versions.iter().all(|version| same_item(first, version)) {
            break;
        }

        let mut revised_in = None;
        if !versions
            .iter()
            .all(|version| version.content == first.content)
        {
            let Some(kept) = revision(forks, &versions, at)? else {
                break;
            };
            revised_in = Some(kept);
        }

        let chosen = (0..versions.len())
            .rev()
            .max_by_key(|&fork| reach(versions[fork].state))
            .unwrap_or(0);
        let states_differ = versions
            .iter()
            .any(|version| version.state != versions[chosen].state);
        let mut item = versions[revised_in.unwrap_or(chosen)].clone();
        item.state = versions[chosen].state;
        item.note = versions[chosen].note.clone();
        if states_differ || revised_in.is_some() {
            let states: BTreeMap<&str, ContextState> = names
                .iter()
                .zip(&versions)
                .map(|(name, version)| (*name, version.state))
                .collect();
            stamp(
                &mut item,
                match revised_in {
                    Some(fork) => json!({ "states": states, "revised_in": names[fork] }),
                    None => json!({ "states": states }),
                },
            );
        }
        shared.push(Shared {
            item,
            revised_in,
            states_differ,
        });
    }

    let Some(last) = shared.last() else {
        return Err(format!(
            "{} are not forks of one session: their first items already differ",
            listed(&names)
        ));
    };
    let shared_through = last.item.id;

    let start = forks
        .iter()
        .map(|fork| fork.snapshot.next_item)
        .max()
        .unwrap_or(1);
    let mut carried: Vec<(usize, ContextItem)> = Vec::new();
    let mut tails = Vec::new();
    for (index, fork) in forks.iter().enumerate() {
        let tail = &fork.snapshot.items[shared.len()..];
        tails.push(tail.len());
        let notes = tail
            .iter()
            .filter(|item| matches!(item.kind, ContextKind::Reference) && item.source == "agent");
        for note in notes {
            let mut item = note.clone();
            stamp(&mut item, json!({ "from": fork.name, "id": note.id }));
            item.id = ContextId::UNASSIGNED;
            carried.push((index, item));
        }
    }
    // numbered as `push_all` will number them: the manifest at `start`, the notes after it
    let numbered: Vec<(usize, ContextId, &ContextItem)> = carried
        .iter()
        .enumerate()
        .map(|(nth, (fork, item))| (*fork, ContextId(start + 1 + nth as u64), item))
        .collect();

    let mut said = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let notes = numbered.iter().filter(|(fork, ..)| *fork == index).count();
        said.push(format!(
            "{name}: {}, and {} left behind",
            match notes {
                0 => "nothing written down".to_owned(),
                1 => "1 note carried".to_owned(),
                n => format!("{n} notes carried"),
            },
            match tails[index] - notes {
                1 => "1 item".to_owned(),
                n => format!("{n} items"),
            }
        ));
    }
    for kept in &shared {
        if let Some(fork) = kept.revised_in {
            said.push(format!(
                "item {} is as {} revised it after the forks parted",
                kept.item.id, names[fork]
            ));
        }
    }
    let differing = shared.iter().filter(|kept| kept.states_differ).count();
    if differing > 0 {
        said.push(format!(
            "{differing} shared item{} in a different state in each fork, and given the most \
             included of them",
            match differing {
                1 => " is",
                _ => "s are",
            }
        ));
    }

    let clashes = clashes(&shared, &numbered, &names);
    said.extend(clashes.iter().map(|clash| {
        format!(
            "`{}` is the label of more than one note: {}",
            clash.label,
            clash.held.join(", ")
        )
    }));
    let manifest = manifest(&names, shared_through, &numbered, &tails, &shared, &clashes);

    let mut merged = forks[0].snapshot.clone();
    merged.format = nachalnik::FORMAT;
    merged.session = session.to_owned();
    merged.items = shared.into_iter().map(|kept| kept.item).collect();
    merged.next_item = start;
    merged.last_seq = forks.iter().map(|f| f.snapshot.last_seq).max().unwrap_or(0);
    merged.next_permission = forks
        .iter()
        .map(|f| f.snapshot.next_permission)
        .max()
        .unwrap_or(0);
    // every call any fork made stays used, carried or not: a provider handing one of them back in
    // this session is the reuse the kernel repairs, and only a reserved identifier is repaired.
    // No call is carried - a note names none - so no result here can be paired with another
    // fork's call
    let mut used: Vec<_> = forks
        .iter()
        .flat_map(|fork| fork.snapshot.used_calls.iter().cloned())
        .collect();
    used.sort();
    used.dedup();
    merged.used_calls = used;

    let models: Vec<Option<String>> = forks.iter().map(Fork::model).collect();
    let model = match models.first() {
        Some(Some(first)) if models.iter().all(|m| m.as_ref() == Some(first)) => {
            Some(first.clone())
        }
        _ => None,
    };
    // what a counter that learns says before it has learned anything is a calibration too, and
    // not one worth a sentence
    let learned: Vec<Calibration> = forks
        .iter()
        .filter_map(|fork| fork.snapshot.calibration)
        .filter(|calibration| calibration.observations > 0)
        .collect();
    merged.calibration = match &model {
        Some(_) => summed(learned.iter().copied()),
        None => None,
    };
    if !learned.is_empty() {
        said.push(match (&model, merged.calibration) {
            (Some(model), Some(_)) => {
                format!("the token counter's correction is summed: every fork talked to {model}")
            }
            _ => format!(
                "the token counter's correction is dropped: {}",
                models
                    .iter()
                    .zip(&names)
                    .map(|(model, name)| match model {
                        Some(model) => format!("{name} was talking to {model}"),
                        None => format!("{name} has no log that says what it talked to"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    let mut dropped = Vec::new();
    merged.params.retain(|key, value| {
        let agreed = forks[1..]
            .iter()
            .all(|fork| fork.snapshot.params.get(key) == Some(value));
        if !agreed {
            dropped.push(format!("`{key}`"));
        }
        agreed
    });
    for fork in &forks[1..] {
        for key in fork.snapshot.params.keys() {
            let named = format!("`{key}`");
            if !merged.params.contains_key(key) && !dropped.contains(&named) {
                dropped.push(named);
            }
        }
    }
    if !dropped.is_empty() {
        said.push(format!(
            "{} left out of the parameters, which the forks do not agree on",
            listed(&dropped)
        ));
    }

    let expected: Vec<ContextId> = std::iter::once(ContextId(start))
        .chain(numbered.iter().map(|(_, id, _)| *id))
        .collect();
    let kernel = Kernel::resume(
        Config {
            session_name: Some(session.to_owned()),
            ..Config::default()
        },
        merged,
    );
    let ids =
        kernel.push_all(std::iter::once(manifest).chain(carried.into_iter().map(|(_, item)| item)));
    // the manifest was written before the notes were numbered, and names them by number
    if ids != expected {
        return Err(format!(
            "the notes were numbered {ids:?} and the manifest names them {expected:?}, so nothing \
             was written"
        ));
    }

    Ok(Reconciled {
        snapshot: kernel.snapshot(),
        records: kernel.history(),
        shared_through,
        said,
        model,
    })
}

/// Which fork's revision of a shared item to keep, where the forks hold it with different
/// content; `None` where nothing says it is one item, which is where the forks part.
fn revision(forks: &[Fork], versions: &[&ContextItem], at: usize) -> Result<Option<usize>, String> {
    let id = versions[0].id;
    let names: Vec<&str> = forks.iter().map(|fork| fork.name.as_str()).collect();

    // the content the item had when the forks parted: the one every fork either still holds or
    // overwrote, by its log; or, failing that, the one every fork that did not revise it holds
    let overwritten = forks.iter().find_map(|fork| {
        fork.log.iter().find_map(|record| match &record.event {
            Event::ContextReplaced {
                id: was_id, was, ..
            } if *was_id == id
                && versions
                    .iter()
                    .zip(forks)
                    .all(|(v, f)| v.content == *was || f.overwrote(id, was)) =>
            {
                Some(was.clone())
            }
            _ => None,
        })
    });
    let next_agrees = forks
        .iter()
        .map(|fork| fork.snapshot.items.get(at + 1))
        .collect::<Option<Vec<&ContextItem>>>()
        .is_some_and(|next| {
            next.iter()
                .all(|item| same_item(next[0], item) && item.content == next[0].content)
        });
    if overwritten.is_none() && !next_agrees {
        return Ok(None);
    }

    let unrevised: Vec<&Content> = versions
        .iter()
        .filter(|version| version.meta.get("revised").is_none())
        .map(|version| &version.content)
        .collect();
    let original = overwritten.or_else(|| {
        unrevised
            .first()
            .filter(|first| unrevised.iter().all(|content| content == *first))
            .map(|first| (*first).clone())
    });

    let mut revisions: Vec<usize> = Vec::new();
    for (fork, version) in versions.iter().enumerate() {
        if Some(&version.content) != original.as_ref()
            && !revisions
                .iter()
                .any(|&seen| versions[seen].content == version.content)
        {
            revisions.push(fork);
        }
    }

    match revisions[..] {
        [one] => Ok(Some(one)),
        _ => Err(format!(
            "item {id} is shared by the forks and says something different in {}, so there is no \
             one version of it to carry on from. Undo all but one of the revisions, or make them \
             say the same, and reconcile again",
            listed(
                &revisions
                    .iter()
                    .map(|&fork| names[fork])
                    .collect::<Vec<_>>()
            )
        )),
    }
}

/// The calibrations summed and the scale left for the counter to derive.
///
/// note: summed rather than averaged, because the figures are totals of what was estimated and
/// what was charged, and a ratio of totals is what the counter keeps. The shared part's requests
/// are in every fork's figures, which weighs them more; the ratio is still one the model charged.
fn summed(calibrations: impl Iterator<Item = Calibration>) -> Option<Calibration> {
    calibrations.reduce(|mut sum, next| {
        sum.observations += next.observations;
        sum.estimated += next.estimated;
        sum.reported += next.reported;
        sum.scale = match sum.estimated {
            0 => 1.0,
            estimated => sum.reported as f64 / estimated as f64,
        };
        sum
    })
}

/// A label more than one note in this session carries, and which items carry it.
struct Clash {
    label: String,
    held: Vec<String>,
}

/// The labels more than one note carries, where at least one of them is carried from a fork.
///
/// note: only notes the model is shown, and never `note`, which is what a note nobody labelled is
/// called - two of those share a name nobody chose.
fn clashes(
    shared: &[Shared],
    numbered: &[(usize, ContextId, &ContextItem)],
    names: &[&str],
) -> Vec<Clash> {
    let note = |item: &ContextItem| {
        matches!(item.kind, ContextKind::Reference)
            && item.source == "agent"
            && item.state.sends_content()
            && item.label != "note"
    };
    let mut labels: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for kept in shared.iter().filter(|kept| note(&kept.item)) {
        labels
            .entry(&kept.item.label)
            .or_default()
            .push(format!("[{}] shared", kept.item.id));
    }
    let mut from_a_fork = HashSet::new();
    for (fork, id, item) in numbered.iter().filter(|(_, _, item)| note(item)) {
        from_a_fork.insert(item.label.as_str());
        labels
            .entry(&item.label)
            .or_default()
            .push(format!("[{id}] from {}", names[*fork]));
    }

    labels
        .into_iter()
        .filter(|(label, held)| held.len() > 1 && from_a_fork.contains(label))
        .map(|(label, held)| Clash {
            label: label.to_owned(),
            held,
        })
        .collect()
}

/// The pinned item that says what this session is made of.
///
/// note: a system instruction, as the one a `fork` is given is, and said for the same reason: the
/// session cannot work it out. It is the read-time counterpart of what `note` says at write time
/// when a label is already taken.
fn manifest(
    names: &[&str],
    shared_through: ContextId,
    numbered: &[(usize, ContextId, &ContextItem)],
    tails: &[usize],
    shared: &[Shared],
    clashes: &[Clash],
) -> ContextItem {
    let mut text = format!(
        "This session is {} forks of one session put back together: {}. They agree up to item \
         {shared_through}, and everything up to there is that shared conversation. After it each \
         fork went on alone ({}), and none of that is here except the notes each wrote down for \
         itself, which follow, numbered as they are in this session.",
        names.len(),
        listed(names),
        names
            .iter()
            .zip(tails)
            .map(|(name, tail)| format!("{name}: {tail} item{}", if *tail == 1 { "" } else { "s" }))
            .collect::<Vec<_>>()
            .join(", "),
    );
    for (index, name) in names.iter().enumerate() {
        let notes: Vec<String> = numbered
            .iter()
            .filter(|(fork, ..)| *fork == index)
            .map(|(_, id, item)| format!("[{id}] `{}`", item.label))
            .collect();
        text.push_str(&format!(
            "\n- {name}: {}",
            match notes.is_empty() {
                true => "nothing written down".to_owned(),
                false => notes.join(", "),
            }
        ));
    }
    for clash in clashes {
        text.push_str(&format!(
            "\n`{}` is the label of more than one note here - {} - and they may not agree: each \
             says what its own fork concluded.",
            clash.label,
            clash.held.join(", ")
        ));
    }
    for kept in shared {
        if let Some(fork) = kept.revised_in {
            text.push_str(&format!(
                "\nItem {} is as {} revised it after the forks parted.",
                kept.item.id, names[fork]
            ));
        }
    }

    let mut item = ContextItem::new(ContextKind::System, "reconcile", "reconciled", text)
        .with_meta(json!({ "reconciled": { "forks": names, "shared_through": shared_through } }))
        .pinned();
    item.note = Some("what this session is made of, which it cannot work out for itself".into());

    item
}

/// Writes `reconciled` beside itself under `into`, refusing to write over anything there.
///
/// note: refused rather than overwritten, unlike `/save`. What is at that path may be one of the
/// forks, and a reconcile written over its own source is the one fork that is gone for good.
pub fn write(reconciled: &Reconciled, into: &str) -> Result<(String, String), String> {
    let (log, state) = crate::check::pair(into);
    let (log, state) = (log.display().to_string(), state.display().to_string());
    for path in [&log, &state] {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "{path} is already there, and a reconcile writes over nothing"
            ));
        }
    }

    let records = reconciled
        .records
        .iter()
        .map(|record| {
            serde_json::to_string(record)
                .map_err(|e| format!("could not render record {}: {e}", record.seq))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let snapshot = serde_json::to_vec_pretty(&reconciled.snapshot)
        .map_err(|e| format!("could not render the session: {e}"))?;

    let log_beside = beside(&log, (records.join("\n") + "\n").as_bytes())
        .map_err(|e| format!("could not write {log}: {e}"))?;
    let state_beside = beside(&state, &snapshot).map_err(|e| {
        let _ = std::fs::remove_file(&log_beside);
        format!("could not write {state}: {e}")
    })?;
    std::fs::rename(&log_beside, &log).map_err(|e| {
        let _ = std::fs::remove_file(&log_beside);
        let _ = std::fs::remove_file(&state_beside);
        format!("could not write {log}: {e}")
    })?;
    std::fs::rename(&state_beside, &state).map_err(|e| {
        let _ = std::fs::remove_file(&state_beside);
        format!("wrote {log}, and could not write {state} beside it: {e}")
    })?;

    Ok((log, state))
}

/// A name for the session a reconcile makes: a new one, because it is not any of the forks.
pub fn session_name() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();

    App::session_stamp(now)
}

/// Gives an item's metadata a `reconciled` entry, leaving what else it holds.
///
/// note: metadata that is not an object is left alone rather than replaced, because it is
/// somebody else's and this cannot read it; the manifest says the same thing anyway.
fn stamp(item: &mut ContextItem, reconciled: Value) {
    match &mut item.meta {
        Value::Object(meta) => {
            meta.insert("reconciled".to_owned(), reconciled);
        }
        Value::Null => item.meta = json!({ "reconciled": reconciled }),
        _ => {}
    }
}

/// `a`, `a and b`, `a, b and c`.
fn listed<S: AsRef<str>>(names: &[S]) -> String {
    match names {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [init @ .., last] => format!(
            "{} and {}",
            init.iter()
                .map(|name| name.as_ref())
                .collect::<Vec<_>>()
                .join(", "),
            last.as_ref()
        ),
    }
}
