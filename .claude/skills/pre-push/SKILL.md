---
name: pre-push
description: Review a branch of this workspace before it is pushed - its commits and diff against the repository's own rules (changelog entries, `#[non_exhaustive]`, semver, MAP.md, tests that catch something, plain prose, commit messages) and against CI, run here - and give a Clean, Minor or Blocking verdict with the evidence for each finding. Needs no model key. Use when asked to "check my branch", "review before I push", "is this ready", "pre-push", or to review a branch or a range of commits.
---

# before a branch is pushed

The question is whether the branch keeps the rules this workspace sets itself: AGENTS.md has them
in a line each, CONTRIBUTING.md in full. Half of it can be read off the diff and the commits by a
script; the other half is reading the diff with the rules in mind. Report what you find. Fix
something only when the person asks, and never push: AGENTS.md leaves pushing to the person.

## 1. what the script finds

```sh
SKILL=<repo>/.claude/skills/pre-push
python3 $SKILL/check.py [BASE]          # BASE defaults to master
```

It compares the branch from where it left BASE, uncommitted changes included, and sorts what it
finds:

- **BLOCKING**: what looks like a key in an added line (shown cut short, never whole), a conflict
  marker, `dbg!`/`todo!`/`unimplemented!` in library or program code, and a break `cargo
  semver-checks` finds in a crate whose unreleased changelog has no `breaking` entry.
- **LOOK AT**: a commit that changes a crate's source without its changelog and without saying
  why ("No changelog entry: ...", "Tests only."); a public enum without `#[non_exhaustive]`; a
  test added `#[ignore]`d, or one removed; a file MAP.md does not follow; a commit subject that is
  not `crate: what changed, in one lowercase line`.
- **FOR THE REVIEW**: the tests the branch adds.

A LOOK AT line is a lead, not a finding: open it and decide. A commit can leave the changelog out
rightly (a private type, a refactor nobody can see) and only forget to say so; an enum can never
gain a variant. `--no-semver` skips the semver check, which builds each changed crate's docs
twice.

## 2. what CI would say

```sh
python3 <repo>/.claude/skills/release/ci.py lint     # formatting, clippy in every shape, docs, references
python3 <repo>/.claude/skills/release/ci.py test     # the builds, the feature shapes, the tests, the examples
```

These are `ci.yml`'s own steps, read from it (see the `release` skill). For a branch that changes
dependencies or the MSRV, run `ci.py` whole. A failure that also fails on BASE is not the branch's:
check with a worktree at BASE before reporting it.

## 3. what only reading finds

Read the branch commit by commit (`git log -p BASE..HEAD`), each against what it says it does,
and then the diff as a whole. For each finding, give the file and line and the evidence: the line
that breaks the rule and the rule it breaks. A suspicion you could not confirm is said to be one,
with what would settle it, and does not count towards the verdict.

**Tests**
- A change in behaviour comes with a test that fails without it. For each test in FOR THE REVIEW
  that pins a fix, check it with `scripts/mutate.sh` as CONTRIBUTING.md says: what counts is that
  nothing *else* caught the change, and that the test fails on its assertion rather than because
  it no longer builds.
- What the branch changes is caught by something. A test the branch adds is checked by hand, as
  above; the code it changes, by cargo-mutants on that code alone, when the branch changes
  behaviour and is small enough for it (`--list` first: each mutant is a few minutes of the
  workspace's suite):
  ```sh
  git diff BASE HEAD -- '*.rs' > $REPO/target/agents/branch.diff
  cargo mutants --list --in-diff $REPO/target/agents/branch.diff --workspace --all-features | wc -l
  ```
  and then the `repo-sweep` skill's mutants mode with `--in-diff` (its first step), whose notes on
  the mutants that are dangerous to run apply here too. A mutant that survives is a finding: a
  line the branch changed that no test would miss.
- An existing test extended where one was natural, rather than a second one beside it.
- A test that cannot fail, or checks less than its name says.

**Changelogs**
- One per crate a caller of it would notice, in the section it belongs to (`breaking` for anything
  that can break a build: a removed item, a changed signature or public field, a variant added to
  an enum that is not `#[non_exhaustive]`, a feature that no longer turns on what it did), in the
  style of the entries beside it.
- A crate that starts to need a newer version of another says so under `changed`.

**The code**
- Events emitted while still holding the lock that made the change; nothing a provider, tool,
  projector or counter calls from inside the kernel calls back into it; each lock read into its own
  local rather than two in one expression. CONTRIBUTING.md's pitfalls have the reasons.
- A `Kernel` held by something the same kernel holds is a cycle: it should be a `Weak`.
- Anything that decides whether a call runs is read against SECURITY.md.
- A new dependency: needed, declared in the workspace manifest if more than one crate uses it,
  with a comment if it is not obvious, and a minimum version the code really builds with.
- `#[non_exhaustive]` on a new public enum that may gain variants, and on a struct the workspace
  returns but nothing outside it builds.

**The prose** - documents, doc comments, `note:` paragraphs, help lines, error messages, commit
messages
- Plain words, short sentences, said directly. No sentences about the text itself, no story of how
  a bug was found, no number that is not a default or limit a reader will meet, no counting the
  repository, no copy of the program's output.
- A doc comment says what something is; a `note:` says why, what was rejected, or what it costs.
- One word per mechanism in anything the program says or accepts: truncate, elide, exclude,
  `supersede`. A synonym there is a bug.
- A document that names a file, test or item names one that exists (`scripts/references.sh`, run
  by `ci.py lint`), MAP.md follows a file added, moved or removed, and a POSTPONED.md entry the
  branch closes is gone (or cut to what is left).

**The commits**
- One change per commit, each building and passing on its own where that is cheap to check.
- The message says what was wrong, what was decided, what was checked, and what was left out.

## 4. report

Start with the branch: its name, its BASE, how many commits, and what it does, in a sentence.
Then a table of the findings - file and line, what, the evidence, severity - and the verdict:

- **Blocking**: unsafe to push as it is - a key, a conflict marker, a CI step that fails because of
  the branch, a break with no `breaking` entry, a behaviour change with no test that catches it,
  a test removed with nothing checking what it did.
- **Minor**: a missing changelog line or a sentence about why it is missing, a `note:` that tells
  a story, a MAP.md row, a commit subject's form - worth fixing before the push, harmless if not.
- **Clean**: nothing confirmed.

List under "not findings" anything the script raised that you cleared, with why, so that the
person does not check it again.

## gotchas

- The changelog check reads each commit's own files, so a fix whose entry went in a later commit of
  the same branch shows as LOOK AT on the first: clear it by finding the entry.
- `cargo semver-checks` sees the public API, not features or behaviour: a module moved behind a
  feature breaks a build without that feature and passes the check.
- The MAP.md check matches a file's name in its crate's section. A new file in a directory MAP.md
  describes as a whole (`tests/`, `web/`) is fine without a row.
