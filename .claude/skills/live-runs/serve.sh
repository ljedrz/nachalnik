#!/bin/bash
# A served session driven by --connect clients: serve.sh ADDRESS FEED [server args...]
#
#   client a pipes FEED and closes its input, leaving any question for another client
#   client b connects with --on-ask deny and nothing to say, answering what was left
#   client c sends /quit
#
# Writes $LIVE/out/serve-<address-slug>/{server,a,b}.{jsonl,err} and a log of exit statuses and how
# long client a took to leave - which, with a turn stopped on a question, is seconds and not ever.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
LIVE=${LIVE:-$REPO/target/agents/live}
K=${BIN:-$REPO/target/release/kamchatka}
source "$LIVE/key.env"
at=$1 feed=$(realpath "$2"); shift 2
O=$LIVE/out/serve-$(echo "$at" | tr -c 'a-zA-Z0-9\n' '-'); rm -rf "$O"; mkdir -p "$O/rec/kamchatka"
WS=$LIVE/ws/$(basename "$O"); rm -rf "$WS"; cp -a "$LIVE/fix/${FIX:-proj}" "$WS"; cd "$WS"
export TMPDIR=$O/rec
# no --headless and no --deadline: a served session with no terminal is headless already, and
# refuses both
"$K" --serve "$at" "$@" < <(sleep 900) > "$O/server.jsonl" 2> "$O/server.err" &
spid=$!
case "$at" in
  unix:*) for _ in $(seq 50); do [ -S "${at#unix:}" ] && break; sleep 0.2; done ;;
  *) sleep 2 ;;
esac
start=$(date +%s)
timeout 300 "$K" --connect "$at" < "$feed" > "$O/a.jsonl" 2> "$O/a.err"
echo "a exit $? after $(( $(date +%s) - start ))s" >> "$O/log"
printf '\n' | timeout 300 "$K" --connect "$at" --on-ask deny > "$O/b.jsonl" 2> "$O/b.err"
echo "b exit $?" >> "$O/log"
printf '/quit\n' | timeout 60 "$K" --connect "$at" > /dev/null 2>&1
wait $spid; echo "server exit $?" >> "$O/log"
cat "$O/log"
