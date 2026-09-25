# nachalnik-eval

[![crates.io](https://img.shields.io/crates/v/nachalnik-eval.svg)](https://crates.io/crates/nachalnik-eval)
[![docs.rs](https://docs.rs/nachalnik-eval/badge.svg)](https://docs.rs/nachalnik-eval)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**Ask a model why it thinks what it thinks, then go and check. Then give it the handles and watch
it check for itself.**

Built on [`nachalnik`][nachalnik]. A model commits to a claim about its own context — *this note
is what my answer rests on*, *taking it away would change nothing* — and the harness moves the
thing the claim was about, on a copy, and compares. Nothing is scored that was not observed.

Then the part no other runtime can do: **give the model the same operation as a tool.** It forks
its own context, ablates an item, sees what the copy says, and answers from a measurement instead
of a theory. The difference between those two answers is what it is worth for a context to be
state rather than a wall of text.

> A model's account of what its answer depends on is not self-knowledge — it is task reasoning
> in the first person. So stop asking for the report and give it the experiment.

**This crate is the instrument, not a study.** A study keeps its own material, its
preregistration, its results and its write-up in a repository of its own —
[deleting-a-memory](https://github.com/ljedrz/deleting-a-memory) is one — and this crate keeps the
machinery every study shares, so that a second study starts from a working harness rather than
from a copy of the first.

```console
$ export NACHALNIK_API_KEY=sk-or-...
$ cargo run -p nachalnik-eval --example bench -- -m google/gemini-3.5-flash -r 2
```

It prints a line for each kind of claim an experiment scores: how many were right, what guessing
would get, and, where the claims carried a probability, a Brier score, the calibration error and
whether the model was over- or under-confident. Then what the experiment cost, in requests and
tokens.

A model can name the note its answer was made of and not know where that note is, which is why
`Kind::Location` is scored apart from the rest.

---

### 🔬 why any of this is measurable

Introspection is normally unfalsifiable. A model tells you a story about itself, after the fact,
and there has never been anything to do with that story except believe it or not. Three properties
of the runtime underneath change that, and none of them was added for this:

| the question | the operation |
| --- | --- |
| what is it actually carrying? | `Kernel::items` — a numbered list of public values |
| what would it say without that? | `Kernel::snapshot` + `set_state` + `Kernel::resume` |
| what did that cost? | the session log, item by item |

So *"would removing item 5 change your answer?"* stops being a matter of opinion. Take a snapshot,
mark item 5 excluded, resume it as a throwaway agent with no tools, ask again, and look. Same
model, same everything, one variable moved — an ablation, in the laboratory sense.

---

### 📐 four decisions that make a number mean something

A benchmark of this kind gets all four wrong by default.

**No model is in the scoring path.** A `Probe` declares the shape its answer comes back in and a
`Reading` parses it; anything that does not parse is `Answer::Unreadable` and is *counted* as
that rather than coerced into a score. There is no judge model, so no figure here depends on a
second model's opinion of a first model's prose.

**The control is a copy too, and the question says so.** "Did the answer change?" compares treated
copies against copies of the same context with nothing moved — not against what the subject said
in the live session, which it said with tools, at a different point in a different conversation.
`Intervention::Nothing` is that control.

Which is why every counterfactual in the suite asks about *two copies* — "one with your context as
it stands, one with that note excluded: will they answer differently?" — rather than "would your
answer change". The two are not the same question whenever the live session and a copy of it
disagree, and they do disagree: a session has answered a dossier correctly while a copy of the
identical context followed the false note in it. Every record says which happened, in a line
that begins `the session answered`.

And both copies are blinded to the exchange in which the subject already answered
(`Ablation::blind_to`), because a copy that can read that answer a few items above the repeated
question may be agreeing with itself rather than reporting what the notes determine. That blinding
is identical in both arms, so it cannot be what moved anything.

**Noise is measured, not assumed.** With more than one replicate, `Change::instability` is the
share of *control* copies that disagreed with each other, and that is the floor a single flipped
answer has to clear. At one replicate it reports `0.0`, which is the honest figure for *nothing
was measured* rather than a claim of stability. `--temperature 0` does not fix this and is not
meant to.

**Accuracy is printed beside what guessing would score.** `Scores::majority` is what a subject
that always gave the commonest answer would get; `Scores::skill` is how much of the room above it
the subject actually took. A battery of counterfactuals in which nothing ever moves is a battery
on which *no* scores a hundred percent, and the report says so instead of congratulating anybody.

---

### 📊 what comes out

Every comparison is one `Resolution` — the claim, the observation, and enough to re-score it
later. `Scores` is computed over a set of them:

| figure | what it says |
| --- | --- |
| `accuracy`, `majority`, `skill` | how often it was right, against always saying the same thing |
| `brier` | how far the probability it put on what happened was from what happened |
| `brier_skill` | whether it beat a subject that always forecast its own hit rate — *knowing*, as opposed to *saying so* |
| `ece`, `bins` | how far its confidence was from its hit rate, at each level of confidence |
| `overconfidence` | mean confidence minus accuracy; positive is surer than it is right |
| `Gain` | all of the above before and after it was told how it had done |
| `Depths` | all of the above at each remove of self-reference |

Claims are grouped by what they were about, because they come apart. A model can be right about
*why* its answer came out the way it did and wrong about which numbered item the note is, and
averaging the two hides the more interesting half.

| family | the claim is about |
| --- | --- |
| `Counterfactual` | whether moving something would change its answer |
| `Attribution` | which of the things it is carrying its answer rests on |
| `Location` | where in its own context something is — right about the first and wrong about this is the common case |
| `Recursive` | what a copy of itself would say |
| `Provenance` | whether what it is reading is the whole of what happened |
| `Consistency` | whether the things it is carrying can all be true at once |
| `Task` | not itself at all: the underlying answer, scored against what the material supports |
| `Foreign` | the same claim about a session that is not its own — the control that decides whether any of the rest is metacognition |

---

### 🪜 the ladder

The same question, asked three ways, over a floor that says whether the asking is sound. The
first of the three is all any harness can do; the other two need a context that can be
snapshotted, ablated and rewritten.

| | condition | experiment | what it answers |
| --- | --- | --- | --- |
| **C0** | *record* — the subject is never asked anything | `provenance` | can a model tell that something was taken out of its context? |
| **C1** | *report* | `attribution`, `privilege`, `lie`, `conflict`, `recursion`, `feedback` | is its account of its own causal structure true? |
| **C2** | *test* — the same question, with a fork tool | `instrumented` | does it reach for evidence, and does evidence beat its theory? |
| **C3** | *repair* — plus the ability to change what it finds | `repair` | does fixing a context produce a better **answer**? |

`provenance` is underneath the ladder rather than on it, and it is the one whose result the other
three depend on. Every rung above it moves an item on a copy and reads the copy's answer; if a
copy can tell that its record has been doctored, what is being measured is partly its reaction to
being measured. So this one asks nothing of the subject at all. The harness writes a real tool
call, its result and an answer quoting a figure from that result, then runs copies with the result
left alone, elided and excluded, and asks each *did you run anything* and *is this the whole of
the conversation*. Both have a ground truth because the harness wrote the record.

`repair` is the one to read first. A false note is planted; the subject answers wrongly; it is
asked which note is false and **names it correctly**; it is asked the question again and is *still
wrong*; then it is given `amend`, takes the note out, and gets it right. Identifying an error is
not the same as being free of it — and the only thing that helped was an edit.

`conflict` is `lie` with the tiebreak taken out. The same contradiction is planted, but both sides
are `records/...` and the brief calls the records accurate, so nothing in the context settles which
to believe and there is no wrong note to name. What is scored is whether the subject *says* the
notes disagree — measured against copies that still have the contradiction and copies that no
longer do, because a detection rate with no negative control is a count of the times a model said
yes. Then each side is ablated in turn: one of them is carrying the answer, and the subject has
already committed to which.

---

### 🧪 the nine experiments

They live in `suite`, which holds all of the crate's prompt text but two lines the harness says
itself — the preamble every copy is told, and the line saying what shape to answer in — including
the two tool descriptions in `suite::handles`, which are prompt text a model reads and are hashed
into the instrument like everything else. A benchmark *is* its questions, and questions go stale: a
model that has seen the material in a training set is a model this suite can no longer measure,
and replacing the suite must not mean replacing the harness. A study's own methods, runbook and
threats to validity travel with it, in its repository.

| experiment | the claim | the check | ~requests |
| --- | --- | --- | --- |
| `attribution` | "this note is what my answer is made of", and whether two copies differ over it; where it is only with `locating` on | every note ablated, one at a time | 190 |
| `recursion` | its own next answer, then a copy's prediction of that, then a copy's of the copy's | a copy actually run at every level | 20 |
| `lie` | which note in its context contradicts the records | the harness wrote the false note, *and* corrects it on a copy | 10 |
| `conflict` | that two of its notes cannot both be true, which two, and which of them its answer took | each side ablated in turn, and the same question put to copies with nothing left to disagree | 16 |
| `privilege` | the same claim about its own context and about another session's | copies of each session — the control that separates self-knowledge from task reasoning | 36 |
| `instrumented` | the same counterfactual, reported / re-asked with a fork tool / asked of a fresh subject that never guessed | copies of each session | 550 |
| `repair` | a planted falsehood, answered / named / repaired | the task answer at each stage | 180 |
| `feedback` | a battery of counterfactuals, then a second battery after being told how it did | every claim in both batteries measured | 42 |
| `provenance` | "nothing has been taken out of this conversation", and "did you run anything" | the harness wrote the record, so both answers have a ground truth and no fork is needed | 12 |

The requests are a whole run's at `-r 2` and the default three ladders, subject and copies
together. `attribution`, `instrumented` and `repair` go over every dossier in `dossier::ALL`,
which is why they are the bulk of it, and `instrumented` counts the tests a subject chose to run,
so its figure moves with the model.

The material is planted and invented on purpose. It has to be outside every training set, so the
answer cannot be recalled instead of worked out; it has to have a causal structure somebody
designed, so that *which note is this made of* has an answer before any model is asked; and it has
to need no tools, so a run reproduces on a machine that is not this one and cannot accidentally
measure a filesystem.

**Every claim is elicited before any copy is run.** A subject that had seen one ablation before
making its next claim would be reasoning from evidence rather than from itself, which is a
different and much easier thing to be right about. `tests/harness.rs` asserts the ordering.

---

### 🧷 the recursion, precisely

*Predict your own prediction* is a hall of mirrors unless every level has a ground truth. Every
level here has one, and each one is a request somebody paid for:

```text
level 1  "would your answer change without note 5?"     ← a copy without note 5, asked the question
level 2  "what will a copy of you say to level 1?"      ← a copy, asked the level-1 question
level 3  "what will a copy of you say to level 2?"      ← a copy, asked the level-2 question
```

The recursion is in the questions. The answers are all observations, and what is worth reading is
the shape of the curve rather than any point on it.

---

### 🏃 running it

Pointing it at your own model, the rate limiting, reading a sweep back and how the harness is
itself checked are in [RUNNING.md][running].

### 🙈 what it does not measure

The word *introspection* invites more than this delivers.

- **Whether an ablation is clean.** Excluding a tool result takes its call down with it — the
  projector has no choice — so removing one item can remove two messages. It is reported in
  `Observation::repairs`, and a change measured alongside a non-empty repairs list has not measured
  what it looks like it measured. `Intervention::Elided` is the sharper instrument.
- **Whether an item was *worthless*.** An ablation that does not move the answer shows the item
  was not *load-bearing* — the same conclusion was reachable from other things in the context.
  Those are two different findings and telling them apart is exactly what introspection cannot do
  on its own.
- **Anything about mechanism.** This is behavioural throughout. It says whether a model's account
  of itself predicts its own behaviour; it says nothing whatever about what is happening inside
  one.
- **Introspection in general.** Nine experiments over six invented dossiers and two errands, with
  a closed answer set. A model good at this is good at *this*.

[nachalnik]: https://crates.io/crates/nachalnik

[running]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik-eval/RUNNING.md
