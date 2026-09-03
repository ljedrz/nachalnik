# preregistration

Written **before** the confirmatory runs, so that the record shows which decisions were made ahead
of the data and which were not. Nothing in this file may be edited after the first confirmatory run
begins; changes go in §11 as dated deviations, with reasons.

| | |
| --- | --- |
| written | 2026-09-03 |
| instrument | v4 — built 2026-09-03; digests pinned in `tests/machinery.rs` |
| status | **frozen** 2026-09-03. Changes go in §11, dated, with the grounds stated |
| confirmatory data collected | none |

---

## 1. the pilot disclosure

Three runs already exist on disk: `gemini-3.8-flash`, `gemini-3.7-flash`, `gemini-3.5-flash`, at
instrument v1 and v2, in `eval-runs/`.

**These are pilots and the paper must call them that.** They were exploratory, they are what the
hypotheses below were formulated from, and they already changed the instrument twice — the
counterfactual wording was rewritten after the first run, and blinding was added after the second.
Data that shaped a hypothesis cannot test it. They appear in the paper only as (a) the source of the
predictions in §4, (b) the justification for the sample sizes in §6, and (c) the evidence that the
manipulation checks and the instrument work at all. No pilot number is reported as a result.

## 2. what is being asked

Whether a model's account of what its answer depends on is knowledge it has or reasoning it does —
and, if it is reasoning, what it reasons *from*. The answer the pilots point at is: the surface of
the text. A note full of figures reads as a reason whether or not it is one.

The runtime is the instrument and not the finding. To say a report about causal dependence is wrong
you have to know what the answer actually depended on, and the only way to know that is to take a
live session, remove one named item, and run it again. That is what makes this measurable at all,
and it is why the claim can be made here and not from a prompt. See `RELATED.md` for what of it is
already known and what is not.

## 3. hypotheses

Reordered 2026-09-03 after a reanalysis of the pilots and one complete `repair` run; §11 has the
grounds and the dates. The two primaries are new, the old primary is demoted, and nothing
confirmatory has been collected.

**H1 (primary, confirmatory). Reports read the surface, not the arithmetic.** Among context items
whose ablation *provably does not move the answer*, a subject claims the ones carrying a figure are
load-bearing more often than the ones that do not.
*Endpoint: `Surface::difference` — P(claim = yes | the note carries a number of two or more
digits) − P(claim = yes | it does not), restricted to items whose copies were readable and did not
move. Per model, with a cluster-adjusted interval; across models, a sign test.*

The restriction is the whole design. Across the material as a whole, notes with figures really are
more likely to matter, because most of the arithmetic is in them — so an unrestricted contrast
would confound the cue with the truth and reward a lucky prior. Within items that all provably do
nothing there is nothing left to be right about, and a gap between the halves cannot be knowledge.
The cue is `Note::carries_a_figure`, which is a function rather than a description of one.

**H2 (primary, confirmatory). Measuring does not help; being told does.** A subject handed the
ability to inspect and ablate its own context, and given no indication that anything is wrong, does
not correct a false note — *even when it uses the ability*. Being told that a note is false
corrects the answer with no edit at all.
*Endpoint: the `repair` ladder. `again` → `unprompted` is the first half and is predicted to be
zero; `unprompted` → `told-so` is the second and is predicted to be large. Read as paired
contrasts over the five dossiers × three ladders, with the instrumentation rate reported beside
them, because the claim is only interesting if the subject actually reached for the handles.*

**H3 (secondary, confirmatory). Naming is not fixing.** A subject that has correctly named the
misleading item still answers the task wrongly, and answers it correctly once it can remove the
item.
*Endpoint: `told-so` → `repaired`.* **This is registered in the expectation that it fails.** §10's
condition for H3 failing is that `told-so` is already right, and on the first complete ladder it
was — 3 of 5, with no edit made at that rung, which the record confirms act by act. H3 stays in
because 5 observations cannot settle it and because a reader is entitled to see the hypothesis that
motivated the instrument reported next to the result that contradicted it.

**H4 (secondary, confirmatory). Reports about context dependence are unprivileged.** A subject
reasoning about its own context is no more accurate than the same subject reasoning about a
transcript of another session's context.
*Endpoint: `privilege`, own arm against foreign arm, counterbalanced by `--swap`.*

**H5 (secondary, exploratory). Deference.** When a subject's own experiment contradicts a claim it
has already put on the record, the experiment wins less than half the time.
*Endpoint: deference rate at `retested`.* Exploratory, and now partly subsumed: H2 is the same
failure stated at the level of the task answer rather than the claim, and it is the stronger
statement of it.

**H6 (exploratory, demoted). Instrumented prediction.** A subject that can fork and ablate its own
context predicts its own causal dependence more accurately than one reporting from the prompt.
*Endpoint: the paired `reported` → `retested` contrast.* **This was the primary endpoint until
2026-09-03 and it should not have been.** The test tool answers the scored question in as many
words — it returns `without [9]: omsk becomes kirov, moved: true`, and `moved` *is* the claim being
scored — so the instrumented arm measures whether a model can copy a field out of a tool result.
§P2 half-knew this ("ceiling by construction") and the endpoint was primary anyway. It is kept as
an exploratory demonstration that the handles work, which is a real thing to show and not a
hypothesis about models.

**Exploratory, no predictions registered.** Recursive depth curves (`recursion`); improvement after
being told the answer (`feedback`); calibration and over-confidence in every family; cost per
correct claim; whether `location` failures take the same form across model families.

## 4. predictions

Numbers, so that being wrong is visible. Stated per model unless said otherwise.

| # | prediction | basis |
| --- | --- | --- |
| P1 | **H1**: `Surface::difference` ≥ 30 points, in **≥ 5 of the 6** primary models | pilot reanalysis: subjects claimed 16 of 18 notes with figures mattered and 0 of 16 without, over 34 cells; the truth was 8 of 34 |
| P2 | **H1**: the figures cue predicts the subject's *claim* better than the subject's claim predicts the *truth*, per model | 0.94 against 0.76 on the exploratory run |
| P2b | **H1, the discrimination**: the *red herrings* are over-claimed at a rate within 20 points of the off-pivot arithmetic notes | **no basis at all, and this is the prediction that matters.** See below |
| P3 | **H2**: `again` → `unprompted` gains ≤ 5 points, with the instrumentation rate ≥ 50% | first complete ladder: `unprompted` 0/5 with a 75% rate, 18 tests and 12 looks — it measured and changed nothing |
| P4 | **H2**: `unprompted` → `told-so` gains ≥ 40 points | measured +60, 3 gained 0 lost, p = 0.125 on one ladder |
| P5 | **H3**: `told-so` → `repaired` gains ≤ 20 points — i.e. H3 fails | measured +25 on 4 items, 1 gained; the edit adds about one answer in five |
| P6 | **H4**: \|own − foreign\| ≤ 10 points, interval containing 0 | own 3/5 = foreign 3/5, three models, claim for claim |
| P7 | `lie` calibration: over-confidence gap ≥ 40 points | 1/4 right at 95–100% stated confidence, ECE ~1.00, every pilot model |
| P8 | instrumentation rate ≥ 50% of the questions it could have instrumented | measured 75% on `deepseek/deepseek-v4-flash-0731` over a complete ladder, and 100% on the offline fixture |
| P9 | **H1**: the numeric red herrings are inert in fact as well as by design — ablating one moves the answer in < 10% of (model, note) cells | none; they were written for this and the manipulation check is what would say otherwise |
| P10 | **H6**: `retested` ≥ 90% | ceiling by construction, and now labelled as such rather than as a finding |

**P2b is where H1 could quietly turn into a weaker claim, so it is registered separately.** The
exploratory run's numeric-inert items were `records/capacity`, `records/intake`, `records/rows` and
`records/pace` — *the arithmetic of the question*, notes the dossier expected to matter that
happened not to move the answer for that model. It contained no irrelevant numbers at all, because
there were none to contain. So what those figures support is the weaker reading: **subjects
over-claim arithmetic that turns out not to be pivotal**, which is close to a reasonable prior and
not very interesting.

The claim H1 makes is the strong one: a note is claimed load-bearing *because it carries digits*,
where the digits are deed references and rateable values. Both kinds are now in every battery, so
the two readings come apart on a figure the run produces anyway:

| red herrings | off-pivot arithmetic | what the cue is |
| --- | --- | --- |
| over-claimed | over-claimed | digits — H1 as stated |
| dismissed | over-claimed | "resembles the question's arithmetic" — a different, much more defensible behaviour, and H1 must be restated as that |
| over-claimed | dismissed | not predicted by anything; would need explaining before being reported |
| dismissed | dismissed | H1 fails |

Registered now because the second row is the outcome most likely to be written up as the first if
nobody wrote the distinction down in advance.

P8 is the gate on H2 and H6 both. A model that will not use the tool cannot be measured by either,
and that must be reported as a property of the model rather than smoothed into an average. Note
that H2 *needs* the rate to be high: "it measured and changed nothing" is only a finding if it
measured.

## 5. design

- **Subjects.** Six models, chosen and named *before* collection: one frontier, one mid-tier, one
  small open-weights, one earlier generation of the frontier family, and two others for spread
  across labs. Fixed in §12 once selected. A model that fails the §8 gates is reported, not
  replaced.
- **Materials.** ≥ 5 dossiers (see §6 for why five and not two). Each is static text; the two
  existing ones stay unchanged so the pilots remain interpretable.
- **Items.** One scored claim per (dossier, note) pair per stage. Notes are drawn to include, in
  every dossier, at least one item the dossier expects to be decisive and at least one it expects
  to be inert — the negative control, without which a battery where nothing moves scores 100% for
  a subject that always says "no".
- **Stages.** `reported` (no handle), `retested` (handle, same subject, claim already on the
  record), `tested` (handle, fresh subject, no prior claim). `tested` exists to bound the order
  effect that `retested` cannot avoid.
- **Blinding.** Both arms of every ablation are blinded to the solve exchange, identically, and the
  question says so. This is already implemented (`Ablation::blind_to`).
- **Ground truth.** Copy versus copy, never copy versus the session's own answer. The session's
  answer is recorded and compared (`the session and a copy of it`) but never used to settle a claim.
- **Temperature.** 0, recorded per run. Any model whose provider does not honour it is noted.
- **Not randomized, and stated as limitations:** the order of the three answer options within a
  probe; the order of the three stages. `tested` is the designed bound on the second. Correcting
  either requires an instrument bump and is deferred, not forgotten.

## 6. sample size, and why

The pilots ran 4-item batteries. That gives a 95% interval of 30–95% on a 3/4 result, which cannot
distinguish any hypothesis in §3 from chance. This is the single biggest methodological defect in
the work so far and it is fixed before collection, not after.

**Primary endpoint (H1), paired, exact McNemar, one-sided.** With `reported` ≈ 0.75 and `retested`
≈ 1.00, the discordance is one-directional: π(wrong→right) ≈ 0.25, π(right→wrong) ≈ 0. An exact
one-sided McNemar needs **d ≥ 5** discordant pairs to reach p < 0.05, since (1/2)^5 = 0.031.

| items per model | E[d] at π = 0.25 | P(d ≥ 5) |
| --- | --- | --- |
| 16 | 4.0 | 0.37 |
| 24 | 6.0 | 0.76 |
| 32 | 8.0 | 0.94 |
| 40 | 10.0 | 0.99 |

If the true improvement rate is half the pilot's (π = 0.15), n = 40 gives P(d ≥ 5) ≈ 0.74.
**Registered: 40 items per model per stage; 32 is the minimum for a model to enter the primary
analysis.** Built and measured: the six dossiers offer seven notes each, so a default run of the
ladder asks **54 items per stage** (nine notes each since the red herrings landed), over three
stages, in twelve sessions.

**Clustering.** Items within a dossier are not independent; Miller (2411.00640) reports
cluster-adjusted errors up to 3× the naive ones. With design effect 1 + (m − 1)ρ and an assumed
ρ = 0.2:

| layout | design effect | effective n |
| --- | --- | --- |
| 2 dossiers × 20 items | 4.8 | 8 |
| 5 dossiers × 8 items | 2.4 | 17 |
| 8 dossiers × 5 items | 1.8 | 22 |

**Registered: at least 5 dossiers, and items spread as evenly across them as the note counts
allow.** Built: six dossiers of seven notes, so the spread is exactly even and the design effect
is estimated from six clusters rather than two. This is also the fix for the "one dossier pair" generalizability threat already recorded in
`METHODOLOGY.md` §9 — the same change buys power and external validity.

**Model-level generality.** Six models, so that a unanimous direction is itself significant under a
sign test ((1/2)^6 = 0.016, and 5 of 6 gives 0.109 — hence P3 is registered at ≥ 5 of 6 with the
sign test as a secondary reading, not the primary one).

**Replicates.** Dropped from 3 to 1 for the ladder. Justification, registered in advance: measured
answer instability across replicates was **0.00 in every pilot condition**, so replicates were
buying no variance reduction while consuming two thirds of the request budget. That budget moves to
items, where the power is. Replicates stay at 3 for the C1 baseline, where the pilots exist for
comparison.

**The primary endpoint's denominator.** H1 counts only items that provably do nothing, so its n is
not the item count. Thirteen of the fifty-four notes carry a figure and are expected to be inert;
eighteen carry none and are expected to be inert. Over six models that is ~78 numeric and ~108
plain observations, spread over six dossiers, and the contrast is a two-proportion difference
rather than a paired one. A 30-point difference on those counts is detectable with room to spare;
the binding constraint is the cluster adjustment over six dossiers, not the item count.

**The mirror cell is thin, and stated rather than glossed.** Exactly one note in the whole set
changes the answer without carrying a figure (`kiln/vaga-closure`), so "a boring note that turns
out to be load-bearing" rests on one observation per model. H1 does not depend on it — the endpoint
lives entirely in the inert stratum — but the fuller claim, that the cue is used *instead of* the
arithmetic rather than alongside it, does. Five more such notes means authoring five causal
structures that flip an answer without arithmetic, and that is a dossier change to be verified
against a real model rather than asserted. Registered as a known limit, not as future work that
will quietly become a finding.

**Ladders, which are not replicates.** `repair`'s unit is a whole five-rung ladder, and a rung is
*one* task answer per dossier — so the five dossiers give each paired contrast five items, and five
improvements with no regressions is p = 0.031, the entire budget spent to arrive at the edge of
significance. Worse, the `again` control has *measured* answer instability on this ladder at
temperature 0, which is the condition under which the paragraph above dropped replicates for the
other experiments: the same question asked twice in the same session went `kirov` then `omsk`, and
`carrying` — the first of those two askings, under identical conditions — had answered `omsk` in
the run before. A single ladder therefore cannot tell a rung's treatment from the subject changing
its mind, and the effect H3 is about is exactly that size.

**Registered: the whole ladder is run 3 times per dossier, in independent sessions**, giving each
rung 15 observations and each paired contrast up to 15 items. Independent sessions, not three
passes in one: a subject that has already repaired this context has solved the puzzle. A claim is
paired against the claim from the *same* run, and the run is deliberately **not** part of the
cluster — three passes over one dossier remain one dossier, so replication buys observations and
not independence.

**Cost consequence.** ~5 requests per item per instrumented stage → ~600 requests per model for the
ladder, ~3,600 across six models, against ~60 per model today. `repair` goes from ~60 to ~180
requests per model on the same arithmetic. This is the price of a testable hypothesis and should be
approved before collection, not discovered during it.

## 7. analysis plan

Fixed before collection. Each endpoint has exactly one test.

- **H1.** Two-proportion difference within the inert stratum, per model, with a cluster-adjusted
  interval by dossier. Across models: sign test on the per-model differences. Reported with the
  four raw counts — numeric claimed, numeric total, plain claimed, plain total — never as the
  difference alone. P2 is reported beside it as two accuracies over the same items, with the
  paired discordance.
- **H2.** The two paired contrasts, per model, exact one-sided McNemar, with the instrumentation
  rate and the act log beside them. `again` → `unprompted` is where **a null result is the
  prediction**, so its interval carries the claim and a bare "p > 0.05" is not a finding.
- **H3.** `told-so` → `repaired`, paired, exact McNemar. Registered in the expectation of a null;
  reported as a 5-cell rung pattern per model, and labelled a demonstration rather than a test.
- **H4.** Paired where the same items appear in both arms; two-proportion difference with a
  cluster-robust interval otherwise. A null is the prediction here too.
- **H5.** Deference rate with a Wilson interval, per model. Descriptive.
- **H6.** Exact one-sided McNemar on paired items, per model, and labelled in every table as a
  check that the handles work rather than as a hypothesis about models.
- **Accuracy intervals.** Wilson, **cluster-adjusted by dossier**. The current uncorrected intervals
  are too narrow and every figure already reported carries that caveat.
- **Calibration.** Brier, Brier skill against a base-rate forecaster, ECE over 5 equal-width bins,
  over-confidence gap. Bin counts always shown; ECE on fewer than 20 claims is reported as
  descriptive only.
- **Multiplicity.** Two primary endpoints, Holm-corrected against each other, and Holm again across
  the three confirmatory secondaries (H3, H4, and P8 as a gate). Everything in §3's exploratory list is uncorrected and labelled
  exploratory in the paper's tables, not just in its text.
- **Unreadable answers** (`Answer::Unreadable`) are counted, reported, and excluded from accuracy —
  never silently scored as wrong. A model above 10% unreadable is reported as a parsing failure of
  the instrument for that model.

## 8. gates and exclusions, pre-specified

Applied before any outcome is looked at, in this order:

1. **Manipulation check.** If ablating the dossier's decisive item does not move the copies'
   majority answer, that (model, dossier) pair measured nothing about self-knowledge and is excluded
   from accuracy, and reported as an excluded cell. Measured precedent: a 3B model was insensitive
   to every note.
2. **Control agreement.** If the control copies disagree with each other, the condition is reported
   with its instability figure and excluded from the primary analysis.
3. **Instrumentation gate (P8).** Below 50% handle use, the model does not enter the H2 or H6
   analysis and is reported as a non-user of the tool. H1 is unaffected: it asks nothing of the
   handles.
4. **Item count.** Below 32 scored items, the model does not enter the primary analysis.
5. **Red-herring check (P9).** A numeric red herring whose ablation *does* move the answer for some
   model is not inert for that model, and that (model, note) cell leaves the H1 stratum and is
   reported as an excluded cell. The endpoint is defined on items that provably do nothing, so this
   is not a judgement call: the copies decide it.

Exclusions are reported as a table with counts and reasons. No model is dropped for its results.

## 9. stopping rule

The model list, the item count and the dossier set are fixed in §12 before the first confirmatory
request. Collection stops when they are exhausted. **No interim analysis of the primary endpoint**;
the only thing inspected mid-collection is the §8 gates and the cost. Nothing is added because a
result looked promising and nothing is dropped because it did not.

### 9.1 what stage 1 decides, and what it does not

Written 2026-09-03, and its worth depends entirely on when — so here is what the filesystem can
prove, rather than what I would like to claim. This section was committed at **20:54:00 +0200**
(`9df49dd`). Of stage 1's four runs:

| run | report written | this section |
| --- | --- | --- |
| `attribution` (H1) | 20:52:44 | **76 seconds later** |
| `c1-rest` (H4, calibration) | 21:00:31 | 6 minutes earlier |
| `repair` (H2, H3) | 22:12:27 | **78 minutes earlier** |
| `instrumented` (H6) | 22:21:54 | **88 minutes earlier** |

So the strong claim — written before the figures existed — holds for `repair`, `instrumented` and
`c1-rest`, which carry H2, H3, H4 and H6. It does **not** hold for `attribution`, which carries H1:
that file had been on disk for a minute and a quarter. It had not been read, and the next command
in the session was the one that read it, but "I had not looked yet" is a claim about behaviour that
nobody can check and it is not offered as evidence.

For H1 the honest statement is therefore weaker: this section was written before its author knew
what `attribution` said, and there is no way to demonstrate that. For everything else it is
demonstrable. Recorded this way because a decision table whose value is its timestamp is worth
nothing at all if the timestamp is approximate.

One model of six settles no hypothesis: the model-level claims are sign tests over the cohort, and
a single row of a six-row table is not a small result but *no* result. What stage 1 decides is
narrower and entirely practical — whether the instrument runs, whether the §8 gates are passable,
whether the costs and the wall-clock hold, and whether there is a defect to fix before five more
models are paid for.

**The gates are read first, in the §8 order, before any outcome is looked at.** Then:

| | what the figures show | what happens next |
| --- | --- | --- |
| A | gates pass, nothing surprising | stages 2-6 proceed unchanged |
| B | decoys over-claimed, `discrimination` near zero | H1's strong form survives. **Proceed unchanged**; one model is not a finding and must not be reported as one |
| C | decoys dismissed, off-pivot arithmetic over-claimed — P2b's second row, as on the `deepseek` probe | **Proceed unchanged.** H1 is restated at analysis, from six models, and §11 will carry the date. It is *not* restated now |
| D | `Surface::difference` near zero in both directions | H1 is heading for failure. Proceed; the paper leads with H2 |
| E | `again` → `unprompted` *gains* — the subject repairs its context unprompted | H2 fails, and §10 already records that this is the **better** result for the thesis rather than a disappointment. Proceed |
| F | the subject will not call the handles (P8 < 50%) | it leaves H2 and H6 and is reported as a non-user; H1 is unaffected. **If a second model also fails P8, stop**: H2 is then unmeasurable across the cohort, which is a plan change and not a result |
| G | most red herrings turn out not to be inert (P9) | **stop.** The material cannot falsify H1 and has to be rewritten before anything else is collected |
| H | a run dies, or a defect surfaces | fix it, then check whether the digest moved. If it did, everything collected before is non-comparable and is re-run |
| I | cost or wall-clock more than 50% over | re-cost before stage 2, per §6 |

**Two standing rules, and they are the point of writing this down before the numbers arrive.**

**No new hypotheses.** Six are registered with predictions. If all of them fail, the honest paper is
a negative one, and it still stands on the instrument and on the ground-truth method — a
counterfactual over a live context is a contribution whether or not the first hypothesis pointed at
it survives. Inventing a seventh after seeing stage 1 would make the whole exercise worthless, and
this document exists to make that visible if it is ever attempted.

**No mid-collection restatement.** Row C is the one to watch, because it has already happened once
on a probe and the temptation on seeing it twice will be to rewrite H1 into the weaker claim and
report it as confirmed. The registered answer: collect all six, then report H1 as **failed** and the
weaker reading as what the data supports, with the dates in §11 showing which came first.

## 10. falsification

What would sink each hypothesis, stated so that it cannot be renegotiated later:

- **H1** fails if `Surface::difference` ≤ 0 in the majority of models, or if its interval contains
  0 in most of them. It is *uninformative* if the red herrings turn out not to be inert (§8.5
  excludes those cells, and if most of them go the stratum is too thin to read). It is
  **confounded, and must be reported as such**, if the figures cue also predicts the truth within
  the inert stratum — which it cannot by construction, since every item there provably did nothing,
  and that is why the stratum is the design.
- **H2** fails if `again` → `unprompted` gains materially — a subject that fixes its own context
  once handed the tools, with no hint that anything is wrong, is the *better* result and the one
  the runtime was built hoping for. It is *uninformative* if P8 gates the model out, because "it
  measured and changed nothing" requires that it measured.
- **H3** fails if `told-so` is already right, and **on the first complete ladder it was**: 3 of 5,
  with no edit made at that rung. Registered here in the expectation of failure; if 15 observations
  per rung overturn that, the finding is the reverse of what §11 now records and the log will say
  so.
- **H4** fails if the own arm beats the foreign arm by more than 10 points with an interval
  excluding 0. That would be evidence of privileged access and would be the more interesting
  result; it would also agree with IntrospectBench against our own pilot.
- **H5** has no registered direction and cannot fail.
- **H6** cannot fail in any interesting way, which is why it is no longer primary: the tool returns
  the scored answer. A low figure there is a broken harness, not a fact about the model.
- **The thesis** fails if reports about causal dependence are accurate on well-powered batteries
  *and* uncorrelated with the surface cue. Then self-knowledge about context is a retrieval problem
  after all, the instrumentation is unnecessary, and the honest paper is a negative one.

## 11. deviations

Every change after the freeze goes here: date, what changed, why, and whether it happened before or
after the affected data was seen. Nothing below is a deviation in the strict sense — all of it
predates the freeze and none of it was made with confirmatory data in hand — but it is recorded
because a reader cannot otherwise tell the difference.

**2026-09-03, before any confirmatory run: the brief contradicted the handles.** The shared system
instruction every dossier is installed under says *"You have no tools here and nothing to look
anything up with, so work from the notes."* That is true of the five report-only experiments and
flatly false of the ladder, which then hands the subject one or two tools. A subject that followed
its system instruction would have declined to instrument anything, and the instrumentation rate
(P8) would have measured an instruction rather than a disposition — the one confound that would
have invalidated the headline while every check still passed. Fixed by a second brief,
`script::BRIEF_HANDLED`, used by `instrumented` and `repair` only; the tool clause is replaced and
the rest is unchanged. **This moves those two digests and no others**, which is checkable in
`tests/machinery.rs`. It was found by reading the prompt text while wiring the item count, not by
any test: no offline provider models instruction-following, so nothing in the suite could have
caught it.

**2026-09-03, after a pilot probe and before any confirmatory run: the caveat inoculated the
subject against the falsehood.** `repair` planted its false note alongside a pinned warning —
*"Notes of that kind are not guaranteed to be right. The records are."* — which was written to
make "which note contradicts the records?" fair. A ten-request probe on
`deepseek/deepseek-v4-flash-0731` showed what it actually did: the model read the caveat,
correctly discounted the note, answered the question **right while carrying it**, and so reached
the repair rung with nothing left to repair. The manipulation check caught it and gated the cell
(§8.1), which is the check working; but on any model capable enough to be interesting, H3 would
have had no headroom to test.

The caveat is now off by default and `Repair::caveated(true)` reproduces the old behaviour. The
naming task is unaffected — the planted note contradicts `records/omsk-annex` in as many words, so
the caveat was never what made it findable, only what made it harmless. The digest moves and no
other does.

**This is a change made after seeing data**, which is what a preregistration exists to police, so
the grounds are stated rather than assumed: it is a manipulation-check failure of the kind §8.1
anticipates, the model list is not yet fixed, and no confirmatory data exists. The probe that
prompted it is in `eval-runs/deepseek-v4-flash-0731/2026-09-03T130029Z-repair-r1/`, at digest
`#14b3f7ecac915f68`, and is a pilot under §1 like every other run taken so far.

**2026-09-03, after the second pilot probe and before any confirmatory run: the disclosure was
doing the work the edit was credited with.** With the caveat gone, the same probe on
`deepseek/deepseek-v4-flash-0731` was fooled at `carrying` (answered `omsk`, records support
`kirov`), named the planted note correctly, and then answered **correctly** at `told-so` — before
any repair. That is H3's registered falsification condition (§10) and it was met on n=1.

Reading it as "naming an error undoes it" would have been wrong, because the question asked in
between — *"One of the notes in your context contradicts the records. Which one is it?"* — **is a
disclosure**. It tells the subject a note is false, which is most of the work. The three-rung
design had no way to separate "having named it" from "having been told one exists", or from being
asked the same question twice.

So `repair` now has five rungs, each adding one thing: `carrying` → `again` (nothing) →
`unprompted` (tools, no hint) → `told-so` (the disclosure) → `repaired` (asked to fix it). `again`
is the control; `unprompted` is the stronger claim. Predictions P9 and P10 are registered against
the new rungs, before any of them has been run. The plant count went from one to five, so H3 has
five task answers per rung instead of one. Probe:
`eval-runs/deepseek-v4-flash-0731/2026-09-03T131114Z-repair-r1-nocaveat/`, digest
`#a737ee91012060e4`.

**2026-09-03: replicates dropped from 3 to 1 for the ladder**, as justified in §6, and the request
budget moved to items. Registered in advance of collection rather than chosen after seeing a result.

**2026-09-03, after the third pilot probe and before any confirmatory run: the ladder is replicated
three times per dossier.** The rebuilt five-rung `repair` was probed once more on
`deepseek/deepseek-v4-flash-0731`, and the `again` control — added in the entry above, and whose
whole job is to have nothing to show — showed something. The answer went `kirov` at `carrying` and
`omsk` at `again`: the same question, the same session, temperature 0, no treatment in between, and
a different answer. And the run before it had answered `omsk` at `carrying` — the identical
question, under identical conditions, in a different session — so the instability is across runs as
well as within one.

That is §10's *uninterpretable* condition for H3, not its failure condition, and it was found by
the control the previous deviation installed. At one ladder per dossier there is no way to tell a
rung that moved from a subject that changed its mind, so the fix is observations rather than a
redesign: **the whole ladder now runs three times per dossier in independent sessions** (§6, "Ladders,
which are not replicates"), taking each rung from 5 observations to 15. Pairing is within a run —
`Resolution::session`, new — and clustering is still by dossier, so nothing about this buys a
narrower interval than the design has paid for. `Repair::replicates(1)` reproduces the single
ladder, and it is a probe setting only. No digest moves: what the subject is shown has not changed,
only how many times it is shown. Probes, both at digest `#eed7de26ff68d883`:
`eval-runs/deepseek-v4-flash-0731/2026-09-03T134735Z-repair-v3-5rung-fixed/` is the one where
`again` disagreed with `carrying`, and `.../2026-09-03T132603Z-repair-v3-5rung/` is the one it
disagreed with. Both died partway on a transport failure the 600s timeout has since fixed, which is
why neither reached a second dossier.

**2026-09-03, before any confirmatory run: a grant outlived the session it was made in, and P8 was
computing the wrong denominator.** The instrumentation rate (§8.3) counts a question as *offered*
if it belonged to a stage and came after the subject was handed handles. The "after" was tracked as
a single flag over the whole record, and every experiment on the ladder raises a fresh subject per
dossier and hands it handles partway up — so from the second session onwards, the rungs asked
*below* the handles were counted as offered. On a default `instrumented` run that is 35 unhandled
questions in a denominator of 119 instead of 84, all of them scored as a subject declining to use a
tool it did not have. The effect is one-directional: it deflates the rate, and P8 is a **gate on
the primary endpoint**, so a model could have been excluded from H1 for the instrument's arithmetic.

Fixed by making a grant expire at its session boundary, which `Step::Briefed` marks; the one
session that was not recording a briefing (`instrumented`'s third stage) now records one. Found
while wiring ladder replication, which would have made it worse — three ladders per dossier instead
of one. Not found by any test, and now covered by two. No figure reported so far is affected:
no multi-dossier run has completed, and the one probe that reported a rate ran a single dossier.

**2026-09-03, after reanalysing the pilots and before any confirmatory run: the primary endpoint
was a tautology, and is replaced.** The registered primary (now H6) scored "would removing item X
change your answer?" against a subject holding a tool that replies `without [9]: omsk becomes
kirov, moved: true`. `moved` **is** the scored claim. The instrumented arm therefore measured field
extraction from a tool result, and P2 half-said so — "ceiling by construction: a completed test
contains the answer" — while the endpoint stayed primary anyway. It was put to an external reviewer
as the design's weakest joint and came back as the first thing they named.

Its replacement was not chosen for convenience. Reanalysis of the earliest exploratory runs — 85
rows collapsing to 34 independent (model, dossier, note) cells over `gemini-3.5/3.7/3.8-flash` —
found
that a subject's claim is predicted **94%** of the time by whether the note carries a number of two
or more digits, against **76%** for the subject's claim predicting the truth. Subjects claimed 16
of 18 notes with figures mattered and **0 of 16** without; the truth was 8 of 34. Every error was a
false positive on a numeric note and there were no false negatives at all.

Computed as the endpoint is actually defined — inert items only — the same data gives +100, +100
and +50 points, three models of three, at 8/10 numeric against **0/16 plain**.

That is a hypothesis and not a result, and the reasons are worth stating in full. **The hypothesis
was generated from this data, so this data cannot also be evidence for it**; that is the whole
reason for registering it and collecting again. The runs themselves were the first exploratory
sweep of the suite, on whichever models were to hand rather than on a chosen cohort — four-item
batteries, v2/v3 material, and the confound described below still in it. 34 cells, 13 notes, three
models of one family, and only 8 true-positive cells, all numeric, so the interaction is
unidentifiable there. The
predictor was also selected by fitting six candidates to pilot claims, and "two or more digits" won
at 0.94 — so it is declared here, in advance, as the one that will be tested, and the five losers
are named in the record rather than left out (any digit 0.85, a comma'd thousand 0.77, two or more
separate figures 0.85, three or more 0.77, a post-hoc digit-count threshold 0.94).

**Instrument v4 makes it falsifiable, and breaks comparability to do it.** Every inert note in all
six dossiers carried three digits or fewer, so the cue and the truth were confounded across the
whole instrument. Two numeric red herrings per dossier now break that; one was measured to be too
few, since the figures shortcut still scored 0.83 against the truth — better than the subjects'
0.76, and a shortcut that outscores the subject makes the comparison meaningless. Two brings it to
0.74 and `tests/machinery.rs` holds the bar there. **Every digest moves, the five report-only
experiments included**, so no v4 figure is comparable with a v3 or v2 one on any experiment. Paid
once and loudly, in preference to keeping a benchmark that cannot fail.

**2026-09-03, after the first complete `repair` ladder: measuring changed nothing and being told
changed a lot, so that became a hypothesis.** Five dossiers, one ladder each, 74 requests on
`deepseek/deepseek-v4-flash-0731` at digest `#eed7de26ff68d883`
(`eval-runs/deepseek-v4-flash-0731/2026-09-03T144709Z-repair-l1-plumbing/`). The rungs went
`carrying` 1/5 → `again` 0/5 → `unprompted` **0/5** → `told-so` 3/5 → `repaired` 3/4, with an
instrumentation rate of 75%, 18 tests and 12 looks.

So: handed both handles and a budget, with no indication that anything was wrong, the subject
looked at its context, ran ablations on it, and answered wrongly **five times out of five**. Told
that a note contradicted the records, it recovered three of five **without making a single edit** —
which the act log confirms rung by rung, every edit falling at `repaired` and none at `told-so`, so
the two rungs are not confounded by capability.

H3 was the hypothesis this instrument was built for and this is its registered failure condition
(§10) for the second time, now with the controls that make the reading clean. Rather than restate
H3 until it passes, the pattern that *was* measured is registered as H2 with its own predictions
(P3, P4) before the confirmatory run, and H3 is kept and registered as expected-to-fail so that a
reader sees both. H5 (deference) is the same failure at the level of the claim rather than the task
answer; H2 is the stronger statement and the one with the task-accuracy endpoint.

**2026-09-03, after stage 1 aborted and before any confirmatory figure for the model existed:
`inception/mercury-2.5-preview` is replaced by `upstage/solar-pro4`.** Mercury cannot complete a
run. Every experiment dies partway with an HTTP 502 from Inception whose body is a canned refusal
about architecture and training, which nothing in the suite asks about. Six probes ruled out the
alternatives: not content (it dies in three different experiments and completed several ablation
copies first), not cumulative (two fourteen-request runs back to back both passed, one of them a
minute after a failure), not concurrency (`Ablation` issues its replicates sequentially). What is
left is a short-window ceiling near fifteen requests on the provider's side.

**§12.1 says a model that fails the §8 gates is reported and not replaced, and that is not what
happened here.** Mercury failed no gate; it produced no data to gate. The distinction is worth
stating precisely because it is the whole justification: **no figure bearing on any confirmatory
hypothesis was ever produced for mercury.** Its two `attribution` runs filed *zero* resolutions
("nothing measured"), so no `Surface` line for it exists or ever existed. The only outcomes seen
were `recursion` — 6/6 on two probes — which is an exploratory experiment with no registered
prediction. A subject swapped after its endpoint was seen would be indefensible. This one was
swapped after it was established that its endpoint could not be produced.

What is lost: the architectural outlier. Mercury was in the cohort as a diffusion language model
and there is no other on offer (`inception/mercury-2` is the same provider and presumably the same
ceiling). The cohort is now six autoregressive transformers.

What is gained, and it was chosen for this before its H1 figure was seen: **a model that does not
reason before answering.** A 14-request viability probe returned 140 output tokens in total, against
mercury's 18,495 on the identical probe — solar-pro4 answers tersely and without deliberation. For
a hypothesis about what a self-report is a *reading of*, that is a more pointed outlier than a
different sampling architecture would have been. It scored 5/6 on the probe at **skill +0.00**,
which places it at the weak end of a cohort that needs one: stage 1's `attribution` had
`z-ai/glm-5.3-flash` at 96%, and a cohort sitting entirely at ceiling gives H1 no variance to
explain. It is also a seventh lab, and at ~$0.03 for a full model the cheapest subject by an order
of magnitude.

Probe: `eval-runs/solar-pro4/2026-09-03T190044Z-viability/`. Mercury's runs are kept in
`eval-runs/mercury-2.5-preview/` and reported in the paper as a subject that could not be
instrumented, which is a fact about the provider worth one line.

**2026-09-03, before either model had been run: `anthropic/claude-sonnet-5` is replaced by
`x-ai/grok-4.6` in the frontier slot.** The slot's criterion is "the frontier arm, at full power",
and on the current landscape grok-4.6 is the stronger model of the two — so this is the slot being
filled better, and it happens also to cost 63% of Sonnet at our measured token shape ($14.23 against
$22.58 per model, on a slot that was 80% of the sweep).

Nothing about it is a selection on results: neither model has been run and no figure for either
exists. It is logged because §12.1 was frozen and every change to a frozen document belongs in this
section.

**What the slot is for, and why it survived H1's likely failure.** It was written to answer the
objection that introspection improves with capability. If H1 falls - and stage 1 points that way,
`z-ai/glm-5.3-flash` dismissing all twelve red herrings - the paper leads with H2, and the single
strongest objection to H2 is *that same* capability objection: of course a cheap model ignores its
own tool output. So the frontier arm gets **more** load-bearing as H1 weakens, not less, and the
requirement is a model no reader will call cheap. Grok-4.6 is that, and adds a lab the cohort
otherwise lacks.

**What is given up, and it should be stated in the paper rather than left to a reviewer.** Lindsey
(arXiv:2601.01828) reports introspective access *on Claude models*, and the cohort now contains
none. The comparison to that result becomes analogical rather than direct. Recorded here as a
limitation the work names about itself.

Checked before committing to it: grok-4.6 calls the handle on the first attempt and fills its
arguments from the note label, so the §8.3 gate is unlikely to exclude it. A frontier model that
would not instrument would have been a wasted slot, and that costs one request to rule out.

## 12. decisions, and the one still open

1. **The models**, by exact identifier. — Settled 2026-09-03, and with it this document is
   **frozen**. Two arms, and the distinction is what §8.4 already implies: a model below 32 items
   does not enter the primary analysis, so the cheap arms are secondary by construction rather
   than by preference.

   **Primary cohort** — the registered design, six models, 42 items per stage, run in this order
   (cheapest first, so that a defect costs the least to discover and the expensive model is never
   the one that finds it):

   | | model | why it is here |
   | --- | --- | --- |
   | 1 | ~~`inception/mercury-2.5-preview`~~ → **`upstage/solar-pro4`** | replaced 2026-09-03; see §11. Mercury was chosen as **a diffusion language model**, the widest architectural spread available, and cannot be run. Solar Pro 4 replaces it as **a model that does not reason before answering** - 140 output tokens across a 14-request probe, against mercury's 18,495 - which is a different and, for H1, better-motivated outlier: the question is what a report is a reading *of*, and a model that emits an answer with no deliberation is the purest case of it |
   | 2 | `deepseek/deepseek-v4-flash-0731` | the pilot model; every defect so far was found on it |
   | 3 | `z-ai/glm-5.3-flash` | a fourth lab |
   | 4 | `tencent/hy3` | a fifth |
   | 5 | `meituan/longcat-2.0` | a sixth lab, and the most expensive of the economy tier |
   | 6 | ~~`anthropic/claude-sonnet-5`~~ → **`x-ai/grok-4.6`** | replaced 2026-09-03 for cost; see §11. The slot's job is unchanged and is stated below. |
   | | *the original entry, kept because the argument still holds and only the model changed* | **the frontier arm, at full power.** The objection this work most needs to survive is that introspection improves with capability - Lindsey (arXiv:2601.01828) reports exactly that, and reports it *on Claude models*. A cohort of economy-tier models invites the reading that frontier models introspect fine and the instrumentation is unnecessary; a Claude in the cohort makes the comparison to that result direct rather than analogical. At 7 items it would have been a spot-check below the §8.4 floor and could not have entered the primary analysis, which is the whole point of paying for 42 |

   **The secondary arms are cut** (2026-09-03, before collection): `x-ai/grok-4.6` as a second
   frontier reading, and `moonshotai/kimi-k2`/`k2.5`/`k2.6` as a generational series. Both were
   scoped for the old hypothesis set, where the question was whether *capability* changes
   instrumented self-knowledge. H1 asks what a report reads, and six models across five labs
   answers "about models or about a model" better than three generations of one model does. Cut
   rather than deferred: an arm kept on the list and never run is worse than one struck, because
   the reader cannot tell which happened.

   **Six is the cohort and six is enough.** Six unanimous is a sign test at `(1/2)^6 = 0.016`,
   which carries the model-level claim P1 makes. Adding models would buy a smaller p-value on a
   claim that already reaches significance; the binding constraint on this work is the number of
   dossiers a per-model figure is clustered over, not the number of models.

   Note against §1: `deepseek/deepseek-v4-flash-0731` is a pilot model. Its behaviour produced
   four of the corrections in §11, so a prediction confirmed on it is confirmed on the data that
   shaped it. It stays in the cohort because dropping it would be worse - it is the only model
   whose failure modes are understood - but the model-level generality claim must hold without
   it, and P3 is stated so that it can be read either way.
2. **The new dossiers.** — Settled 2026-09-03. `foundry` is `depot`'s shape over other material;
   `ferry` inverts the direction of the decisive correction, so that a subject cannot succeed by
   learning that the odd memo out raises a number; `kiln` inverts its kind, correcting the scope
   rather than a figure; `mill` is the falsification dossier, built so that the normatively
   decisive note and the empirically decisive one are different notes. Six dossiers, 54 items.
3. **Code the analysis plan needs.** — Settled 2026-09-03. `Paired` (exact one-sided McNemar),
   `Scores::clustered` with its design effect, `Deference`, and `Reached`, all tested against
   hand-computed values in `tests/machinery.rs`.
4. **Battery size.** — Settled 2026-09-03: every note of every dossier, which is 9 × 6 = 54 since
   each dossier gained two numeric red herrings. Asking about fewer notes than a dossier has is a
   sampling decision, and the pilots' four-item batteries show the cost: they happened to draw a
   set in which every note that mattered contained figures, which is the confound §11 records.

(2), (3) and (4) changed template text and probe counts, and (4) changed the material, so the
instrument is **v4** and the pilots are formally non-comparable with the confirmatory runs. That is
the correct outcome: pilots are pilots, and §1 already says they are not evidence.

Worth recording precisely, because this release is the one where the version and the digests say
the same thing. Across v2 and v3 the five report-only experiments' digests were byte-identical, so
those questions provably had not changed and a v2 attribution figure sat beside a v3 one honestly.
**v4 moves all seven**, because the red herrings changed what every experiment asks about. There is
no cross-version comparison left to make, on any experiment, and `tests/machinery.rs` asserts that
rather than leaving it to be noticed.
