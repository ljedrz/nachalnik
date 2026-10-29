//! The other names a file has: the hard links to it in what a session reaches, so that a path rule
//! about one of them is not read past under another.
//!
//! note: a path rule is about a name. A symbolic link names its target, and `fs` checks where one
//! leads; a hard link names nothing, and is the file itself under a second name. So a file with
//! more than one link is looked for under every name it has, which only a walk can find.

use std::{
    collections::HashMap,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use nachalnik::Verdict;

use crate::sandbox::Reach;

use super::policy::path_matches;

/// The most entries a walk for names looks at before it stops.
///
/// note: the roots can be a whole home directory. Past this, a file whose names were not all found
/// is said to be unchecked rather than taken as having none that matter.
const LOOKED: usize = 200_000;

/// Every file with more than one name in what a session reaches, with the names found.
pub(super) struct Names {
    /// Each such file by its device and inode, with the names found for it, resolved.
    by_file: HashMap<(u64, u64), Vec<PathBuf>>,
    /// Whether the walk got to the end rather than stopping at [`LOOKED`].
    whole: bool,
}

/// What a file's other names come to.
pub(super) enum Other<'a> {
    /// One of them is matched by a rule `named` is not: the rule, and that name relative to the
    /// working directory.
    Barred(&'a (String, Verdict), PathBuf),
    /// They were not all found, so whether a rule matches one is not known: how many names it has.
    Unchecked(u64),
}

impl Names {
    /// Walks the working directory and every path opened up beside it, without following a link.
    ///
    /// note: a name outside them is not looked for. A confined command cannot link a file into its
    /// directories from outside them, since Landlock refuses that, so such a name was made by
    /// somebody outside the session; and `fs` cannot read the file under it.
    pub(super) fn of(reach: &Reach) -> Self {
        let mut names = Self {
            by_file: HashMap::new(),
            whole: true,
        };
        let mut looked = 0;
        // each directory once, so a root inside another is walked once
        let mut seen = std::collections::HashSet::new();
        let mut pending: Vec<PathBuf> = std::iter::once(&reach.workdir)
            .chain(&reach.writable)
            .chain(&reach.readable)
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| {
                std::fs::metadata(root).is_ok_and(|meta| seen.insert((meta.dev(), meta.ino())))
            })
            .collect();
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                looked += 1;
                if looked > LOOKED {
                    names.whole = false;
                    return names;
                }
                // `DirEntry::metadata` does not follow a link
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    if seen.insert((meta.dev(), meta.ino())) {
                        pending.push(entry.path());
                    }
                } else if meta.is_file() && meta.nlink() > 1 {
                    let found = names.by_file.entry((meta.dev(), meta.ino())).or_default();
                    if !found.contains(&entry.path()) {
                        found.push(entry.path());
                    }
                }
            }
        }

        names
    }

    /// What the names of `resolved` other than itself come to, where one of `barred` matches one
    /// of them and does not match `named`, the name it was asked for by; `None` where nothing bars
    /// it. `meta` is `resolved`'s, and `workdir` the working directory, resolved.
    ///
    /// note: matched against each name relative to the working directory, as
    /// [`led_past`](super::search::led_past) matches where a link leads, and the rules in the order
    /// given, so the first found is the strictest. A rule that matches `named` as well was
    /// consulted for it already.
    pub(super) fn other<'a>(
        &self,
        named: &str,
        resolved: &Path,
        meta: &std::fs::Metadata,
        workdir: &Path,
        barred: &'a [(String, Verdict)],
    ) -> Option<Other<'a>> {
        let found = self
            .by_file
            .get(&(meta.dev(), meta.ino()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let others: Vec<PathBuf> = found
            .iter()
            .filter(|name| name.as_path() != resolved)
            .map(|name| super::search::climbing(name, workdir))
            .collect();
        let rule = barred.iter().find_map(|rule| {
            let other = others
                .iter()
                .find(|other| path_matches(&rule.0, &other.to_string_lossy()))?;
            (!path_matches(&rule.0, named)).then(|| Other::Barred(rule, other.clone()))
        });
        let missing = (found.len() as u64) < meta.nlink();

        match rule {
            Some(rule) => Some(rule),
            None if missing && !self.whole => Some(Other::Unchecked(meta.nlink())),
            None => None,
        }
    }
}

/// Whether `meta` is of a file that has another name, which [`Names`] is for.
pub(super) fn shared(meta: &std::fs::Metadata) -> bool {
    meta.is_file() && meta.nlink() > 1
}
