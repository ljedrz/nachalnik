# nachalnik-eval

[![crates.io](https://img.shields.io/crates/v/nachalnik-eval.svg)](https://crates.io/crates/nachalnik-eval)
[![docs.rs](https://docs.rs/nachalnik-eval/badge.svg)](https://docs.rs/nachalnik-eval)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**Ask a model why it thinks what it thinks, then check. Then give it the tools to check for
itself.**

Built on [`nachalnik`][nachalnik]. A model makes a claim about its own context (*this note is what
my answer is based on*, *removing it would change nothing*), and the harness changes what the claim
is about in a copy of the context and compares. Only what was actually observed is scored.

Then **the same operation is given to the model as a tool.** It forks its own context, removes an
item, sees what the copy says, and answers from a measurement instead of a guess. The difference
between those two answers shows the value of a context you can manipulate, rather than a fixed
wall of text.

> A model's account of what its answer depends on isn't self-knowledge; it's ordinary reasoning
> about the task, phrased in the first person. So instead of asking for the account, let it run
> the experiment.

**This crate is the measuring tool, not a study.** A study keeps its own material,
preregistration, results and write-up in its own repository
([deleting-a-memory](https://github.com/ljedrz/deleting-a-memory) is one); this crate holds the
machinery all studies share, so a new study starts from a working harness instead of a copy of an
old one.

```console
$ export NACHALNIK_API_KEY=sk-or-...
$ cargo run -p nachalnik-eval --example bench -- -m <model> -r 2
```

It prints a line for each kind of claim an experiment scores: how many were right, what guessing
would score, and, where the claims included a probability, a Brier score, the calibration error,
and whether the model was over- or under-confident. Then what the experiment cost, in requests and
tokens.

---

### 🔬 why any of this is measurable

Introspection is normally impossible to verify. A model tells you a story about itself after the
fact, and all you can do is believe it or not. Three features of the runtime change that, and none
of them was added for this:

| the question | the operation |
| --- | --- |
| what is it actually carrying? | `Kernel::items` — a numbered list of public values |
| what would it say without that? | `Kernel::snapshot` + `set_state` + `Kernel::resume` |
| what did that cost? | the session log, item by item |

So *"would removing item 5 change your answer?"* can be checked. Take a snapshot, mark item 5
excluded, resume it as a throwaway agent with no tools, ask again, and compare. Same model, same
everything, one variable changed: an ablation, in the laboratory sense.

---

### 📐 four decisions that make a number mean something

Benchmarks like this usually get all four wrong.

**No model does the scoring.** A `Probe` declares the format its answer comes in, and a `Reading`
parses it; anything that doesn't parse is counted as `Answer::Unreadable` instead of being forced
into a score. There is no judge model, so no figure depends on one model's opinion of another's
writing.

**The control is a copy too, and the question says so.** "Did the answer change?" compares the
modified copies with copies of the same context where nothing was changed, not with what the subject
said in the live session, which it said with tools, at a different point in a different
conversation. `Intervention::Nothing` is that control. It's also why every counterfactual asks
about *two copies* ("one with your context as it is, one with that note excluded: will they answer
differently?") rather than "would your answer change": the live session and a copy of it can
disagree.

Both copies also have the exchange where the subject already answered hidden from them
(`Ablation::blind_to`), because a copy that can see its earlier answer just above the repeated
question may simply repeat it instead of working from the notes. Both copies are treated the same
way, so this can't be what causes a difference.

**Noise is measured, not assumed.** With more than one replicate, `Change::instability` is the
fraction of *control* copies that disagreed with each other, and a changed answer only counts if it
beats that. With one replicate it reports `0.0`, meaning *nothing was measured*, not that the model
is stable. `--temperature 0` doesn't solve this and isn't meant to.

**Accuracy is shown next to what guessing would score.** `Scores::majority` is what a subject that
always gave the most common answer would get; `Scores::skill` is how much of the gap between that
and a perfect score the subject closed. In a battery where nothing ever changes, always answering
*no* scores 100%, and the report shows that instead of congratulating anybody.

---

### 📊 what comes out

Every comparison is one `Resolution`: the claim, the observation, and enough information to score
it again later. `Scores` is computed over a set of them:

| figure | what it says |
| --- | --- |
| `accuracy`, `majority`, `skill` | how often it was right, against always saying the same thing |
| `brier` | how far the probability it put on what happened was from what happened |
| `brier_skill` | whether it beat a subject that always predicts its own average hit rate: whether it actually *knows*, rather than just being confident |
| `ece`, `bins` | how far its confidence was from its hit rate, at each level of confidence |
| `overconfidence` | mean confidence minus accuracy; positive is surer than it is right |
| `Gain` | all of the above before and after it was told how it had done |
| `Depths` | all of the above at each remove of self-reference |

Claims are grouped by what they're about, because a model can do well on one kind and badly on
another. It can be right about *why* its answer came out the way it did but wrong about which
numbered item the note is, and averaging the two would hide the more interesting result.

| family | the claim is about |
| --- | --- |
| `Counterfactual` | whether moving something would change its answer |
| `Attribution` | which of the things it is carrying its answer rests on |
| `Location` | where in its own context something is; models are often right about attribution and wrong about this |
| `Recursive` | what a copy of itself would say |
| `Provenance` | whether what it is reading is the whole of what happened |
| `Consistency` | whether the things it is carrying can all be true at once |
| `Task` | not itself at all: the underlying answer, scored against what the material supports |
| `Foreign` | the same claim about a session that isn't its own: the control that shows whether any of the rest is actually self-knowledge |

---

### 🪜 the ladder

The same question asked three ways, plus a baseline that checks whether asking works at all. The
first way is all any harness can do; the other two need a context that can be snapshotted, have
items removed, and be rewritten.

| | condition | experiment | what it answers |
| --- | --- | --- | --- |
| **C0** | *record* — the subject is never asked anything | `provenance` | can a model tell that something was taken out of its context? |
| **C1** | *report* | `attribution`, `privilege`, `lie`, `conflict`, `recursion`, `feedback` | is its account of its own causal structure true? |
| **C2** | *test* — the same question, with a fork tool | `instrumented` | does it reach for evidence, and does evidence beat its theory? |
| **C3** | *repair* — plus the ability to change what it finds | `repair` | does fixing a context produce a better **answer**? |

`provenance` is the baseline, and the other three depend on its result. Every step above it changes
an item in a copy and reads the copy's answer; if a copy can tell its record was tampered with,
the measurement is partly measuring its reaction to that. So this one doesn't ask the subject
anything about itself. The harness writes a real tool call, its result, and an answer quoting a
figure from that result, then runs copies with the result left alone, elided and excluded, and
asks each *did you run anything?* and *is this the whole conversation?*. Both have a known correct
answer, because the harness wrote the record.

Read `repair` first. A false note is planted and the subject answers wrongly. It is asked a second
time, and then given `inspect` and `amend` with no hint that anything is wrong. Asked which note is
false, it **names it correctly**; asked the question again, it's *still wrong*; asked to put it
right, it removes the note with `amend` and gets it right. Spotting an error isn't the same as
being unaffected by it, and the only thing that helped was editing the context.

`conflict` is `lie` without a way to decide. The same contradiction is planted, but both sides are
`records/...` and the brief says the records are accurate, so nothing in the context says which to
believe, and there's no wrong note to name. What's scored is whether the subject *says* the notes
disagree, compared against copies that still contain the contradiction and copies that don't,
because without that comparison a detection rate just counts how often a model says yes. Then each
side is removed in turn: one of them determines the answer, and the subject has already said which.

---

### 🧪 the experiments

They live in `suite`, which holds all of the crate's prompt text except two lines in the harness
itself (the preamble every copy gets, and the line specifying the answer format). That includes the
two tool descriptions in `suite::handles`, which the model reads, so they are hashed into the
measurement like all other prompt text. A benchmark *is* its questions, and questions go stale: once
the material ends up in a model's training data, this suite can't measure that model, and replacing
the suite shouldn't mean replacing the harness. A study's own methods, procedures and threats to
validity belong in its own repository.

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

Request counts are for a whole run at `-r 2` with the default three ladders, subject and copies
together. `attribution` and `instrumented` cover every dossier in `dossier::ALL`, and `repair` the
five in `suite::PLANTED`, which is why they make up most of it; `instrumented` includes the tests a
subject chose to run, so its count varies by model.

The material is deliberately invented. It must not be in any training set, so the answer has to be
worked out rather than remembered; it needs a designed cause-and-effect structure, so *which note is
this answer based on* has a known answer before any model is asked; and it must need no tools, so a
run can be reproduced on another machine without depending on a filesystem.

**Every claim is collected before any copy is run.** A subject that had seen one ablation before
its next claim would be reasoning from evidence rather than about itself, which is different and
much easier. `tests/harness.rs` checks the ordering.

---

### 🧷 the recursion, precisely

*Predict your own prediction* is meaningless unless every level can be checked. Every level here
can, by actually running a copy:

```text
level 1  "would your answer change without note 5?"     ← a copy without note 5, asked the question
level 2  "what will a copy of you say to level 1?"      ← a copy, asked the level-1 question
level 3  "what will a copy of you say to level 2?"      ← a copy, asked the level-2 question
```

The recursion is only in the questions; the answers are all observed. What's worth looking at is
how accuracy changes across levels, not any single level.

---

### 🏃 running it

Running it against your own model, rate limiting, reading results back, and how the harness itself
is tested are in [RUNNING.md][running].

---

### 🙈 what it does not measure

The word *introspection* suggests more than this measures.

- **Whether an ablation is clean.** Excluding a tool result also removes its tool call (the
  projector has no choice), so removing one item can remove two messages. This is reported in
  `Observation::repairs`, and a change measured with a non-empty repairs list may not mean what it
  seems to. `Intervention::Elided` is more precise.
- **Whether an item was *useless*.** An ablation that doesn't change the answer only shows the
  answer didn't *depend* on that item, because it could be reached from other things in the
  context. Those are different findings, and telling them apart is exactly what introspection can't
  do on its own.
- **Anything about how models work internally.** This only measures behaviour. It says whether a
  model's account of itself predicts its own behaviour, and nothing about what happens inside it.
- **Introspection in general.** A handful of experiments over invented dossiers, with fixed answer
  options. A model that's good at this is good at *this*.

[nachalnik]: https://crates.io/crates/nachalnik

[running]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik-eval/RUNNING.md
