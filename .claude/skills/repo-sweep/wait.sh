#!/bin/bash
# Waits until every named sweep has written its `exit` line, then prints them.
#
#   wait.sh NAME...        (run it in the background; it polls once a minute)
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
for name in "$@"; do
    until grep -q '^exit' "$SWEEPS/$name.err" 2> /dev/null; do sleep 60; done
done
for name in "$@"; do echo "$name: $(grep '^exit' "$SWEEPS/$name.err")"; done
