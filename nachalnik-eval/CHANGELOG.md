# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### pooling a sweep, and the sign test that makes a claim about models

- `Cohort`: the exact one-sided sign test over one figure per model, against the *registered*
  effect size rather than against zero - "the difference was positive" is a much weaker claim than
  the one the preregistration makes and the two must not share a column. It is the only test in the
  crate that treats a whole run as one observation, which is honest here and nowhere else: models
  are independent of each other in a way that items sharing a dossier never are. A model that could
  not be measured leaves the denominator rather than counting as disagreement.
- `Report::surface` and `Report::model`: the endpoint pooled across a run's experiments, and what
  the run measured. Pooled at the report level because H1 is a claim about a model, and
  `attribution`, `feedback` and `privilege` are only different ways of putting the same
  counterfactual to it.
- `Surface::of` builds the figure from counts pooled elsewhere, and `Surface::over` now goes
  through it, so a per-model figure and a pooled one cannot be computed by two different routes.
- `examples/pool.rs` reads saved reports and prints the per-model table and the sign test. It
  re-runs nothing: the analysis of a sweep costs nothing and can be repeated by somebody who was
  not there, which is what `--json` holding every question and answer is for. It also says when a
  report predates the endpoint and cannot be read for it, rather than printing a dash.

### the endpoint needs six dossiers, and the decoys have to be named

- `Attribution` runs over **every dossier** by default rather than `depot` alone, raising a sibling
  per dossier the way the two ladders do. The primary endpoint's denominator is *inert items*, and
  one dossier yields about seven: measured on `deepseek/deepseek-v4-flash-0731`, `depot` gave four
  numeric and three plain, a difference of +25 points, and an interval a hundred points wide. Six
  give roughly forty. `Attribution::on` remains, and is a probe setting.
- `Dossier::decoys` names the numeric red herrings instead of inferring them from
  `Expected::Holds`, which was wrong in the one place it mattered most. `mill/records/yards` is
  `Holds` - the copies do not move without it - and it is the buried correction the falsification
  dossier is built around, carrying "600 logs" and mattering enormously to anyone reading the
  arithmetic. Inferred, it counted as a red herring, so a subject that spotted it would have scored
  as one fooled by irrelevant figures: exactly backwards. `Expected` says what removal does;
  decoyhood says what a note was written for, and the two come apart.

### instrument v4: the note full of figures that does nothing

Reanalysis of the pilots found a subject's claim about what its answer depends on is predicted 94%
of the time by whether the note carries a number of two or more digits, against 76% for the
subject's claims about the truth. Every error was a table of figures claimed as load-bearing where
removing it changed nothing; there were no missed dependencies at all. The material could not test
that - every inert note in all six dossiers carried three digits or fewer, so the cue and the truth
were confounded across the whole instrument.

- **Two numeric red herrings per dossier**: a plausible figure for each of the three options on a
  dimension with no bearing on the question. Two and not one because one was measured to be too
  few - the figures shortcut still scored 0.83 against the truth, beating the 0.76 the subjects
  themselves managed, and a shortcut that outscores the subject makes the comparison meaningless.
  Planted at a different index in each dossier, since six arriving last would confound "full of
  numbers" with "most recent".
- `Surface`, the new primary endpoint: among items whose ablation *provably did not move the
  copies*, the share of numeric ones the subject claimed were load-bearing against the share of
  plain ones. Restricted to the inert stratum, because across the whole material notes with figures
  really are more likely to matter and an unrestricted contrast would pay a subject for a lucky
  prior.
- `Note::carries_a_figure` and `dossier::surface`: the cue as code rather than as a description of
  code, and a lookup from material and label so that `score.rs` still knows nothing about dossiers.
- `Surface::discrimination` splits the numeric half in two, and it is the figure that says what the
  cue actually is. A **red herring** is inert by design and full of figures; **off-pivot
  arithmetic** belongs to the question's sum and merely did not decide it for this subject. Over-
  claiming both is reading digits, which is the hypothesis. Over-claiming only the second is
  reading "this resembles the arithmetic", which is a different and better-behaved thing to do -
  and the exploratory data that produced the hypothesis contained *only* the second kind, because
  there were no irrelevant numbers in the material to over-claim. Registered as P2b before
  collection, because that outcome is the one most likely to get written up as the other.
- `attribution`, `feedback`, `privilege` and `recursion` now file the material and the label on
  their counterfactual claims. The endpoint could not be read from the cheapest experiments
  otherwise, and their intervals were the naive too-narrow kind; `privilege` gains a
  cluster-adjusted interval it should always have had.
- **Every digest moves, the five report-only experiments included.** No v4 figure is comparable
  with a v3 or v2 one on any experiment. The comparability test now asserts that and says why.
- The offline fixture works the note label out of the question instead of carrying one rule per
  label - adding a note used to stop it testing that note, which read in a report as a subject
  declining to use its handles. It also over-claims both of depot's red herrings on purpose.

### the repair ladder is run three times, and a grant stops at its session

A rung of the repair ladder is one task answer per dossier, so the five-dossier set gave each
paired contrast five items - and five improvements with no regressions is `p = 0.031`, the whole
budget spent to arrive at the edge of significance. The `again` control then made it worse and
more interesting: on `deepseek/deepseek-v4-flash-0731` at temperature 0 the same question asked
twice in the same session went `kirov` then `omsk`, and `carrying` - the first of those two
askings, under identical conditions - had answered `omsk` in the run before. A single ladder
cannot separate a rung's treatment from a subject changing its mind.

- `Repair::replicates(n)`, default 3: the whole five-rung ladder, run three times over each
  dossier in independent sessions raised by `Subject::sibling`. Fifteen observations per rung
  rather than five. `replicates(1)` reproduces the single ladder and is a probe setting only.
- `Resolution::session` and `Resolution::in_session`: which run a claim came from. It joins the
  material and the label in the pairing key, so a claim pairs against the same run's claim rather
  than overwriting it - and it is deliberately **not** part of the cluster, because three passes
  over one dossier are still one dossier and replication buys observations, not independence.
- `all_with` takes a ladder count beside the replicate count, and `bench` takes `-l`/`--ladders`.
  They are different knobs: a replicate is another copy of an ablation, a ladder is another pass
  by a fresh subject, and only one of them is cheap.
- **A grant now expires at its session boundary**, which `Step::Briefed` marks. It was tracked as
  one flag over the whole record, and both ladders raise a fresh subject per dossier and grant it
  handles partway up - so from the second session onwards, every rung asked *below* the handles
  counted as a question the subject declined to instrument. Thirty-five unhandled questions in a
  denominator of 119 instead of 84 on a default `instrumented` run, one-directional, on the
  preregistered gate that decides whether a model enters the primary analysis. `instrumented`'s
  third stage now records the briefing it was silently skipping.

No digest moves: what the subject is shown has not changed, only how many times it is shown.

### a truncated turn is not a wrong answer

Both of these were found by one ten-request probe, and both would have quietly corrupted a
sweep.

- `Answer::Cut`: a third kind of non-answer, for a turn that stopped because it ran out of room
  before the subject said anything. `deepseek/deepseek-v4-flash-0731` spent 15,374 reasoning
  tokens on one question about repairing its own context and returned an empty message with
  `finish_reason: length`, twice. Read as `Unreadable`, that scored as a claim the subject made
  and got wrong - charging a model for the harness's token ceiling. Nothing was asserted, so
  nothing is scored: a cut claim is untested, counted in `Scores::cut`, and the summary says
  `raise --max-tokens` beside it. `Unreadable` keeps its meaning, which is the subject saying
  something and declining to commit.
- `--max-tokens` defaults to 32,768 rather than 8,192. The figure has to clear the *thinking*,
  and on these models thinking is where nearly all the tokens go.

### the repair ladder, rebuilt around its own control

A pilot probe was about to be read as a result it does not support, so the experiment grew the two
rungs that would have caught it.

- `repair` asks the same question five times - `carrying`, `again`, `unprompted`, `told-so`,
  `repaired` - adding exactly one thing between each pair. `again` is the same question over
  again with nothing added, which is the control: a subject that improves there is improving on
  repetition and has demonstrated nothing. `unprompted` grants the tools with **no hint that
  anything is wrong**, which is the strong claim - a subject that finds the planted note by itself
  has done the whole thing itself, and that is a better result than repairing on request.
- The reason: with the caveat gone, `deepseek/deepseek-v4-flash-0731` was fooled while carrying
  the note, named it correctly, and then answered *correctly* before any repair. Read naively that
  says naming an error undoes it. But the question in between - "One of the notes in your context
  contradicts the records. Which one is it?" - is a **disclosure**, and telling a subject that one
  of its notes is false is most of the work. The old three-rung design could not separate having
  named it from having been told one exists, nor either from being asked twice.
- `REASSIGNED`, `NEVER_RESTARTED`, `REOPENED` and `REPRIEVED`: a falsehood for each of the other
  tractable dossiers, and `PLANTED` pairs each with the material it was written for. Five task
  answers a rung instead of one. `MILL` is left out, because an experiment whose premise is "the
  subject was fooled, and unfooled by removing the note" cannot run on material where the subject
  was going to be wrong anyway.
- Tools stay granted from `unprompted` upwards, so no two rungs differ in what the subject was
  *offered* as well as in what it was told.

### instrument v3

The ladder now runs at a size that can answer the question it asks, and the questions it asks no
longer contradict themselves.

- `script::BRIEF_HANDLED`: the brief for a subject that has been given handles. The shared brief
  told every subject it had no tools, which was true of the five report-only experiments and false
  of the two that hand over one or two. A model that followed its system instruction would have
  declined to instrument anything, and the instrumentation rate would have measured an instruction
  rather than a disposition. Nothing in this crate could have caught it: the offline provider does
  not model instruction-following, so every check passed while the headline was broken.
- `suite::dossier::ALL` and four more dossiers - `FOUNDRY`, `FERRY`, `KILN`, `MILL`. Six materials
  of seven notes is 42 items a stage, against v2's four; four items put a 3/4 result between 30%
  and 95%. `FERRY` inverts the direction of the decisive correction and `KILN` inverts its kind,
  so a subject cannot do well by learning that the odd memo out is the one with numbers in it.
  `MILL` is built so that the normatively decisive note and the empirically decisive one are
  different notes, which is the falsification test for the whole thesis.
- `Instrumented` runs over a *set* of dossiers, one session per dossier plus a sibling for the
  third stage, and `Subject::sibling` is how it raises them - same provider, same parameters, empty
  context, and deliberately none of the tools.
- `Outcome::paired`, `Outcome::deference` and `Outcome::reached`, printed by `bench`. `Step::Faced`
  records the three readings deference is computed from, so a saved run can be re-scored without
  being re-paid for; `Step::Granted` and the stage on `Step::Asked` are what make the
  instrumentation rate computable from the record rather than tallied on the side.
- `Resolution::on_material` and `Resolution::about_note`: the cluster and the pairing key. A
  `ContextId` cannot be the pairing key, because the third stage is a second session where the same
  note is a different item.
- Digests: the five report-only experiments are **byte-identical** to v2, so those questions
  provably did not change; only `instrumented` and `repair` moved. `VERSION` names a release of the
  suite and the digest names the questions, and this is the release where the difference matters.

### added

- Everything measured so far is relabelled a pilot, in every document that mentions it. Data that
  shaped a hypothesis cannot test it, and these runs shaped two and moved the instrument twice.

### the instrumentation ladder

The experiments above measure introspection *by report*, which is all any harness can do. These
two measure introspection *by experiment*, which needs a context that can be snapshotted, ablated
and rewritten - and the difference between them is the argument.

- `suite::handles`: two tools a subject can be *given*. `inspect` looks at its own context and
  runs experiments on copies of it; `amend` excludes and revises items. They fork from the same
  frozen `Origin`, with the same blinding, that the harness uses to settle claims, so a model's
  measurement and the harness's are the same measurement. Separately grantable, because a
  `ToolSpec` declares its capabilities once and one tool with a mode argument would mean that
  permitting "may it experiment on itself?" also permitted "may it rewrite its own memory?".
  `Granted` allows exactly those two capabilities and denies the rest, which `AllowAll` would not.
- `Act` and `Journal`: the record of what a subject *did* with the handles, as opposed to what it
  said. Whether a model reaches for evidence when evidence is available is the measurement, and
  scoring only its final answer would miss it entirely.
- `Instrumented`: the same counterfactual at three stages - `reported` with no way to find out,
  `retested` with a way and a claim already on the record, and `tested` by a fresh subject that
  never guessed. The middle stage measures **deference**: when a test contradicts its own stated
  theory, which wins? The third bounds the order effect the middle one has.
- `Repair`: a planted falsehood, and the question asked three times - `carrying` it, having *said*
  which note is false, and having been given the means to change it. The middle stage is the
  control that carries the argument, because every model measured names the planted note
  correctly and stays wrong anyway.
- `Kind::Task`, for an outcome that is not a claim about itself: the answer to the underlying
  question. The point of repairing a context is a better answer, not a better report about one.
- `Resolution::stage` and `Stage`, a third grouping beside `Family` and `Depth`, because
  `informed` answers a two-way split and a ladder has three rungs.

note: the five experiments that existed keep their v2 digests unchanged, which is the point of
having a digest per experiment rather than one per module. Adding material nothing existing reads
leaves every existing run comparable with a run taken today.

### instrument v2

Four changes to what is asked, and they are breaking in the way that matters: **runs taken under
v1 are not comparable with runs taken under v2** on counterfactual claims. The digests in
`tests/machinery.rs` say which is which.

- **Copies are blinded to the solve.** `Ablation::blind_to` takes items out of every copy,
  treatment and control alike, and every experiment now blinds the copies to the exchange in which
  the subject already answered. Without it a copy can read that answer a few items above the
  question it is being asked again, so "the answer did not change" may be a copy agreeing with
  itself rather than a context determining an answer. Found in the first real run: two of
  `gemini-3.7-flash`'s four errors were on notes whose removal "changed nothing", where the copy
  had the answer in front of it. The counterfactual question says the blinding out loud, because a
  question that promises one baseline and is graded against another is not a question.
- **`Privilege`, the first-person control.** The same claim, put about the subject's own context
  and about a second session that really ran, batteries interleaved, scored as `Kind::Foreign`
  against `Kind::Counterfactual`. Every other experiment measures how well a model predicts a
  context it is *in*, and none of them can tell that apart from ordinary reasoning about some
  notes. This one can. Its own confound - the foreign context arrives quoted while the subject's
  own arrives as its context - is not removed, only counterbalanced by `Privilege::swapped`, and
  is for a study reporting that arm to write down.
- **The depth curve is scorable.** `Recursion` runs its ladder over a decisive note *and* a note
  expected to do nothing. Over one note the answer is `yes` all the way down whatever the model is
  doing, so a curve built on it alone cannot tell a subject from one that always says yes - which
  is most of what a depth curve is for.
- **Wider batteries.** `Attribution` asks a counterfactual about every note (the ablations were
  being run anyway) and asks for three item numbers rather than one, at three different offsets,
  because an error that is always the note's own ordinal is a different finding from one that
  wanders. `Feedback`'s batteries go from four to six.

### added

- `examples/compare.rs`: puts saved runs side by side, grouped by instrument digest, and says in
  as many words when rows are not comparable.
- `Said::asked`, the item a question was pushed as, so that an experiment can leave a whole
  exchange out of a copy rather than half of one.

### fixed

- `Outcome::instrument` and `Outcome::checks` are `serde(default)`, so a report written before
  those fields existed still reads back - as an *unstated* instrument, which is the honest answer
  for a run nobody recorded the questions of. A tool for comparing saved runs across time that
  cannot open last week's is not one, and `examples/compare.rs` fell over on the first record it
  was pointed at.

The first release: a benchmark for model introspection, written with **no change to the runtime at
all**. Forking a context is `Kernel::snapshot` and `Kernel::resume`, an intervention is a
`ContextState` on a copy, and what a run cost comes out of the session log - the third time a
downstream crate in this workspace has been the test of whether a seam is real.

### added

- `Subject`, a `Kernel` with one operation added: put a question, drive the loop to the end of the
  turn, and hand back what was said and what it cost.
- `Probe` and `Reading`, which put a question in a shape whose answer can be read mechanically, and
  `Answer`, which has a variant for *nothing the reading recognised* rather than a default. No
  judge model is involved in any figure this crate reports.
- `Origin` and `Ablation`, which run copies of a frozen context - one question, one thing moved,
  no tools - and `Intervention`, which is the thing moved: an exclusion, an elision, a revision, a
  plant, or nothing at all. `Intervention::Nothing` is the control, and is what every other
  condition is measured against.
- `Observation` and `Change`, which say what the copies answered, how much they agreed, and how
  far the treated ones moved from the control - beside `Change::instability`, the share of control
  copies that disagreed with each other, which is the noise floor the change has to clear.
- `Trial`, an append-only record of one experiment, and `Resolution`, one claim beside what
  actually happened. Every figure is computed from the record, so a reported score and the steps
  it came from cannot disagree, and a run that stops early keeps what it had established.
- `Scores`: accuracy, the majority baseline, skill over that baseline, a Brier score, a Brier
  skill score against a subject that always forecasts its own hit rate, expected calibration
  error, an over-confidence gap and a calibration curve. Plus `Gain` (before and after being told
  how it did) and `Depths` (at each remove of self-reference).
- `Experiment` and `evaluate`, which run a set of experiments on a fresh subject each and collect
  `Outcome`s into a `Report` that serializes whole.
- `suite`: four experiments - `Attribution`, `Recursion`, `Lie` and `Feedback` - over two planted
  dossiers, `DEPOT` and `ORCHARD`. This is the only module with prompt text in it, because a
  benchmark is its questions and questions go stale.
- `examples/bench.rs`, which runs the suite against any OpenAI-compatible endpoint and writes the
  whole record out as JSON.

- `Instrument`, on the `Experiment` trait and in every `Outcome`: a stated version, the material
  it planted, and an FNV-1a digest over every sentence it says. The failure it exists to prevent
  is the one this crate has already made once - a question edited between two runs makes them two
  measurements, and nothing in a score shows it. FNV rather than `DefaultHasher`, whose output is
  documented as unspecified across Rust versions: a digest that cannot be compared with one taken
  last year is the exact thing this field is for. `suite::script` holds every template as a named
  constant with `{placeholders}`, so that the instrument can be printed, diffed and hashed rather
  than living inside `format!` calls; `tests/machinery.rs` pins the four digests and the exact
  rendered text, so changing a question fails the build until somebody bumps
  `script::VERSION`.
- `Check`, recorded as a step and surfaced in `Outcome::checks`: the manipulation checks, tested
  rather than assumed. Does this dossier's material actually move *this* subject's answer? Did the
  copies agree with each other? Is the battery mixed, or would one word score it? A subject that
  cannot do the underlying task produces ablations that move nothing, and a battery of "no" claims
  against outcomes that were all "no" scores beautifully while measuring nothing whatever.
  Unmet checks print above the scores.
- `Scores::interval`, a 95% Wilson interval, and `Scores::p_value`, an exact one-sided binomial
  tail against the majority baseline. Wilson because the counts here are small and near the ends:
  4 out of 4 has a normal interval of zero width, which is a claim of certainty from four
  observations. The p-value staying large over four claims is the honest result rather than a
  defect - it is what stops 75% from being reported as a finding.

### notes

- Every counterfactual in `suite` asks about *two copies* - one with the context as it stands, one
  with the intervention applied - rather than about "your answer". Found by a real model, not by
  reasoning: `gemini-3.7-flash` answered a dossier correctly while a copy of the identical context
  followed the false note planted in it, so the claim in between was being graded against a
  baseline the subject had never been shown. The measurement was right and the question was wrong.
  Every record now carries a line saying whether the session and a copy of it answered the same
  way, because that is the caveat the rest of the figures are read under.

- The suite at its defaults is about sixty requests, one copy per condition. That is enough to
  produce every figure in a report and not enough for `Change::instability`, which needs at least
  two copies and is reported as zero without them.
- Every figure is rounded to six decimal places. A Brier score to seventeen significant figures
  over four claims is a claim about precision the sample size does not support - and `serde_json`
  does not parse floats back to the bit pattern it wrote unless it is built to, so a record with
  seventeen digits in it is a record that cannot be re-read.
