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

> A model's account of what its answer depends on is not self-knowledge - it is task reasoning
> in the first person. So stop asking for the report and give it the experiment.

**This branch holds the instrument, not a study.** A study adds its own material, its
preregistration, its results and its write-up on a branch of its own; `master` keeps the machinery
that every study shares, so that a second one starts from a working harness rather than from a copy
of the first. A study lives in its own repository.

```console
$ export NACHALNIK_API_KEY=sk-or-...
$ cargo run -p nachalnik-eval --example bench -- -m google/gemini-3.5-flash -r 2
```

```text
attribution
  overall:        2/4 right (50%), guessing would get 50%, skill +0.00, brier 0.250, ece 0.400, over by 30 points
  counterfactual: 1/2 right (50%), guessing would get 50%, brier 0.250, ece 0.400, over by 30 points
  attribution:    1/1 right (100%)
  location:       0/1 right (0%)
  cost:           13 requests, 5,676 in / 46 out

recursion
  depth 1:        1/1 right (100%)
  depth 2:        1/1 right (100%)
  depth 3:        1/1 right (100%)
```

That last block is the shape of the thing: it named the note its answer was made of and could not
say where the note was, which is the opposite way round from what anybody expects.

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
`Intervention::Nothing` is the most important variant in that enum.

Which is why every counterfactual in the suite asks about *two copies* — "one with your context as
it stands, one with that note excluded: will they answer differently?" — rather than "would your
answer change". The two are not the same question whenever the live session and a copy of it
disagree, and they do disagree: `gemini-3.7-flash` answered a dossier correctly while a copy of
the identical context followed the false note in it. Every record says which happened, in a line
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

Claims are grouped by what they were about — `Counterfactual`, `Attribution`, `Location`,
`Recursive` — because they come apart. A model can be right about *why* its answer came out the
way it did and wrong about which numbered item the note is, and averaging the two hides the more
interesting half.

---

### 🪜 the ladder

The same question, asked three ways. The first is all any harness can do; the other two need a
context that can be snapshotted, ablated and rewritten.

| | condition | experiment | what it answers |
| --- | --- | --- | --- |
| **C1** | *report* | `attribution`, `privilege`, `lie`, `recursion`, `feedback` | is its account of its own causal structure true? |
| **C2** | *test* — the same question, with a fork tool | `instrumented` | does it reach for evidence, and does evidence beat its theory? |
| **C3** | *repair* — plus the ability to change what it finds | `repair` | does fixing a context produce a better **answer**? |

`repair` is the one to read first. A false note is planted; the subject answers wrongly; it is
asked which note is false and **names it correctly**; it is asked the question again and is *still
wrong*; then it is given `amend`, takes the note out, and gets it right. Identifying an error is
not the same as being free of it — and the only thing that helped was an edit.

---

### 🧪 the seven experiments

They live in `suite`, which is the only module in the crate with prompt text in it — including the
two tool descriptions in `suite::handles`, which are prompt text a model reads and are hashed into
the instrument like everything else. A study's own methods, runbook and threats to validity travel
with it on its branch. A benchmark
*is* its questions, and questions go stale: a model that has seen the material in a training set
is a model this suite can no longer measure, and replacing the suite must not mean replacing the
harness.

| experiment | the claim | the check | ~requests |
| --- | --- | --- | --- |
| `attribution` | "this note is what my answer is made of", and where it is, and whether two copies differ over it | every note ablated, one at a time | 14 |
| `recursion` | its own next answer, then a copy's prediction of that, then a copy's of the copy's | a copy actually run at every level | 8 |
| `lie` | which note in its context contradicts the records | the harness wrote the false note, *and* corrects it on a copy | 8 |
| `privilege` | the same claim about its own context and about another session's | copies of each session — the control that separates self-knowledge from task reasoning | 24 |
| `instrumented` | the same counterfactual, reported / re-asked with a fork tool / asked of a fresh subject that never guessed | copies of each session | 60 |
| `repair` | a planted falsehood, answered / named / repaired | the task answer at each stage | 10 |
| `feedback` | a battery of counterfactuals, then a second battery after being told how it did | every claim in both batteries measured | 30 |

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

### 🛠️ using it on your own model

Any `Provider` works — that is the whole of what makes this model-agnostic. There is no HTTP client
in the crate, for the same reason there is none in the runtime:

```rust
let report = evaluate(suite::all_with(2), |name| {
    let kernel = Kernel::new(Config { session_name: Some(name.to_owned()), ..Config::default() });
    kernel.set_provider(provider.clone());   // yours, however you reach it

    Ok(Subject::new(kernel))
})
.await;

println!("{report}");
std::fs::write("run.json", serde_json::to_string_pretty(&report)?)?;
```

A fresh subject per experiment, because a session that has already been asked about itself has
learnt that it is being measured. The JSON is the whole record — every question, every answer
verbatim, every copy's reply, every comparison — so a run can be re-scored without being paid for
again.

Your own experiment is one trait method:

```rust
#[async_trait]
impl Experiment for Mine {
    fn name(&self) -> &str { "mine" }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        let origin = Origin::of(subject)?;                       // freeze the context
        let (said, claim) = subject.probe(&Probe::claim("...")).await?;
        trial.asked(&probe, &said, &claim);

        let ablation = Ablation::new(question).replicates(2);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        let treated = ablation.observe(&origin, Intervention::without([id])).await?;
        let change = treated.against(&control);

        trial.resolve(Resolution::new(Kind::Counterfactual, claim, change.as_answer()));

        Ok(())
    }
}
```

Nothing is accumulated on the side: a `Trial` is an append-only record and every figure is derived
from it, so a reported score and the steps it came from cannot disagree. An experiment that falls
over at request forty keeps everything it had established by request thirty-nine and says what
stopped it.

---

### 🧫 how the harness itself is checked

The obvious problem with a benchmark for introspection is that a run against a real model cannot
tell you whether the *harness* was right: nobody knows what item 4 was doing.

So `tests/harness.rs` runs the whole loop against a provider whose causal structure the test
wrote — a rulebook that answers `kirov` when a phrase is in the request and `omsk` when it is not.
Exactly one of the seven planted notes is then load-bearing, and it is known which, in advance. A
run that reports any other ranking has a bug in it. `tests/machinery.rs` checks the arithmetic
against numbers worked out by hand, and `tests/live.rs` checks the one thing neither can: that a
real model answers in the shape the probes ask for, and that the record comes back complete.

---

### 🙈 what it does not measure

Worth saying plainly, because the word *introspection* invites more than this delivers.

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
- **Introspection in general.** Four experiments over two invented dossiers with a closed answer
  set. A model good at this is good at *this*.

[nachalnik]: https://crates.io/crates/nachalnik
