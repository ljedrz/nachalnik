#!/bin/bash
# One headless session: run.sh NAME FEED [kamchatka args...]
#
#   FIX=proj      the fixture its workspace is copied from (WS= names a workspace to use as it is)
#   LIMIT=N       KAMCHATKA_CONTEXT_LIMIT, to reach compaction and the wall within minutes
#   DEADLINE=N    --deadline, 1200 by default; a hard kill follows five minutes after it
#   SIG="TERM N"  send a signal once N tool calls have started
#   BASE=URL      KAMCHATKA_BASE_URL, for the fake endpoint or the logging proxy
#
# Writes $LIVE/out/NAME.{jsonl,err,code,stem,check}: the record stream, the prose, the exit
# status, the record pair the session left in its own TMPDIR, and `kamchatka --check` on it.
# A feed's @T@ is $LIVE, @WS@ the workspace, and @PASTE@ a generated 20,000-word paste.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
LIVE=${LIVE:-$REPO/target/agents/live}
BIN=${BIN:-$REPO/target/release/kamchatka}
source "$LIVE/key.env"
[ -n "${BASE:-}" ] && export KAMCHATKA_BASE_URL=$BASE
name=$1 feed=$(realpath "$2"); shift 2
out=$LIVE/out/$name rec=$LIVE/rec/$name
mkdir -p "$LIVE/out" "$LIVE/ws"; rm -rf "$rec" "$out".*; mkdir -p "$rec/kamchatka"
if [ -z "${WS:-}" ]; then
  WS=$LIVE/ws/$name; rm -rf "$WS"; cp -a "$LIVE/fix/${FIX:-proj}" "$WS"
fi
[ -n "${LIMIT:-}" ] && export KAMCHATKA_CONTEXT_LIMIT=$LIMIT
export TMPDIR=$rec
cd "$WS"
fed() {
  LIVE=$LIVE WS=$WS python3 - "$feed" <<'PY'
import os, sys
paste = ' '.join('kamchatka' if i % 97 == 0 else 'lorem' for i in range(20000))
for line in open(sys.argv[1]):
    sys.stdout.write(line.replace('@T@', os.environ['LIVE']).replace('@WS@', os.environ['WS'])
                     .replace('@PASTE@', paste))
PY
}
# process substitution rather than a pipe, so that $! is kamchatka and a signal reaches it
"$BIN" --headless --deadline "${DEADLINE:-1200}" "$@" < <(fed) > "$out.jsonl" 2> "$out.err" &
pid=$!
echo $pid > "$out.pid"
sig=${SIG:-} start=$(date +%s)
while kill -0 $pid 2>/dev/null; do
  if [ -n "$sig" ] && [ "$(grep -c '"tool.started"' "$out.jsonl")" -ge "${sig#* }" ]; then
    echo "run: sending SIG${sig% *} after ${sig#* } tool calls" >> "$out.err"; kill -"${sig% *}" $pid; sig=
  fi
  if [ $(( $(date +%s) - start )) -gt $(( ${DEADLINE:-1200} + 300 )) ]; then
    kill -KILL $pid; echo "run: hard kill" >> "$out.err"
  fi
  sleep 3
done
wait $pid; code=$?
echo "exit $code" >> "$out.err"
stem=$(ls -t "$rec"/kamchatka/*.jsonl 2>/dev/null | head -1)
if [ -n "$stem" ]; then
  echo "${stem%.jsonl}" > "$out.stem"
  "$BIN" --check "${stem%.jsonl}" > "$out.check" 2>&1; echo "check exit $?" >> "$out.check"
fi
# last, so that a waiter on it finds everything else written
echo $code > "$out.code"
