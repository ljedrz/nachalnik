---
name: release
description: Prepare a release of this workspace's crates - decide which crates move and by how much from `cargo semver-checks` and the changelogs, run the whole pre-release check, make the bump commits in dependency order and the release commit that only dates the changelogs, and the annotated tags - stopping before anything leaves the machine. Use when asked to "release", "cut a release", "prepare a release", "bump the versions", or "what would a release need".
---

# a release

CONTRIBUTING.md's conventions on versions and releases are the rules; this is their order, and the
checks that go with them. The last release commit (`git log --grep '^release:' -1`) and the bump
commits before it are the model for every message below: read them first.

## rules that do not bend

- **Nothing leaves the machine.** No push, no `cargo publish`, no tag pushed: the person does those,
  from the commands this ends with. Tags are made locally and only when the person says so.
- **A version follows the public API**, not the size of the work. In `0.x.y`, anything that can
  break a caller's build moves `x`; everything else, new API included, moves `y`.
- **Bumps are commits of their own**, one per crate, before the release commit; **the release
  commit only dates the changelogs** - no code, no number - and is the one that is tagged.
- **Everything is checked before the first commit**, not after: a failure found between a bump and
  the release means undoing commits.

## 1. what there is to release

```sh
SKILL=<repo>/.claude/skills/release
python3 $SKILL/plan.py
```

It lists each published crate in dependency order: its version and last tag, what its changelog's
unreleased section holds, the commits since the tag that touch it, and which crates depend on it.
Settle everything under "look at" first:

- **Commits with nothing unreleased.** Read them. Tests, docs and tooling take no entry; a change
  a caller can see does, written now in the changelog's style, in its own commit.
- **A heading spelt another way.** The release commit replaces `## [unreleased]` exactly; fix the
  heading in its own commit.
- **A version already bumped** past its last tag (made early, because a crate above needed new
  API before the release): it stays, and the release dates its section.

A crate with nothing unreleased and no commits is not released.

## 2. decide the numbers

For each crate to release, in the order `plan.py` gives:

```sh
cargo semver-checks -p <crate> --all-features --baseline-rev <crate>-v<last version>
```

- A break it reports moves `x`. So does anything in the changelog's `breaking` section, and what it
  cannot see: a removed or renamed feature, a feature that no longer turns on what it did, a raised
  MSRV, a change in behaviour a caller relies on. If both are silent and the API changed, compare
  the `cargo doc` HTML of the last tag and this tree page by page: two methods with the same
  signature on different types cancel each other out in the tool.
- **A crate whose dependency moves `x` moves too** where it names that dependency's types in its
  own API - the providers, the bridge, the eval and `kamchatka` all name the runtime's - and its
  requirement on it rises. A `y` move of a dependency asks nothing of the crates above it unless
  they now need its new API.
- **A minor move of `nachalnik` raises the minimum `nachalnik-mcp` requires**, so the bridge is
  released again even with nothing of its own.
- `kamchatka` is a program and a library: its library's API decides its number like any other's.

Write the decision down per crate - old number, new number, the one-line reason - before going on;
it is the table in the release commit.

## 3. check everything, before any commit

On the tree as it will be released:

```sh
python3 $SKILL/ci.py --keep-going     # every step of ci.yml, read from it
```

It runs each `run:` step of `.github/workflows/ci.yml` with the environment CI gives it and the
toolchain its job pins (`cargo +1.95.0` for the MSRV and minimal-versions jobs), and `cargo deny`
for the job that is only an action. Being read from the workflow, it cannot fall behind it. The
Linux runners' steps are the ones run; macOS and Windows are left to CI and `scripts/windows.sh`.
`--list` shows the steps; naming a job (`ci.py lint`) runs only that one, which is what to do
after a fix, before running it all again.

- **`cargo package` again after the bumps** (`ci.py package`), since the versions it packages are
  the new ones only then. `ci.py` gives it a fresh target directory each time: a second run on an
  unpublished version compiles the first run's tarball (CONTRIBUTING.md's pitfalls).
- **`zizmor .github`** if a workflow changed since the last release.
- **The live suites**, with the `live-tests` skill, on cheap models. A failure that is the model's,
  shown so as that skill says, does not hold a release; one that is the code's does. Say which
  suites could not run for want of a key.
- `scripts/windows.sh` needs `cargo-xwin` and Microsoft's licence accepted: the person's call.

Anything that fails is fixed in its own commit, before the bumps, and this step starts over.

## 4. the bumps

One commit per crate, in `plan.py`'s order, titled `<crate>: <version>`:

- the crate's `version` in its `Cargo.toml`, its requirement in the workspace `Cargo.toml` where it
  is there, and `Cargo.lock` (`cargo check --workspace` updates it);
- in a crate whose requirement on another rose, a bullet under its unreleased `changed`: what it
  requires now, and the new API of that crate it uses, named;
- the message says why this number: what `cargo semver-checks` found against which tag, and what in
  the changelog it could not see. Read the last release's bump commits for the shape.

## 5. the release commit

It replaces each released crate's `## [unreleased]` with `## [<version>] - <today>`, and nothing
else. `kamchatka`'s release workflow uses that section as the release's description and fails
without one.

The message, as the last release's: `release: <crate> <version>, ...` in dependency order; a table
of each crate's old and new number and what it brings, in a line or three; why each number moved;
the requirements between the crates; the tags to make; what step 3 ran, and what it did not run and
why.

## 6. hand over

Stop here and tell the person:

1. **Run the release workflow by hand first** (`workflow_dispatch` on `release.yml`, at the release
   commit): it builds and packages everything a tag would and uploads nothing. A tag that fails the
   workflow is a release that half-happened.
2. **The tags**, annotated, on the release commit, to make once that passes - one per released
   crate, and the workspace's only when `nachalnik` itself is released:
   ```sh
   git tag -a <crate>-v<version> -m "<crate> <version>"
   git tag -a v<nachalnik's version> -m "the workspace at nachalnik <version>"
   ```
   A `kamchatka-v*` tag is what builds and publishes the binaries.
3. **Publishing**, in dependency order, each crate only once the one below it is on the registry:
   `nachalnik`, then `nachalnik-providers` and `nachalnik-mcp`, then `nachalnik-eval` and
   `kamchatka`. `nachalnik-utils` is never published.

## gotchas

- `cargo semver-checks` with `--workspace` takes one baseline for every crate, and the crates'
  last tags differ: check each with its own `--baseline-rev`.
- A feature moved behind another (a module now needing `tui` or `webui`) breaks a caller building
  without it, and `cargo semver-checks` does not see features: it is the changelog's `breaking`.
- An enum variant inserted among others moves the implicit discriminant of every one after it,
  which `cargo semver-checks` reports. It matters only if something reads the number; say which in
  the bump's message rather than taking the report on trust.
- A `-D warnings` build can fail on a newer toolchain than CI's. Check what CI pins before fixing a
  lint the release did not introduce.
