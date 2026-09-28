#!/bin/bash
# One segment of a soak: a headless kamchatka session fed FEED, fresh or resumed from the snapshot
# segment FROM left, and optionally sent SIGNAL once N tool calls have started in it.
#
#   segment.sh NAME FEED [FROM|-] [SIGNAL N]
#
# Writes $SOAK/segments/NAME.{jsonl,err} (stdout and stderr) and NAME.stem, the record pair the
# session left under $SOAK/rec/kamchatka - which is what check.py reads, since a killed process's
# stdout can stop short of its record. Needs setup.sh to have run and the key in the environment.
set -u
SOAK=${SOAK:?set SOAK}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
BIN=${CARGO_TARGET_DIR:-$REPO/target}/release/kamchatka
DEADLINE=${DEADLINE:-3600}
name=$1 feed=$(realpath "$2") from=${3:--} signal=${4:-} after=${5:-0}
out=$SOAK/segments/$name

resume=()
if [ "$from" != - ]; then
    resume=(-r "$(cat "$SOAK/segments/$from.stem").json")
fi

# the records land in the session's own temporary directory; this one is on disk and the soak's
export TMPDIR=$SOAK/rec
export CARGO_TARGET_DIR=$SOAK/wt/target
export KAMCHATKA_CONTEXT_LIMIT=${LIMIT:-40000}
mkdir -p "$TMPDIR/kamchatka"
before=$(ls "$TMPDIR"/kamchatka/*.jsonl 2>/dev/null | sort)

[ -r "$feed" ] || { echo "no feed at $2" >&2; exit 2; }
cd "$SOAK/wt"
# process substitution rather than a pipe, so that $! is kamchatka and a signal reaches it
"$BIN" --headless --config-file "$SOAK/soak.json" --deadline "$DEADLINE" "${resume[@]}" \
    < <(sed "s|@SOAK@|$SOAK|g" "$feed") > "$out.jsonl" 2> "$out.err" &
pid=$!
started=$(date +%s)
while kill -0 $pid 2>/dev/null; do
    if [ -n "$signal" ] && [ "$(grep -c '"tool.started"' "$out.jsonl")" -ge "$after" ]; then
        echo "soak: sending SIG$signal after $after tool calls" >> "$out.err"
        kill -"$signal" $pid
        signal=
    fi
    # --deadline stops a session that is still answering; this is for one that is not
    if [ $(( $(date +%s) - started )) -gt $((DEADLINE + 600)) ]; then
        kill -KILL $pid
    fi
    sleep 5
done
wait $pid
echo "exit $?" >> "$out.err"

after_run=$(ls "$TMPDIR"/kamchatka/*.jsonl 2>/dev/null | sort)
new=$(comm -13 <(echo "$before") <(echo "$after_run") | head -1)
echo "${new%.jsonl}" > "$out.stem"
echo "$name: $(cat "$out.stem")"
