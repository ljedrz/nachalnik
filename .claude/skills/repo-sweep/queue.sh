#!/bin/bash
# Runs kill.sh over task files, at most N at a time: a session builds in a worktree of its own,
# and a per-session disk quota is the limit that has been hit, not the rate.
#
#   queue.sh N TASKFILE...
here=$(cd "$(dirname "$0")" && pwd)
n=$1; shift
for t in "$@"; do
    while [ "$(jobs -rp | wc -l)" -ge "$n" ]; do sleep 10; done
    "$here/kill.sh" "k-$(basename "$t" .txt)" "$t" "${t%.txt}" &
done
wait
