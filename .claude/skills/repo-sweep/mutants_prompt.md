<!-- The prompt a mutants verification agent gets; see SKILL.md, "mutants". Fill in REPO, BRANCH,
     WORK (where worktrees, targets, scratch and patches go: on a disk without a quota), SWEEPS,
     TASKS (the mutants_tasks.py output directory for this crate), BASE (HEAD when the sessions
     ran), NAME and SESSIONS. -->

You are verifying tests that a headless model session wrote to kill mutants `cargo mutants` found surviving in the Rust workspace at REPO (branch BRANCH), and turning the good ones into patches. Every mutant here survived the WHOLE workspace suite (all crates, all features), not only its own crate's tests.

Read REPO/AGENTS.md first, and the "check that tests catch something" part of CONTRIBUTING.md.

HARD RULES
- Keep scratch files of your own under WORK/scratch-NAME/. Other agents run beside you.
- Do NOT modify any tracked file in REPO itself and run no git command that changes its state (no checkout, stash, reset, commit there). Another process commits there.
- Work in your own worktree: `git -C REPO worktree add --detach WORK/wt-NAME HEAD`, and build ONLY with `CARGO_TARGET_DIR=WORK/target-NAME` (the shell has CARGO_TARGET_DIR set to the shared target; always override it). Remove both when you finish (`git -C REPO worktree remove --force .../wt-NAME`, `rm -rf .../target-NAME`). Never use `git stash`. Commit freely inside your own worktree (detached HEAD) - that is how `scripts/mutate.sh` gets its clean tree.
- Leave each test you keep in a patch under WORK/patches/NAME-<n>-<slug>.patch, made with `git diff <base> -- <files>` in your worktree, one patch per concern, each applying to the worktree's starting HEAD on its own. Test code only: nothing outside `tests/` or a `#[cfg(test)]` module.

WHAT YOU HAVE
For each session in your group (session name `k-<task>`):
- SWEEPS/mtr/k-<task>.diff: the test code it wrote, filtered to src/ and tests/ (a change under src/ is either a #[cfg(test)] module or something that must not be there). It applies to the base commit BASE; use `git apply --3way` if it does not apply cleanly.
- SWEEPS/mtr/k-<task>.err: its transcript. Look for lines with KILLED: and EQUIVALENT: and the final list.
- TASKS/<task>/: the mutants' diffs, already with real `--- a/<file>` / `+++ b/<file>` headers, applicable with `git apply` from the worktree root. The task text is <task>.txt beside that directory.
The session's claims are leads, not facts. It may have written a test that does not fail under the mutation, one that pins an implementation detail, one that duplicates an existing test, or called a mutant equivalent that is not. Sessions could not run the network or unix-socket tests.

FOR EACH MUTANT
1. Read the mutant's diff and the code. Could a caller of the crate's public API observe the change, or the crate's own code in a way a test can see? If not, it is EQUIVALENT: say why in one sentence, and write no test. A private threshold documented only on a private item is an implementation detail, not a promise, so don't pin it.
2. If it is observable, look for an existing test that should have caught it, and extend that where it's natural. Otherwise take the session's test, or write your own, in the house style: a doc comment saying what is checked; `note:` for why, plainly; the `nachalnik::test` fixtures rather than new mocks; no souvenir numbers; a test named for the behaviour, not for the mutant. Keep it small: a model will read these.
3. Measure it the house way, in your worktree with the test committed: `CARGO_TARGET_DIR=<your target> scripts/mutate.sh <the mutant's diff> '<regex naming the test>'`. It needs a clean tree and runs the whole workspace suite, which takes a few minutes. Keep the test only if the verdict is "nothing else caught it". If the workspace already caught it, the mutant was never a gap: drop the test and record which test caught it. If NOTHING CAUGHT IT, the test does not work: fix it or drop it. The unix-socket tests `a_command_can_connect_to_one_it_could_have_written`, `a_command_cannot_connect_to_a_unix_socket_it_could_not_write_to`, `a_refused_socket_is_the_confinement_where_the_kernel_says_it_is`, `items::a_restart_on_the_drawn_loop_lets_go_of_its_clients_too`, `program::the_program_serves_a_socket_and_a_second_one_drives_it` and `reading_a_path_is_not_connecting_to_a_socket_in_it` can fail for path-length reasons unrelated to the mutant. If they are the only "other" failures, run the suite unmutated to confirm they fail there too, and discount them.
4. `cargo fmt --all`, then `cargo clippy -p <crate> --all-features --all-targets -- -D warnings`. The crate's tests must pass with your patches applied.

REPORT: your final message, concise.
- A table of every mutant in your group, each marked one of:
  - KILLED (test name, patch file)
  - ALREADY CAUGHT (by which test)
  - EQUIVALENT (why)
  - DROPPED (why)
- Then each patch with a proposed commit message: `crate: what changed, in one lowercase line`, a blank line, prose saying what is now checked and why it matters, ending "Tests only." Add no changelog entries: tests take none.

YOUR NAME: NAME
YOUR SESSIONS: SESSIONS
- The sessions ran under a network gate that refuses loopback, so they could not run a test that serves anything on it (nachalnik-providers' mock servers, kamchatka's sockets). Their claims about those tests are unverified: run them.
- `a_gitignore_is_obeyed_outside_a_repository_too` also fails at long paths, like the unix-socket tests above.
- Keep at most two `scripts/mutate.sh` runs going at once: other agents share the machine.
- Write each mutant's verdict, with its one-line reason, to WORK/scratch-NAME/verdicts.md as soon as you reach it. If this session dies, that file is the only record.
