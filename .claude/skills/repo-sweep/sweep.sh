#!/bin/bash
# One headless kamchatka sweep over one scope.
#
#   sweep.sh KIND NAME SCOPEFILE      KIND is audit, quality, tests or docs
#
# Writes $SWEEPS/NAME.jsonl (the stream records) and $SWEEPS/NAME.err (the prose, ending in a line
# `exit N`). Needs configure.py to have written $SWEEPS/audit.json, quality.json and docs.json, the
# key in the environment (source $SWEEPS/key.env), and a release build of kamchatka. Model
# parameters go in $SWEEPS/params.txt, one `KEY JSON` per line, sent as `/params` first.
set -u
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
BIN=${CARGO_TARGET_DIR:-$REPO/target}/release/kamchatka
DEADLINE=${DEADLINE:-4500}
kind=$1 name=$2 scope=$3
case $kind in
    audit) config=$SWEEPS/audit.json; word=audit ;;
    quality|tests) config=$SWEEPS/quality.json; word=review ;;
    docs) config=$SWEEPS/docs.json; word=check ;;
    *) echo "KIND is audit, quality, tests or docs" >&2; exit 2 ;;
esac
cd "$REPO"
{
    if [ -f "$SWEEPS/params.txt" ]; then
        grep -v '^[[:space:]]*$' "$SWEEPS/params.txt" | sed 's|^|/params |'
    fi
    printf '%s' "$(cat "$scope")"
    echo ' As soon as you have verified a finding, state it in your reply text (not only in your reasoning) on a line starting with FINDING:, then carry on exploring.'
    echo "Continue the $word where you left off: verify anything still unchecked with the tools, and state each verified defect in your reply text on a line starting with FINDING:. If you are completely finished, write the FINDINGS section in prose."
    echo 'Continue once more: look at the parts of the scope you have not yet read, verify, and report new FINDING: lines in your reply text. If there is nothing more, reply DONE.'
    echo 'Now write the final FINDINGS section in prose, in your reply text, listing every verified defect from this session with severity, file:line, the defect, the failure scenario and the fix.'
} | timeout $((DEADLINE + 300)) "$BIN" --headless --config-file "$config" --deadline "$DEADLINE" \
    > "$SWEEPS/$name.jsonl" 2> "$SWEEPS/$name.err"
echo "exit $?" >> "$SWEEPS/$name.err"
