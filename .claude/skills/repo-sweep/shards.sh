#!/bin/bash
# One cargo-mutants run as N in-place shards, each in a git worktree of its own at a short path.
#
#   shards.sh OUT N CARGO_MUTANTS_ARGS...      e.g.
#   shards.sh nachalnik 4 --workspace --file 'nachalnik/src/**/*.rs' --all-features \
#       --test-workspace=true --iterate --timeout 300
#
# Writes $MUTANTS/out/OUT-1 .. OUT-N (each with its mutants.out) and $MUTANTS/out/OUT-i.log, and
# the guard's log as $MUTANTS/out/OUT.guard. With --iterate, OUT-i is iterated on if it exists,
# and otherwise seeded from $MUTANTS/out/$SEED (a run's output to carry on from) when SEED is set.
#
# Why not mutants.sh's copies: cargo-mutants copies the tree to TMPDIR/cargo-mutants-PKG-XXXXXX.tmp,
# and kamchatka's tests serve on unix sockets under the target directory, whose path has a hard
# limit of 107 bytes. No copy anywhere in this repository is short enough, so those tests fail
# unmutated and every mutant looks caught. A worktree at $REPO/wI is: the socket prefix is under
# fifty bytes, the same as the repository's own.
#
# A worktree an earlier run was killed in may hold a mutation still; each is reset before use.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
MUTANTS=${MUTANTS:-$REPO/target/agents}
out=$1 n=$2; shift 2
mkdir -p "$MUTANTS/mt" "$MUTANTS/out"
grep -qx '/w[0-9]*/' "$REPO/.git/info/exclude" 2> /dev/null || echo '/w[0-9]*/' >> "$REPO/.git/info/exclude"
# see mutants.sh: the cap binds this subtree only once the user's total reaches it
cap=$(($(ps -u "$(id -u)" -L --no-headers | wc -l) + 20000))
# these two fail unmutated in a worktree here: the first wants a pty this machine will not
# allocate, the second reads the repository's `.gitignore` above its scratch directory
skips=(a_restart_on_the_drawn_loop_lets_go_of_its_clients_too
    a_gitignore_is_obeyed_outside_a_repository_too)
skip_args=()
for s in "${skips[@]}"; do skip_args+=(--skip "$s"); done
pids=()
for i in $(seq 1 "$n"); do
    wt=$REPO/w$i
    [ -e "$wt" ] || git -C "$REPO" worktree add --detach "$wt" HEAD > /dev/null || exit 1
    git -C "$wt" checkout --detach -q -f "$(git -C "$REPO" rev-parse HEAD)"
    git -C "$wt" reset -q --hard
    o=$MUTANTS/out/$out-$i
    if [ ! -d "$o" ] && [ -n "${SEED:-}" ]; then cp -r "$MUTANTS/out/$SEED" "$o"; fi
    (
        cd "$wt"
        ulimit -u "$cap"
        exec env -u CARGO_TARGET_DIR TMPDIR="$MUTANTS/mt" RUST_TEST_THREADS=${RUST_TEST_THREADS:-8} \
            cargo mutants --in-place --shard "$((i - 1))/$n" "$@" --output "$o" -- --no-fail-fast -- "${skip_args[@]}"
    ) > "$MUTANTS/out/$out-$i.log" 2>&1 &
    pids+=($!)
done
# see mutants.sh and guard.py
python3 "$(dirname "$0")/guard.py" "$(IFS=,; echo "${pids[*]}")" "$REPO/w" "$MUTANTS/out/$out.guard" \
    --min-avail-gib "${MIN_AVAIL_GIB:-10}" --max-procs "${MAX_PROCS:-4000}" \
    --max-rss-gib "${MAX_RSS_GIB:-8}" &
guard=$!
for p in "${pids[@]}"; do wait "$p"; done
wait "$guard" || echo "guard.py killed the run: see $MUTANTS/out/$out.guard" >&2
for i in $(seq 1 "$n"); do
    git -C "$REPO/w$i" reset -q --hard
    echo "shard $i: $(python3 "$(dirname "$0")/tainted.py" "$MUTANTS/out/$out-$i" 2>&1 | tail -1)"
done
