#!/bin/bash
# One cargo-mutants run over the workspace, with the process count capped and the traps in
# SKILL.md ("mutants") avoided: no shared target, copies on a disk with room, the long-path tests
# skipped and the suite run to its end.
#
#   mutants.sh OUT CARGO_MUTANTS_ARGS...      e.g.
#   mutants.sh nachalnik-ws --workspace --file 'nachalnik/src/**/*.rs' --all-features \
#       --test-workspace=true --iterate --timeout 300 -j 4
#
# Writes $MUTANTS/out/OUT/mutants.out and $MUTANTS/out/OUT.log. MUTANTS defaults to the
# repository's target/agents, which is ignored by git and on disk rather than on a tmpfs.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
MUTANTS=${MUTANTS:-$REPO/target/agents}
out=$1; shift
mkdir -p "$MUTANTS/mt" "$MUTANTS/out"
# at a path this long, these fail unmutated: a unix socket's path is over the limit, and a
# `.gitignore` above the copy is read
skips=(a_command_can_connect_to_one_it_could_have_written
    a_command_cannot_connect_to_a_unix_socket_it_could_not_write_to
    a_refused_socket_is_the_confinement_where_the_kernel_says_it_is
    a_restart_on_the_drawn_loop_lets_go_of_its_clients_too
    the_program_serves_a_socket_and_a_second_one_drives_it
    reading_a_path_is_not_connecting_to_a_socket_in_it
    a_gitignore_is_obeyed_outside_a_repository_too)
skip_args=()
for s in "${skips[@]}"; do skip_args+=(--skip "$s"); done
# a mutant can spawn without end, and when the user's process limit is reached every process of
# theirs fails to fork, including the session that started this. The cap binds this subtree only
# once the user's total reaches it, so whoever runs this can still fork
cap=$(($(ps -u "$(id -u)" -L --no-headers | wc -l) + 20000))
cd "$REPO"
(
    ulimit -u "$cap"
    exec env -u CARGO_TARGET_DIR TMPDIR="$MUTANTS/mt" cargo mutants "$@" \
        --output "$MUTANTS/out/$out" -- --no-fail-fast -- "${skip_args[@]}"
) > "$MUTANTS/out/$out.log" 2>&1
python3 "$(dirname "$0")/tainted.py" "$MUTANTS/out/$out" > /dev/null
