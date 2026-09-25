#!/bin/bash
# The snapshot a finished sweep wrote, or nothing if it could not write one: the last line naming
# one, since a model that quotes the line in its own reply puts another before it.
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
grep -o 'a session in [^ ]*\.json' "$SWEEPS/$1.err" | tail -n 1 | sed 's/a session in //'
