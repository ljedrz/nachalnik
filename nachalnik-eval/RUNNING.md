# running the suite

Running it against a model, staying within rate limits, reading results back, and how the harness
itself is tested. [The README](README.md) says what is measured and why the numbers mean something.

---

## 🛠️ using it on your own model

Any `Provider` works, so it isn't tied to any model. The crate has no HTTP client, for the same
reason the runtime doesn't:

```rust
let report = evaluate(suite::all_with(2, 1), |name| {
    let kernel = Kernel::new(Config { session_name: Some(name.to_owned()), ..Config::default() });
    kernel.set_provider(provider.clone());   // yours, however you reach it

    Ok(Subject::new(kernel))
})
.await;

println!("{report}");
std::fs::write("run.json", serde_json::to_string_pretty(&report)?)?;
```

Each experiment gets a fresh subject, because a session that has already been asked about itself
knows it's being measured. The JSON is the complete record (every question, every answer word for
word, every copy's reply, every comparison), so a run can be scored again without paying for it
again.

Your own experiment is one trait method:

```rust
#[async_trait]
impl Experiment for Mine {
    fn name(&self) -> &str { "mine" }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        let origin = Origin::of(subject)?;                       // freeze the context
        let probe = Probe::claim("...");
        let (said, claim) = subject.probe(&probe).await?;
        trial.asked(&probe, &said, &claim);

        let ablation = Ablation::new(probe).replicates(2);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        let treated = ablation.observe(&origin, Intervention::without([id])).await?;
        let change = treated.against(&control);

        trial.resolve(Resolution::new(Kind::Counterfactual, claim, change.as_answer()));

        Ok(())
    }
}
```

Nothing is tracked separately: a `Trial` is an append-only record and every figure is computed
from it, so a reported score always matches the steps behind it. An experiment that fails at
request forty keeps everything up to request thirty-nine and reports what stopped it.

---

## ⏱️ how fast it is allowed to go

A whole suite against one model is about a thousand requests at `-r 2`, which takes hours if each
waits for the previous one. Most of that waiting can be avoided. The questions within a battery
(solve, then introspect, then predict) form a *conversation*, each based on the previous answer, so
they can't run in parallel. Ablations can: every copy is resumed from the same `Origin`, frozen
before any claim was made, so no copy sees another's, and they can all run at once.

`evaluate` runs everything one at a time. `evaluate_with` is the opt-in:

```rust
let report = evaluate_with(
    suite::all_with(2, 1),
    make_subject,
    Pace::at_once(4).per_minute(18),   // in flight, and started per minute
    |outcome| println!("{outcome}"),   // fires as each experiment lands
)
.await;
```

The scores are the same either way, since nothing they're computed from depends on what else was
running. The difference is that concurrency can make a run *fail* where a sequential one would have
slowly succeeded: a burst of requests gets 429s, the retries use up the budget, probes come back
`Unreadable`, and the report ends up full of untested claims.

So `Pace` has the two limits endpoints publish: `at_once` caps how many requests are **in
progress**, and `per_minute` how many are **started** within a sliding window, spread out rather
than all at the start. `evaluate` and `evaluate_with` wrap the subject's `Provider` in it, so no
experiment can get around it, however many requests it runs in parallel. Those parallel requests
are made in `Ablation::observe` and `Ablation::observe_each`.

What to do once a limit is exceeded anyway is deliberately not handled here. A `429` and its
`Retry-After` are handled by the `Provider` you supplied, since that's the layer that understands
the response format. `Pace` is about not causing them.

The `bench` example takes `-j` and `--per-minute` for these, and saves its report atomically after
every experiment, so a run killed partway keeps every finished experiment.

---

## 📈 reading a sweep back

Two more examples, neither of which sends any requests: they read the reports `bench` saved, so
analysing results costs nothing and can be repeated months later by someone else. That's why
`--json` keeps every question and answer word for word: a figure in a paper should be recomputable
from the record. The runs below were each saved with `--json eval-runs/<study>/<run>/report.json`;
without it, `bench` writes `bench-<model>-<when>.json` in the working directory.

```console
$ cargo run -p nachalnik-eval --example compare -- eval-runs/*/*/report.json
$ cargo run -p nachalnik-eval --example pool    -- eval-runs/*/*/report.json
```

**`compare`** shows runs side by side, but won't treat runs that asked different questions as
comparable. The arithmetic could be done in a spreadsheet; what a spreadsheet won't notice is that
one file came from a version of the questions with a word changed. Runs are grouped by
`Instrument::digest` and by `Outcome::rules` (the rules their claims were scored by), and if one
experiment's rows come from more than one of either, they're printed under a line saying they
aren't comparable.

**`pool`** computes figures across *models*. `bench` measures one model, and every figure it prints
comes from items that share a dossier, so they aren't independent. The only statistical test `pool`
applies is the sign test, over one run per model, which is valid there and nowhere else in this
crate, because models are independent of each other in a way items aren't. A model is identified
by its name, whichever endpoint served it, since the same weights through two endpoints are still
one model. `Cohort::is_unanimous` means unanimous, not statistically significant (three models
agreeing is unanimous at `p = 0.125`), which is why a study should fix its cohort size in advance.

---

## 🧫 how the harness itself is checked

The obvious problem with an introspection benchmark is that a run against a real model can't tell
you whether the *harness* is correct, because nobody knows what item 4 was really doing.

So `tests/harness.rs` runs the whole loop against a provider whose behaviour the test defines: it
answers `kirov` when a certain phrase is in the request and `omsk` when it isn't. Then exactly one
of the planted notes determines the answer, and the test knows which in advance; a run that reports
anything else has a bug. `tests/machinery/` checks the arithmetic against numbers worked out by
hand, and `tests/live.rs` checks what neither can: that a real model answers in the format the
probes ask for, and that the record comes back complete.
