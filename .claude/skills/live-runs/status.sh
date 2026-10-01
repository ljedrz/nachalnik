#!/bin/bash
# One line per run: lines of prose, tool calls started, calls whose arguments did not parse, exit.
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
LIVE=${LIVE:-$REPO/target/agents/live}
cd "$LIVE/out" || exit 1
for f in "${@:-*}"; do
  for err in $f.err; do
    n=${err%.err}
    [ -f "$n.jsonl" ] || continue
    printf '%-22s lines=%-5s calls=%-4s unparsed=%-3s code=%s\n' "$n" "$(wc -l < "$err")" \
      "$(grep -c '"tool.started"' "$n.jsonl")" "$(grep -c '_unparsed' "$err")" "$(cat "$n.code" 2>/dev/null)"
  done
done
