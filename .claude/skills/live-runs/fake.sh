#!/bin/bash
# One session per misbehaviour of the hostile endpoint: fake.sh [MODE...]; all of them by default.
# Starts fakeapi.py on PORT (18765) if nothing listens there. Each writes $LIVE/out/fake-MODE.*.
set -u
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
SKILL=$REPO/.claude/skills/live-runs
LIVE=${LIVE:-$REPO/target/agents/live}
PORT=${PORT:-18765}
curl -s -m 2 "http://127.0.0.1:$PORT/v1/models" > /dev/null \
  || { nohup python3 "$SKILL/fakeapi.py" "$PORT" > "$LIVE/out/fakeapi.log" 2>&1 & sleep 1; }
modes=("$@")
[ ${#modes[@]} -eq 0 ] && modes=($(python3 -c "import sys; sys.path.insert(0, '$SKILL'); import fakeapi; print(' '.join(fakeapi.MODES))"))
printf 'hello\nsecond message\n/budget\n' > "$LIVE/out/fake.feed"
# the runs and not the server, which a bare `wait` would wait on as well
runs=()
for m in "${modes[@]}"; do
  BASE=http://127.0.0.1:$PORT/v1 DEADLINE=60 nohup "$SKILL/run.sh" "fake-$m" "$LIVE/out/fake.feed" \
    -m "$m" --allow fs > /dev/null 2>&1 &
  runs+=($!)
done
wait "${runs[@]}"
