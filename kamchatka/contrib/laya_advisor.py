#!/usr/bin/env python3
"""A local System One advisor for kamchatka, backed by laya.

    $ pip install laya
    $ export SYSTEM1_ADVISOR_COMMAND="$HOME/ai/venv/bin/python path/to/laya_advisor.py"
    $ kamchatka --advise

    $ python3 laya_advisor.py --probe "ls -la"      # what laya actually answers, verbatim

note: this exists because laya ships no interface to point a base URL at - no HTTP server, no
CLI, no `python -m laya`. It is a library, so reaching it from another process means a process,
and this is the smallest one that will do.

note: one JSON object per line in, one per line out, in the body kamchatka already builds for the
hosted engine - `{"model": ..., "state": ..., "questions": {...}}` in, `{"model": ...,
"answers": {...}}` out.

note: **the answer is built here rather than passed through, and that is the whole job.** The two
engines agree on the question shape - the same three types, the same names, the same `criteria` -
and they do not agree on the answer. The documented shape carries a `type` on every answer and a
`confidence` meaning *how concentrated the distribution is*; laya's answers are keyed by the
primitive, and its `confidence` is its own quantity.

Passing one through as the other is how `ls` arrives at 1% and is drawn as a command that changes
something: kamchatka will not draw an unsure reading green, so a number that is not a confidence
turns every answer yellow whatever it scored. That rule is right and the input was wrong.

So this reads laya's distribution and computes the field the caller means by the word, and stamps
the `type` from the question that was asked - which is the authoritative source for it, since the
shim is what sent it. Where there is no distribution to work from it falls back to whatever laya
called confidence, which is the case `--probe` exists to make visible.

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

# The keys laya might carry each of these under, most likely first.
#
# note: several rather than one, because this is written against laya's README rather than a
# specification: it shows `{"choice": ..., "confidence": ...}` and does not say what an ordinal
# answer's fields are called. Trying the plausible names and saying nothing when none is there is
# honest about that. `--probe` is how to find out which it really uses and cut these to the one.
SCORES = ("score", "expected", "expected_level", "value")
SPREADS = ("probabilities", "distribution", "probs", "scores")
NOULS = ("noul", "probability", "p", "true")


def pick(answer, names, fallback=None):
    """The first of `names` this answer carries, or `fallback`."""
    for name in names:
        if isinstance(answer, dict) and name in answer:
            return answer[name]
    return fallback


def spread(answer):
    """The distribution as a list of probabilities, or nothing where there is none."""
    found = pick(answer, SPREADS)
    if isinstance(found, dict):
        return [v for v in found.values() if isinstance(v, (int, float))]
    if isinstance(found, (list, tuple)):
        return [v for v in found if isinstance(v, (int, float))]
    return []


def confidence(answer):
    """How concentrated the distribution is, which is what the caller means by the word.

    note: the largest probability. What a caller comparing this against a threshold is asking is
    whether the model settled on one answer or spread itself over several, and that is the
    quantity that says so. Where laya reports no distribution this falls back to its own
    `confidence`; where it reports neither, the answer goes out at 0.0, which the caller reads as
    "could not tell" rather than as a clean bill.
    """
    probabilities = spread(answer)
    if probabilities:
        return float(max(probabilities))

    own = pick(answer, ("confidence",), 0.0)
    return float(own) if isinstance(own, (int, float)) else 0.0


def answered(kind, answer):
    """One of laya's answers, in the shape the documented API answers in."""
    if not isinstance(answer, dict):
        return None

    if kind == "noul":
        # note: a noul's number *is* its confidence, so there is no second field to compute
        value = pick(answer, NOULS)
        if not isinstance(value, (int, float)):
            value = confidence(answer)
        return {"type": "noul", "noul": float(value)}

    if kind == "choice":
        chosen = pick(answer, ("choice", "label", "answer"))
        if not isinstance(chosen, str):
            return None
        return {
            "type": "choice",
            "choice": chosen,
            "confidence": confidence(answer),
            "probabilities": pick(answer, SPREADS, {}),
        }

    if kind == "score":
        value = pick(answer, SCORES)
        if not isinstance(value, (int, float)):
            return None
        return {
            "type": "score",
            "score": float(value),
            "confidence": confidence(answer),
            "legend": pick(answer, ("legend",), {}),
            "probabilities": pick(answer, SPREADS, {}),
        }

    return None


def translated(asked, result):
    """Everything laya answered, by the name and type each question was asked under."""
    raw = result.get("answers", result) if isinstance(result, dict) else {}
    out = {}
    for name, question in asked.items():
        answer = answered(question.get("type"), raw.get(name))
        if answer is not None:
            out[name] = answer

    return out


def probe(command):
    """Prints what laya answers about one command, verbatim and translated.

    note: the reason this mode exists is that the translation above is written against a README.
    Run it once against the engine actually installed and the guessing stops: whichever keys are
    really there are in the first block, and whether this shim reads them is the second.
    """
    from laya import Router

    asked = {
        "rating": {
            "type": "score",
            "instructions": "Place this command by what it would do.",
            "criteria": [
                "it only looks, or moves about, and leaves nothing changed",
                "it leaves something changed that could be put back",
                "it destroys something, or sends something off this machine",
            ],
        },
        "irreversible": {
            "type": "noul",
            "instructions": "Would running this destroy something that cannot be got back?",
        },
    }

    result = Router(preload=True).predict({"cmd": command}, asked)
    print("--- what laya answered, verbatim ---")
    print(json.dumps(result, indent=2, default=str))
    print("--- what this shim would send on ---")
    print(json.dumps(translated(asked, result), indent=2))

    return 0


def selftest() -> int:
    """Checks the translation against a recorded laya-shaped answer, with no laya installed.

    note: the case that sent this file back for a second try. `ls` scores near zero - correctly,
    it only looks - and laya reports a `confidence` of about the same, which is not what the
    caller means by the word. Passed through, that is a reading nobody is sure of, and kamchatka
    will not draw one of those green; every command came out yellow whatever it scored. Read off
    the distribution instead it is 0.97, and `ls` is green.

    note: runnable without the package, which is the point - it is the half of this shim that can
    be wrong on a machine that cannot load a checkpoint, and `cargo test` runs it.
    """
    asked = {
        "rating": {"type": "score", "instructions": "x", "criteria": ["a", "b", "c"]},
        "verdict": {"type": "choice", "instructions": "x", "criteria": {"allow": None}},
        "irreversible": {"type": "noul", "instructions": "x"},
    }
    recorded = {
        "answers": {
            "rating": {
                "score": 0.01,
                "confidence": 0.01,
                "probabilities": {"0": 0.97, "1": 0.02, "2": 0.01},
            },
            "verdict": {
                "choice": "allow",
                "confidence": 0.94,
                "probabilities": {"allow": 0.94, "deny": 0.06},
            },
            "irreversible": {"noul": 0.02},
        }
    }

    out = translated(asked, recorded)

    # every answer carries the type the *question* had, which laya does not send
    assert out["rating"]["type"] == "score", out
    assert out["verdict"]["type"] == "choice", out
    assert out["irreversible"]["type"] == "noul", out

    # the score is laya's own and the confidence is not: 0.01 is where the command landed, and
    # 0.97 is how sure it was of landing there
    assert out["rating"]["score"] == 0.01, out
    assert out["rating"]["confidence"] == 0.97, out
    assert out["verdict"]["confidence"] == 0.94, out

    # a noul's number is its confidence, so there is no second field to invent
    assert out["irreversible"]["noul"] == 0.02, out
    assert "confidence" not in out["irreversible"], out

    # an answer this cannot read is left out rather than guessed at, which the caller reads as
    # nothing having been said about that question
    assert translated(asked, {"answers": {"rating": {"nope": 1}}}) == {}, "a shape it cannot read"
    assert translated(asked, {}) == {}, "no answers at all"

    print("laya_advisor: the translation holds")
    return 0


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        return selftest()
    if len(sys.argv) > 1 and sys.argv[1] == "--probe":
        return probe(sys.argv[2] if len(sys.argv) > 2 else "ls -la")

    try:
        from laya import Router
    except ImportError:
        print(
            "laya is not installed in this interpreter: pip install laya",
            file=sys.stderr,
        )
        return 1

    router = Router(preload=True)
    # note: to stderr, which kamchatka holds rather than inheriting - it would otherwise be
    # written over the screen - and reports as it arrives. It is the line that says the
    # checkpoint has finished loading, which is the one thing somebody waiting wants
    print("ready", file=sys.stderr, flush=True)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            request = json.loads(line)
            asked = request["questions"]
            answers = translated(asked, router.predict(request["state"], asked))
        except Exception as e:  # noqa: BLE001 - see the note on answering every line
            print(f"{type(e).__name__}: {e}", file=sys.stderr, flush=True)
            answers = {}

        json.dump({"model": "laya", "answers": answers}, sys.stdout)
        print()
        sys.stdout.flush()

    return 0


if __name__ == "__main__":
    sys.exit(main())
