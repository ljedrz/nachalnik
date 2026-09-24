# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### breaking

- **`Act::Tested` has a `spend` and a `failed`.** Both are `serde(default)`, so a saved report
  reads as it was; an `Act::Tested` written as a struct literal elsewhere - a handle of somebody
  else's pushing onto a `Journal` - needs the two fields.
- **`Outcome::failed` is a `Failure`, which says which error it was.** It was the message alone, so
  a sweep deciding whether a cell was worth running again - a provider that fell over, against a
  subject that ran out of requests - had to read the words. `Failure` holds an `ErrorKind` beside
  the message, `Error::kind` gives one, and `Outcome::of` takes a `Failure`. A report written with
  the message alone still reads, as `ErrorKind::Unknown`, and so does a kind this version has no
  name for.

### changed

- **Only the stages an experiment records as a ladder are paired.** `Outcome::of` paired every two
  stages it found. `conflict`'s three are the subject's own unprompted claim and two sets of
  copies, one of them with the opposite truth, and each pair of them was reported as a paired
  contrast with a McNemar p-value. `Trial::ladder` records `Step::Ladder`, the stages that put the
  same claims to the same respondent again, and `Outcome::paired` is every ordered pair on one.
  `instrumented` and `repair` record theirs, so their contrasts are the ones they had; an
  experiment of somebody else's that wants its stages paired says which.

### fixed

- **What a run cost includes the copies the subject's own tests ran.** `Act::Tested` carried no
  spend and `Trial::spend` summed the subject's requests and the harness's copies, so an
  instrumented run was reported as costing less than it was billed, by every test the subject ran.
- **A test whose copies could not be run is on the record.** Each copy's error returned before
  the journal was written, so a test that came off the subject's budget left no trace, and the
  question it followed read as one the subject declined to instrument. It is an `Act::Tested` with
  nothing moved and `failed` saying why, and the subject is still shown the error.
- **A name `inspect` does not know is not a refusal.** It was journaled as `Act::Refused`, which is
  what a subject was not allowed to have, where `amend` journals nothing for the same mistake; so
  `Reached::refusals` counted a typo in one handle and not in the other. Neither journals it.
- **`privilege` records its other session.** It briefed and asked a second session and put one
  line of it in the record, so the notes that session was given and the answer the foreign arm
  was scored against were not there - and neither was the request, which `Trial::spend` left out.
  The other session now runs first, to the end of its answer, and is recorded as a session: a
  `Briefed` in the middle of the subject's would have filed the subject's questions under it. A
  sibling starts empty, so nobody is asked anything different for the order.
- **A move inside the control's own noise is not read as one.** `Change::as_answer` read `moved`
  alone and `Attribution` ranked notes on divergence whatever the noise, so with replicates a
  commonest answer that flipped on one readable copy in three scored a claim as though the note
  had moved the answer, against a control that disagreed with itself as often. `Change::shown` is
  `moved` where the change clears the noise and nothing where it does not, so such a claim is
  untested; the scored outcomes, the feedback `feedback` gives, `conflict`'s reading of which side
  an answer rested on and the manipulation checks all read it. `moved` stays on the record as what
  the two commonest answers were. At one replicate, the default, every figure is as it was.
- **A tied control agrees as often as each of its commonest answers was given.**
  `Observation::agreement` went through `majority`, which gives a tie to nobody, and answered `0.0`,
  so two copies that said one thing each were recorded with an instability of `1.0` - the same as
  copies that said nothing - and the checks that quote it said they agreed 0% of the time. It is
  `0.5` for them now. A tie still has no commonest answer, so nothing it is compared with moves.
- **An attribution that named no note is scored wrong, and one that was cut off is counted as
  cut.** Both came back unmeasured: a subject that named none of the notes, or several, left the
  denominator, where `lie` and `conflict` score the same refusal as wrong, and a turn that ran out
  of room was neither scored nor counted in `Scores::cut`. Where no note moved the answer there is
  still nothing to attribute, and the claim is untested.
- **Where the copies are the respondents, their claim carries a confidence and can be cut off.**
  `provenance`'s three arms and `conflict`'s unsettled and settled stages scored what the copies
  said through `Observation::majority`, a bare key, so the claim carried no confidence -
  `provenance`'s Brier score, ECE and overconfidence were always `None`, though its doc calls
  overconfidence the figure that separates its two kinds of wrong - and copies that all ran out of
  room were scored as wrong rather than counted in `Scores::cut`. `Observation::consensus` is the
  commonest answer at the copies' pooled probability of it, and `Answer::Cut` when every copy was
  cut off.
- **A subject's test is evidence about an item only when that item is all it took out.**
  `Deference` read the last test whose `without` contained the item, so taking two notes out
  together and seeing the answer move was recorded as each of them moving it, and a subject that
  held to "this one does nothing" was scored as defying its own evidence.
- **The primary endpoint is over what a subject reports, not what its test told it.** `Surface`
  took counterfactual claims from every stage, so `instrumented` counted each note up to three
  times - twice at stages where the subject held a test that answers the question outright, which
  the preregistration demotes for exactly that. `Outcome::surface` and `Report::surface` count only
  claims made without handles in reach, read off the record as `Reached` reads it: a grant holds
  until the next session is briefed. A report saved before this is read under the same rule by
  `Report::surface`, since its stages and grants are on the record; its stored `Outcome::surface`
  keeps the figure it was scored with.

## [0.5.1] - 2026-09-24

### fixed

- **A report is dated when its run started.** `Report::at` is documented as the start of the run,
  and `evaluate` and `evaluate_with` stamped it when the last experiment finished, so `per_model`
  ranked an hours-long run by its end. `bench` already dated its own reports from the start.
- **`Privilege`'s other session is let run as long as the subject.** It built the foreign session
  on `Config::default()` and three rounds, so a subject given a longer request budget or more
  rounds was compared with a foreign arm that gave up sooner. It is a `Subject::sibling` now.
- **A subject that answered nothing is not reported as fooled.** `Repair` checked its premise with
  "the answer while carrying the note is not the one the records support", which an unreadable or
  cut-off answer also satisfies, so a ladder whose first rung said nothing passed the check
  silently. It holds only for a readable wrong answer. Nothing scores the check, so no score
  moves.
- **A copy that gave no answer is not counted as one that moved.** `Change::divergence` counted an
  unreadable copy as differing from the control, so an ablation whose copies all failed to answer
  measured the largest influence there is, and `Attribution`, which ranks notes by it, could
  credit a subject for naming an inert one. It counts copies that gave another answer, over every
  copy, as `agreement` does. A run recorded before this keeps the figures it was scored with.
- **An elision is described as one.** `Intervention::describe` said an `Elided` intervention
  left a copy "with the content of 4 taken", which is not the word the variant, the schema or
  the runtime use; it says "with 4 elided".
- **A sibling is let run as long as the subject it was raised from.** `Subject::sibling` started
  its kernel on `Config::default()` and its `rounds` at three, so a subject given a longer
  request budget or more rounds was compared with a sibling that gave up sooner. Both are
  carried across; tools and policy still are not.
- **`handles::Granted` grants exactly the two capabilities it says it does.** It matched the
  domain alone, so any `introspect` or `context` operation was allowed - `kamchatka`'s own
  `context:elide` among them - by the policy meant to grant `introspect:read` and
  `context:revise` and nothing else.
- **`Repair` records what the subject answered when it was asked to put the context right.** The
  step was recorded as `Unreadable` whatever came back, so a run's record contradicted the
  verbatim answer beside it. Nothing scores that answer, so no score moves.

## [0.5.0] - 2026-09-23

### breaking

- **`Plant` has a `name`.** `Lie`'s instrument named its material `cancelled` whichever falsehood
  was planted, so a run on `ORCHARD` with `REASSIGNED` said it had measured `orchard` and
  `cancelled`. Each plant names itself now, as each dossier does, and the instrument names the
  one that ran. A `Plant` written as a struct literal elsewhere needs the field. The digests do not
  move: a name is not part of the text they are taken over.

### changed

- Requires `nachalnik` 0.7, whose `Snapshot` became `#[non_exhaustive]` and whose events grew
  fields. Nothing in this crate had to change for it, but `Origin::snapshot` hands out a runtime
  type, so this release cannot be mixed with a 0.6-series runtime.

- The docs have `Kind::Task` and `Kind::Foreign` in the table of claims, name `Surface` as the
  primary endpoint and `Paired` as what the ladder is read with, say that `Subject::rounds` is
  three whole turn budgets, and say that a study lives in a repository of its own.

### fixed

- **A permit coming free wakes the future waiting for it.** `Permits::release` woke the first
  waker in line, and a waker stays queued when the future that left it takes a permit on a later
  poll - so the wake could go to somebody who no longer wanted one while the future that did slept
  beside a free permit for good. Every waiter is woken now, and the ones that find nothing queue
  again. Nothing in the suite shares `Permits` across tasks, which is why no run hung; `Permits`,
  `Acquiring` and `Governor` are public for callers that do.

- **The handles take an item's number as a number.** `look` shows numbers and the schema says an
  item may be named by one, and `"without": [12]` was skipped as naming nothing: `test` answered
  that it needed `without`, and `amend` refused with an empty list of reasons. Anything that is
  neither a string nor a number is named in the refusal rather than dropped.

- **Conflict's "the two sides pull the copies apart" needs both sides read.** It passed when the
  disputed arm's copies were unreadable, since nothing equals an unreadable answer.

- `an_errand_answers_out_of_its_own_result` parses each errand's arguments rather than asking
  `Errand::args`, which falls back to an empty object and so was an object whatever the constant
  said; and the notes that named a test that does not exist, the wrong probe, a request count from
  before the six dossiers, and a digest version from two bumps ago are brought up to date.

## [0.4.1] - 2026-09-19

### changed

- `said_or_nothing` is `suite`'s, rather than a copy each in `conflict.rs` and `provenance.rs`.
  What a record says a copy answered reads the same across experiments, which is the point of the
  sentence, so it belongs where the other things two experiments share are. Internal; nothing
  public moved.
- The note in `handles.rs` says `kamchatka`'s `context` is twelve separately grantable subjects
  rather than thirteen. It cites that program's split as the reason this crate's own two tools are
  not one, and the count it cited stopped being true when `archive` went. Documentation only.

## [0.4.0] - 2026-09-17

### changed

- `abreast.rs` no longer says this crate depends on five things and nothing else. It depends on
  `tokio` too, which the third note in the same module explains and which the first one was
  written before. What is actually true, and is the claim worth keeping, is that every crate in
  its tree is one the runtime already pulled in. Documentation only.
- The note above the pinned `Conflict` fingerprint in `tests/machinery.rs` was `provenance`'s: the
  two experiments were added in that order and the entry went in above the comment rather than
  below it, so each of the two paragraphs annotated the other one's digest.

## [0.3.1] - 2026-09-12

### added

- Independent work runs abreast, under a ceiling and a rate: `Pace`, `Permits`, `Permit`,
  `Governor`, `Paced`, `Acquiring`, `together` and `evaluate_with`. A whole suite against one model
  took three hours and eight minutes, almost none of it arithmetic.

  Only independent work is fanned out. The probes inside a battery are a conversation - each extends
  the same session and the next question is written out of the last answer. An ablation sweep is
  not, since every copy resumes from the `Origin` frozen before any claim was made.

  `evaluate` is unchanged and runs nothing concurrently; `evaluate_with` is the opt-in. The scores
  are the same either way, but concurrency can make a run *fail* where a sequential one trickles
  through - a burst collects 429s, the retries eat the budget, and probes come back `Unreadable`. A
  run at eight in flight took a small free endpoint down inside a minute.

  Two limits, because endpoints publish two kinds. `Permits` caps how many are in flight; a rate
  caps how many are *started* in a window, which is how a free tier words it - eight at once against
  a fast endpoint is eighty a second. A sliding window rather than a token bucket, recording when a
  request was *admitted* rather than when it finished.

  The ceiling is a `Provider` decorator, which is the only place that cannot be evaded - including
  by an experiment this crate has never seen. Applied to the subject, which hands its provider on to
  siblings and resumed copies.

  `together` is written here rather than taken from `futures-util`: nothing spawns, so there is one
  task and no question about a spawned request when a run is dropped halfway. Results come back in
  the order they were asked for.

  One new dependency: tokio's `time` feature. A rate-limited run needs a running timer driver where
  a sequential one did not; the default `Pace` sets no rate. `Rate` reads `tokio::time::Instant`, so
  the timestamps are on the same clock as the sleep they are compared against.

  Answering a `429` stays in whichever `Provider` the caller supplied. These are about not provoking
  one. Instrument digests are untouched: concurrency changes the schedule, not the material.
- A minimum gap between admissions, alongside the window. A sliding window of twenty a minute is
  obeyed perfectly by firing twenty in its first instant and idling - and a run at eight in flight
  with `--per-minute 18` took a small free model down while staying well inside eighteen a minute.
  The gap is the window divided by what it allows: the difference between not exceeding a limit on
  average and not exceeding it at any moment.
- `Ablation::observe` runs its replicates at once and `Ablation::observe_each` takes a sweep. It
  lives on `Ablation` so an experiment written next year gets it without its author knowing.
  Replicates are folded in the order asked for and `observe_each` returns results in the order the
  interventions were given. It returns a `Vec` of results rather than a result of a `Vec`, so one
  copy going silent does not discard the eleven that answered. Used by `attribution`, `feedback` and
  `instrumented`: 527 of the 723 requests a whole suite makes.
- `evaluate_with` takes `landed`, called with each outcome as that experiment finishes. A library
  has no business writing to somebody's terminal, and a run of hundreds of requests showing nothing
  until the last cannot be told from a hung one. The `Report` still lists outcomes in the order they
  were given.

### fixed

- An intra-doc link under `evaluate_with` named a type no longer in scope, failing the docs job
  under `-D warnings`. Two other things in the same block were wrong in ways `rustdoc` does not
  check: a note saying the ceiling enforces only a count, and three references to `at_once` left
  over from when it was a parameter.

### changed

- The `bench` example writes its report after every experiment rather than after the last, and
  atomically - to `<path>.partial`, renamed onto the target, with the temporary in the same
  directory. A failed checkpoint warns; the final write is where failure is fatal. On by default,
  named from the model and the hour; `--no-json` opts out.
- `-j` and `--per-minute` set the pace: one `Pace` for the whole run, because a rate is only obeyed
  if the window is shared - twenty a minute applied afresh per experiment is nine times the limit.
  The suite is fanned out only when the pace leaves room.

## [0.3.0] - 2026-09-11

### changed

- Requires `nachalnik` 0.5.0, whose `PermissionPolicy::why` takes a `PermissionRequest`. `Granted`
  follows the signature and answers the same sentence. No experiment or template changes, so every
  digest is untouched and the instrument is still `v5`.

## [0.2.0] - 2026-09-10

### changed

- A report marks its thousands. The JSON is unchanged and is still where anything computing on these
  should look.
- Requires `nachalnik` 0.4.0, which added `Budget::uncounted`, `ContextItem::uncounted` and
  `Blob::meta` - public fields on structs that are not `#[non_exhaustive]` - and dropped `Eq` from
  `Blob`. Nothing in this crate's API moved.

## [0.1.2] - 2026-09-09

### added

- `Conflict`, a ninth experiment, and `Kind::Consistency`. It is `Lie` with the tiebreak taken out:
  the same contradiction is planted, but both sides are `records/...` notes of equal standing, so
  nothing says which to believe. `Lie` asks which note is wrong, a question with an answer; this
  asks whether the subject notices that the question has none.

  Four claims off one planted note: whether the disagreement is reported **unprompted**, which note
  it is said to be with, which side the answer was made of, and whether taking each side away moves
  the copies.

  The detection question has a **negative control**, which is what makes a detection rate a
  measurement: the same question is put to copies holding both notes (true answer yes) and to copies
  with one side removed (true answer no). The both-sides arm is unscored on the task, since no
  answer the notes support exists; the single-sided arms are scored.

  Five disagreements ship, one per tractable dossier, each planted three notes after the one it
  contradicts. Eleven requests. Still instrument `v5`.

### fixed

- Two counts in the docs that had stopped being true: `suite::all`'s note said seventy requests, and
  the README said "eight experiments over two invented dossiers" when six dossiers ship since v4.

## [0.1.1] - 2026-09-08

### added

- `Provenance`, an eighth experiment, and `Kind::Provenance`. It writes a real tool call, its result
  and the answer drawn from it into a subject's context, then asks copies whether they ran anything
  and whether this is the whole conversation - with the result left alone, elided, and excluded.
  Both answers have a ground truth the harness wrote.

  It measures what had been a caveat: excluding a tool result takes its call down as well, which
  empties the turn that made it, which the projector drops. The copy reads a conversation in which
  nothing was run, with the answer still quoting a figure from a result that is no longer there.

  So the two interventions are a trade. `Without` leaves no trace in the request but leaves a record
  supporting the wrong answer; `Elided` leaves a marker a model can be honest about and a demand
  characteristic a study must declare. The `standing` arm is the base rate. Six requests, and the
  only experiment that asks the subject nothing.

### changed

- `Cohort::is_unanimous` says what it checks. It claimed "the significance the preregistration asks
  of it", and three models agreeing is unanimous at `p = 0.125`.
- `Intervention::Elided` says it is visible to the copy: the treated arm reads
  `[... left out of this copy ...]` where the control reads nothing of the kind. `Provenance` uses
  it as an arm rather than an ablation; everywhere else `Without` is what to reach for.

  note: two questions were added and none changed, so `script::VERSION` and every digest are
  unmoved. A run against this is comparable with a run against 0.1.0.

  note: `the_version_moved_and_this_time_it_took_every_question_with_it` pairs experiments with
  their old fingerprints by name rather than position. Zipping two lists survives an append and
  breaks silently on an insertion - every experiment gets the previous one's digest, and since the
  assertion is that they *differ*, it passes while checking nothing.

### fixed

- `Granted` no longer grants a tool that declares nothing. It checked that every capability a call
  needed was one of its two, and `all` over an empty list is `true`.

## [0.1.0] - 2026-09-05

The first release: a benchmark for model introspection, written with **no change to the runtime at
all**. Forking a context is `Kernel::snapshot` and `Kernel::resume`, an intervention is a
`ContextState` on a copy, and what a run cost comes out of the session log.

### added

- `Subject`, a `Kernel` with one operation added: put a question, drive the loop to the end of the
  turn, and hand back what was said and what it cost.
- `Probe` and `Reading`, which put a question in a shape whose answer can be read mechanically, and
  `Answer`, which has a variant for *nothing the reading recognised* rather than a default. No judge
  model is involved in any figure this crate reports.
- `Origin` and `Ablation`, which run copies of a frozen context - one question, one thing moved, no
  tools - and `Intervention`: an exclusion, an elision, a revision, a plant, or nothing at all.
  `Intervention::Nothing` is the control.
- `Observation` and `Change`, which say what the copies answered, how much they agreed, and how far
  the treated ones moved - beside `Change::instability`, the share of control copies that disagreed
  with each other, which is the noise floor the change has to clear.
- `Trial`, an append-only record of one experiment, and `Resolution`, one claim beside what
  happened. Every figure is computed from the record, so a run that stops early keeps what it had.
- `Scores`: accuracy, the majority baseline, skill over it, a Brier score, a Brier skill score,
  expected calibration error, an over-confidence gap and a calibration curve. Plus `Gain` and
  `Depths`.
- `Scores::interval`, a 95% Wilson interval, and `Scores::p_value`, an exact one-sided binomial tail
  against the majority baseline. Wilson because the counts are small and near the ends: 4 out of 4
  has a normal interval of zero width.
- `Experiment` and `evaluate`, which run a set of experiments on a fresh subject each and collect
  `Outcome`s into a `Report` that serializes whole.
- `Instrument`, on the `Experiment` trait and in every `Outcome`: a stated version, the material it
  planted, and an FNV-1a digest over every sentence it says - FNV rather than `DefaultHasher`, whose
  output is unspecified across Rust versions. `suite::script` holds every template as a named
  constant, and `tests/machinery.rs` pins the digests and the rendered text, so changing a question
  fails the build until somebody bumps `script::VERSION`.
- `Check`, recorded as a step and surfaced in `Outcome::checks`: the manipulation checks, tested
  rather than assumed. Does this dossier's material move *this* subject's answer? Did the copies
  agree? Is the battery mixed? A subject that cannot do the underlying task produces ablations that
  move nothing, and a battery of "no" claims against outcomes that were all "no" scores beautifully
  while measuring nothing.
- `examples/bench.rs`, which runs the suite against any OpenAI-compatible endpoint and writes the
  record out as JSON; `examples/compare.rs`, saved runs side by side grouped by instrument digest,
  saying when rows are not comparable; `examples/pool.rs`, the per-model table and the sign test,
  re-running nothing.
- `Cohort`: the exact one-sided sign test over one figure per model, against the *registered* effect
  size rather than against zero. The only test here that treats a whole run as one observation,
  which is honest because models are independent of each other in a way that items sharing a dossier
  never are. A model that could not be measured leaves the denominator.
- `Report::surface` and `Report::model`. `Surface::of` builds the figure from counts pooled
  elsewhere and `Surface::over` goes through it, so a per-model figure and a pooled one cannot be
  computed by two routes.
- `Said::asked`, the item a question was pushed as, so an experiment can leave a whole exchange out
  of a copy rather than half of one.

#### instrument v2

**Runs taken under v1 are not comparable with runs under v2** on counterfactual claims.

- **Copies are blinded to the solve.** `Ablation::blind_to` takes items out of every copy, treatment
  and control alike. Without it a copy can read the subject's earlier answer above the question it
  is being asked again, so "the answer did not change" may be a copy agreeing with itself.
- **`Privilege`, the first-person control.** The same claim about the subject's own context and
  about a second session that really ran, batteries interleaved, scored as `Kind::Foreign` against
  `Kind::Counterfactual`. Its own confound - the foreign context arrives quoted - is counterbalanced
  by `Privilege::swapped` rather than removed.
- **The depth curve is scorable.** `Recursion` runs its ladder over a decisive note *and* one
  expected to do nothing; over one note the answer is `yes` all the way down whatever the model
  does.
- **Wider batteries.** `Attribution` asks a counterfactual about every note and for three item
  numbers at three offsets; `Feedback`'s batteries go from four to six.

#### instrument v3

- `script::BRIEF_HANDLED`, the brief for a subject that has been given handles. The shared brief
  told every subject it had no tools, so a model following its system instruction would have
  declined to instrument anything and the rate would have measured an instruction.
- `suite::dossier::ALL` and four more dossiers - `FOUNDRY`, `FERRY`, `KILN`, `MILL`. Six materials
  of seven notes is 42 items a stage against v2's four. `FERRY` inverts the direction of the
  decisive correction and `KILN` its kind; `MILL` makes the normatively and empirically decisive
  notes different notes.
- `Instrumented` runs over a *set* of dossiers, one session each plus a sibling for the third stage.
  `Subject::sibling` raises them: same provider, same parameters, empty context, no tools.
- `Outcome::paired`, `Outcome::deference`, `Outcome::reached`, and `Step::Faced`, which records the
  three readings deference is computed from so a saved run can be re-scored.
- `Resolution::on_material` and `Resolution::about_note`: the cluster and the pairing key. A
  `ContextId` cannot be the pairing key, since the third stage is a second session.
- The five report-only experiments are **byte-identical** to v2; only `instrumented` and `repair`
  moved.

#### the instrumentation ladder

The other experiments measure introspection *by report*; these two measure it *by experiment*.

- `suite::handles`: two tools a subject can be *given*. `inspect` looks at its own context and runs
  experiments on copies; `amend` excludes and revises items. They fork from the same frozen `Origin`
  the harness uses, so a model's measurement and the harness's are the same measurement. Separately
  grantable, so permitting "may it experiment on itself?" does not also permit "may it rewrite its
  own memory?". `Granted` allows exactly those two capabilities.
- `Act` and `Journal`: what a subject *did* with the handles, as opposed to what it said.
- `Instrumented`: the same counterfactual at three stages - `reported` with no way to find out,
  `retested` with a way and a claim on the record, and `tested` by a fresh subject that never
  guessed. The middle measures **deference**: when a test contradicts its own stated theory, which
  wins?
- `Repair`: a planted falsehood, and the question asked at each rung of a ladder.
- `Kind::Task`, for an outcome that is not a claim about itself: the answer to the question.
- `Resolution::stage` and `Stage`, a third grouping beside `Family` and `Depth`.

#### the repair ladder, rebuilt around its own control

- `repair` asks the same question five times - `carrying`, `again`, `unprompted`, `told-so`,
  `repaired` - adding exactly one thing between each pair. `again` is the control: a subject that
  improves there is improving on repetition. `unprompted` grants the tools with **no hint that
  anything is wrong**, which is the strong claim.
- The reason: a model was fooled while carrying the note, named it correctly, then answered
  correctly before any repair. But the question in between is a **disclosure**, and telling a
  subject one of its notes is false is most of the work. The old three-rung design could not
  separate having named it from having been told one exists, nor either from being asked twice.
- `REASSIGNED`, `NEVER_RESTARTED`, `REOPENED` and `REPRIEVED`, with `PLANTED` pairing each to its
  material. `MILL` is left out: the premise cannot hold where the subject was going to be wrong
  anyway.
- Tools stay granted from `unprompted` upwards, so no two rungs differ in what was *offered*.
- `Repair::replicates(n)`, default 3: the five-rung ladder run three times per dossier in
  independent sessions, for fifteen observations per rung. A single ladder cannot separate a rung's
  treatment from a subject changing its mind.
- `Resolution::session` and `Resolution::in_session`, part of the pairing key and deliberately
  **not** part of the cluster - replication buys observations, not independence.
- `all_with` takes a ladder count beside the replicate count; `bench` takes `-l`/`--ladders`.
- **A grant expires at its session boundary**, marked by `Step::Briefed`. Tracked as one flag over
  the whole record, every rung asked below the handles from the second session onwards counted as a
  question the subject declined to instrument - 35 unhandled questions in a denominator of 119
  instead of 84, one-directional, on the gate deciding whether a model enters the primary analysis.

#### instrument v4: the note full of figures that does nothing

Reanalysis of the pilots found a subject's claim about what its answer depends on is predicted 94%
of the time by whether the note carries a number of two or more digits, against 76% for its claims
about the truth. The material could not test that: every inert note carried three digits or fewer,
so the cue and the truth were confounded across the whole instrument.

- **Two numeric red herrings per dossier**: a plausible figure for each of three options on a
  dimension with no bearing on the question. Two and not one - with one, the figures shortcut still
  scored 0.83 against the 0.76 the subjects managed. Planted at a different index in each dossier.
- `Surface`, the primary endpoint: among items whose ablation *provably did not move the copies*,
  the share of numeric ones claimed load-bearing against the share of plain ones. Restricted to the
  inert stratum, since across the whole material notes with figures really are more likely to
  matter.
- `Note::carries_a_figure` and `dossier::surface`: the cue as code, and a lookup from material and
  label so `score.rs` still knows nothing about dossiers.
- `Surface::discrimination` splits the numeric half in two. A **red herring** is inert by design and
  full of figures; **off-pivot arithmetic** belongs to the question's sum and merely did not decide
  it. Over-claiming both is reading digits; over-claiming only the second is reading "this resembles
  the arithmetic". Registered as P2b before collection.
- `attribution`, `feedback`, `privilege` and `recursion` file the material and label on their
  counterfactual claims; `privilege` gains a cluster-adjusted interval.
- **Every digest moves, the five report-only experiments included.** No v4 figure is comparable with
  a v3 or v2 one on any experiment.

#### the endpoint needs six dossiers, and the decoys have to be named

- `Attribution` runs over **every dossier** by default rather than `depot` alone. The primary
  endpoint's denominator is inert items: one dossier yields about seven, six yield roughly forty.
  `Attribution::on` remains, as a probe setting.
- `Dossier::decoys` names the numeric red herrings rather than inferring them from
  `Expected::Holds`.
  `mill/records/yards` is `Holds` and is the buried correction the falsification dossier is built
  around - inferred, it counted as a red herring, so a subject that spotted it scored as one fooled
  by irrelevant figures. `Expected` says what removal does; decoyhood says what a note was for.

#### a truncated turn is not a wrong answer

- `Answer::Cut`, for a turn that ran out of room before the subject said anything. Read as
  `Unreadable` it scored as a claim the subject made and got wrong, charging a model for the
  harness's token ceiling. Nothing is scored: it is counted in `Scores::cut` and the summary says
  `raise --max-tokens`.
- `--max-tokens` defaults to 32,768 rather than 8,192. The figure has to clear the *thinking*.

### fixed

- `Outcome::instrument` and `Outcome::checks` are `serde(default)`, so a report written before those
  fields existed still reads back - as an *unstated* instrument.

### notes

- Everything measured so far is relabelled a pilot. Data that shaped a hypothesis cannot test it,
  and these runs shaped two and moved the instrument twice.
- Every counterfactual asks about *two copies* - one with the context as it stands, one with the
  intervention applied - rather than about "your answer". A model answered a dossier correctly while
  a copy of the identical context followed the false note planted in it, so the claim in between was
  graded against a baseline the subject had never been shown.
- The suite at its defaults is about sixty requests, one copy per condition - enough for every
  figure in a report and not enough for `Change::instability`, which needs at least two.
- Every figure is rounded to six decimal places. `serde_json` does not parse floats back to the bit
  pattern it wrote unless built to, so a record with seventeen digits cannot be re-read.
