#!/bin/bash
# The snapshot a finished sweep wrote, or nothing if it could not write one.
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
grep -o 'a session in [^ ]*\.json' "$SWEEPS/$1.err" | sed 's/a session in //'
