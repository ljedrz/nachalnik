#!/bin/bash
# One session carried across processes, then forked and reconciled: chain.sh
#
#   ca  fresh: a note, a pin, a checkpoint      cb  resumed, SIGTERM mid-turn
#   cc  resumed, SIGKILL mid-turn               cd  resumed past a killed log: undo, /load
#   cx, cy  two forks of ca                     cz  resumed from `reconcile cx cy`
#
# Every run's record is held to `--check` by run.sh; read the .check files and the resume lines.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
SKILL=$REPO/.claude/skills/live-runs
LIVE=${LIVE:-$REPO/target/agents/live}
K=${BIN:-$REPO/target/release/kamchatka}
S="$(cat "$SKILL/system.txt")" F=$SKILL/feeds A="--allow fs,context,exec:run"
export WS=$LIVE/ws/chain; rm -rf "$WS"; cp -a "$LIVE/fix/proj" "$WS"
o() { cat "$LIVE/out/$1.stem"; }
"$SKILL/run.sh" ca "$F/chain-a.txt" -s "$S" $A
SIG="TERM 3" "$SKILL/run.sh" cb "$F/chain-b.txt" $A -r "$(o ca).json"
SIG="KILL 3" "$SKILL/run.sh" cc "$F/chain-c.txt" $A -r "$(o cb).json"
"$SKILL/run.sh" cd "$F/chain-d.txt" $A -r "$(o cc).json"
"$SKILL/run.sh" cx "$F/chain-x.txt" $A -r "$(o ca).json"
"$SKILL/run.sh" cy "$F/chain-y.txt" $A -r "$(o ca).json"
rm -f "$LIVE/out/merged".*
"$K" reconcile "$(o cx).json" "$(o cy).json" -o "$LIVE/out/merged" > "$LIVE/out/reconcile.out" 2>&1
echo "reconcile exit $?" >> "$LIVE/out/reconcile.out"
"$K" --check "$LIVE/out/merged" >> "$LIVE/out/reconcile.out" 2>&1
"$SKILL/run.sh" cz "$F/chain-z.txt" $A -r "$LIVE/out/merged.json"
