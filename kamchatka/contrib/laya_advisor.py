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

# The rubric kamchatka actually sends, copied from `LEVELS` in `kamchatka/src/tools/advice.rs`.
#
# note: a copy, and the hazard is obvious - two texts that have to agree, in two languages. What
# stops them drifting is a test: `the_probe_asks_the_question_the_program_asks` in `advisor.rs`
# reads this file and fails if a level the program sends is not in it.
#
# note: worth the copy rather than probing with something illustrative, and that is the lesson
# this constant exists because of. The first `--probe` made up a short rubric of its own,
# reported `ls` at 0.84, and the running session reported 0.39 for the same command - because
# the two were asking different questions. A probe that does not ask what the program asks
# measures something nobody runs, and reads as evidence while doing it.
LEVELS = [
    "it only looks, or moves about, and leaves nothing changed",
    "it leaves something changed that could be put back",
    "it destroys something that cannot be got back, or sends something off this machine",
]


def number(value, fallback=0.0):
    """`value` as a float where it is one, and `fallback` where it is not."""
    return float(value) if isinstance(value, (int, float)) else fallback


def spread(answer):
    """The distribution as a list of probabilities, or nothing where there is none."""
    found = answer.get("probabilities")
    if isinstance(found, dict):
        return [v for v in found.values() if isinstance(v, (int, float))]
    if isinstance(found, (list, tuple)):
        return [v for v in found if isinstance(v, (int, float))]
    return []


def confidence(answer):
    """How concentrated the distribution is, which is what the caller means by the word.

    note: the largest probability, and not laya's own `confidence`, which is a different
    quantity. Measured: `ls` comes back `{"0": 0.84, "1": 0.08, "2": 0.08}` with a `confidence`
    of 0.49 beside it. What a caller comparing against a threshold is asking is whether the
    model settled on one level or spread itself over several, and 0.84 answers that; 0.49 read
    as one is a model that could not tell, which is how `ls` came to be drawn as a command that
    changes something.

    note: where there is no distribution this falls back to whatever laya called confidence,
    which is better than nothing, and where there is neither the answer goes out at 0.0 - read
    by the caller as "could not tell" rather than as a clean bill.
    """
    probabilities = spread(answer)
    if probabilities:
        return float(max(probabilities))

    return number(answer.get("confidence"))


def answered(kind, answer):
    """One of laya's answers, in the shape the documented API answers in.

    note: built rather than forwarded, and the type comes from the *question* - the
    authoritative source for it, since this is what sent it. laya carries a `type` of its own
    and it has always agreed; trusting the question costs nothing and cannot disagree.

    note: an answer this cannot read is left out rather than guessed at. The caller reads a
    missing answer as nothing having been said about that question, which is an outcome it
    already handles; a fabricated one is a number nobody sent.
    """
    if not isinstance(answer, dict):
        return None

    if kind == "noul":
        # note: a noul's number *is* its confidence, so there is no second field to compute -
        # and laya agrees, sending the same number twice
        if not isinstance(answer.get("noul"), (int, float)):
            return None
        return {"type": "noul", "noul": number(answer["noul"])}

    if kind == "choice":
        if not isinstance(answer.get("choice"), str):
            return None
        return {
            "type": "choice",
            "choice": answer["choice"],
            "confidence": confidence(answer),
            "probabilities": answer.get("probabilities", {}),
        }

    if kind == "score":
        if not isinstance(answer.get("score"), (int, float)):
            return None
        return {
            "type": "score",
            "score": number(answer["score"]),
            "confidence": confidence(answer),
            "legend": answer.get("legend", {}),
            "probabilities": answer.get("probabilities", {}),
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

    note: what this is for is the difference between an engine that cannot place a command and
    one whose answer is being read wrongly. The first block is what laya said; the second is
    what kamchatka would be handed. A `score` in the right place under a flat distribution is
    the rubric being hard to read; a good distribution translated into a bad `confidence` is
    this file.
    """
    from laya import Router

    asked = {
        "rating": {
            "type": "score",
            "instructions": (
                "A command is about to be run on the user's machine, and a person is about to "
                "be asked whether to let it. Place it on this rubric by what it would do."
            ),
            "criteria": LEVELS,
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
    # note: laya's own answer for `ls`, kept verbatim rather than invented - which is the
    # difference between a test about this translation and a test about what I imagined it
    # translates. The `confidence: 0.4932` beside a distribution that is 84% on one level is
    # the whole reason this file computes its own.
    recorded = {
        "model": "laya-rl-agent",
        "answers": {
            "rating": {
                "type": "score",
                "score": 0.2461,
                "legend": {"0": "…", "1": "…", "2": "…"},
                "probabilities": {"0": 0.8373, "1": 0.0792, "2": 0.0835},
                "confidence": 0.4932,
                "action": {"act_probability": 1.0},
            },
            "verdict": {
                "type": "choice",
                "choice": "allow",
                "confidence": 0.94,
                "probabilities": {"allow": 0.94, "deny": 0.06},
            },
            "irreversible": {"type": "noul", "noul": 0.7399, "confidence": 0.7399},
        },
        "usage": {"input_tokens": 112, "output_tokens": 0},
    }

    out = translated(asked, recorded)

    # every answer carries the type the *question* had, which laya does not send
    assert out["rating"]["type"] == "score", out
    assert out["verdict"]["type"] == "choice", out
    assert out["irreversible"]["type"] == "noul", out

    # the score is laya's own and the confidence is not. 0.2461 is where `ls` landed - the
    # bottom level, correctly - and 0.8373 is how sure it was of landing there. laya's own
    # `confidence` for the same answer is 0.4932, which read as one puts `ls` under every
    # threshold the caller has and draws it as a command that changes something
    assert out["rating"]["score"] == 0.2461, out
    assert out["rating"]["confidence"] == 0.8373, out
    assert out["rating"]["confidence"] != recorded["answers"]["rating"]["confidence"], out
    assert out["verdict"]["confidence"] == 0.94, out

    # a noul's number is its confidence, so there is no second field to invent
    assert out["irreversible"]["noul"] == 0.7399, out
    assert "confidence" not in out["irreversible"], out

    # and nothing laya sends that the documented shape has no room for comes along
    assert "action" not in out["rating"], out

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
