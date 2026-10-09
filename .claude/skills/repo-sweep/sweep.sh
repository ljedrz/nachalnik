#!/bin/bash
# One headless kamchatka sweep over one scope. `run.py` starts these; run one by hand only to
# try a scope out.
#
#   sweep.sh KIND NAME SCOPEFILE      KIND is audit, quality, tests, docs, compact or maintain
#
# Writes, under $SWEEPS:
#   NAME.in     the lines the session is given: `/params` commands, the scope, three continuations
#   NAME.pid    the session's process id, while it runs
#   NAME.jsonl  the stream records
#   NAME.err    the prose, ending in a line `exit N`
#
# No time limit unless DEADLINE (seconds) is set: a sweep is over when the model has answered the
# last line, however long that takes. `run.py` watches for one that has stopped making progress.
# Needs configure.py to have written $SWEEPS/{audit,quality,docs,compact,maintain}.json, the key
# in the environment (source $SWEEPS/key.env) and a release build of kamchatka. Model parameters go
# in $SWEEPS/params.txt, one `KEY JSON` per line. FLAGS is passed to kamchatka as it is:
# `FLAGS=--anthropic` for a Claude model, whose API caches nothing unless asked.
set -u
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
# what the session reads, and what it runs: `run.py` pins both for the length of a run, so that
# work in the repository meanwhile - a fix being committed, a file reverted for a mutation check,
# a rebuild - is not what a sweep half-way through reads or runs
TREE=${TREE:-$REPO}
BIN=${BIN:-${CARGO_TARGET_DIR:-$REPO/target}/release/kamchatka}
DEADLINE=${DEADLINE:-}
FLAGS=${FLAGS:-}
kind=$1 name=$2 scope=$3
case $kind in
    audit) config=$SWEEPS/audit.json; word=audit ;;
    quality|tests) config=$SWEEPS/quality.json; word=review ;;
    docs) config=$SWEEPS/docs.json; word=check ;;
    compact) config=$SWEEPS/compact.json; word=review ;;
    maintain) config=$SWEEPS/maintain.json; word=review ;;
    *) echo "KIND is audit, quality, tests, docs, compact or maintain" >&2; exit 2 ;;
esac
# the window configure.py worked from, so that kamchatka compacts against the same figure rather
# than what the endpoint lists
if [ -f "$SWEEPS/limit" ]; then
    export KAMCHATKA_CONTEXT_LIMIT=$(cat "$SWEEPS/limit")
fi
cd "$TREE"
{
    if [ -f "$SWEEPS/params.txt" ]; then
        grep -v '^[[:space:]]*$' "$SWEEPS/params.txt" | sed 's|^|/params |'
    fi
    # the scope is one message, however many lines its file has
    printf '%s' "$(tr '\n' ' ' < "$scope")"
    echo ' As soon as you have verified a finding, state it in your reply text (not only in your reasoning) on a line starting with FINDING:, then carry on exploring.'
    echo "Continue the $word where you left off: verify anything still unchecked with the tools, and state each verified defect in your reply text on a line starting with FINDING:. If you are completely finished, write the FINDINGS section in prose."
    echo 'Continue once more: look at the parts of the scope you have not yet read, verify, and report new FINDING: lines in your reply text. If there is nothing more, reply DONE.'
    echo 'Now write the final FINDINGS section in prose, in your reply text, listing every verified defect from this session with severity, file:line, the defect, the failure scenario and the fix.'
} > "$SWEEPS/$name.in"

limit=()
[ -n "$DEADLINE" ] && limit=(--deadline "$DEADLINE")
"$BIN" $FLAGS --headless --config-file "$config" "${limit[@]}" \
    < "$SWEEPS/$name.in" > "$SWEEPS/$name.jsonl" 2> "$SWEEPS/$name.err" &
echo $! > "$SWEEPS/$name.pid"
wait $!
echo "exit $?" >> "$SWEEPS/$name.err"
rm -f "$SWEEPS/$name.pid"
