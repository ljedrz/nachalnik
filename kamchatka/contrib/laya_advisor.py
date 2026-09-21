#!/usr/bin/env python3
"""A local System One advisor for kamchatka, backed by laya.

    $ pip install laya
    $ export SYSTEM1_ADVISOR_COMMAND="$HOME/ai/venv/bin/python path/to/laya_advisor.py"
    $ kamchatka --advise --shell-advisor

note: this exists because laya ships no interface to point a base URL at - no HTTP server, no
CLI, no `python -m laya`. It is a library, so reaching it from another process means a process,
and this is the smallest one that will do.

note: one JSON object per line in, one per line out, which is the body kamchatka already builds
for the hosted engine - `{"model": ..., "state": ..., "questions": {...}}` in, `{"model": ...,
"answers": {...}}` out. Nothing here translates: laya's question dicts use the same three types
under the same names, and its answers carry the same `choice`/`score`/`noul` and `confidence`.
That is why the shim is this short, and why there is no second request shape to keep in step.

note: the model loads once, before the first line is read. A 421M-parameter checkpoint costs
seconds to load and milliseconds to run, so loading it per question would put that wait in front
of somebody deciding whether to press `y` - which is the whole thing this kind of model is for
not doing. `preload=True` is laya's own name for paying it up front.

note: it answers every line, including one it could not handle. A shim that stayed silent on a
bad question would leave the caller waiting out its timeout and then closing the pipe, so one
malformed request would cost the advisor for the rest of the session; an `answers` object with
nothing in it is read as nothing having been said about that call, which is the outcome the
caller already handles on every other failure.
"""

import json
import sys


def main() -> int:
    try:
        from laya import Router
    except ImportError:
        print(
            "laya is not installed in this interpreter: pip install laya",
            file=sys.stderr,
        )
        return 1

    router = Router(preload=True)
    # note: to stderr, which kamchatka inherits rather than pipes, so this lands on the terminal
    # the session was started from. It is the only sign the checkpoint has finished loading
    print("laya: ready", file=sys.stderr, flush=True)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            request = json.loads(line)
            result = router.predict(request["state"], request["questions"])
            answers = result.get("answers", result)
        except Exception as e:  # noqa: BLE001 - see the note on answering every line
            print(f"laya: {e}", file=sys.stderr, flush=True)
            answers = {}

        json.dump({"model": "laya", "answers": answers}, sys.stdout)
        sys.stdout.write("\n")
        sys.stdout.flush()

    return 0


if __name__ == "__main__":
    sys.exit(main())
