# running the suite

Pointing it at a model, keeping it inside a rate limit, reading a sweep back, and how the
harness itself is checked. [The readme](README.md) says what is being measured and why the
number means anything.

---

## 🛠️ using it on your own model

Any `Provider` works, which is what makes this model-agnostic. There is no HTTP client in the
crate, for the same reason there is none in the runtime:

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

## ⏱️ how fast it is allowed to go

A whole suite against one model is seven hundred-odd requests, and hours of waiting if each one
waits for the last. Most of that is avoidable. The probes inside a battery — solve, then
introspect, then predict — are a *conversation*, each question written out of the last answer, so
they cannot overlap. An ablation sweep is not. Every copy in one is resumed from the `Origin`
frozen before a single claim was made, so no copy can see another's, and the whole sweep can go at
once.

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

The two are not interchangeable, and the reason is not the scores — nothing a figure is computed
from depends on what else was in flight, so those are the same either way. It is that concurrency
can make a run *fail* where a sequential one would have trickled through: a burst collects 429s,
the retries behind them eat the budget, probes come back `Unreadable`, and a report quietly becomes
a page of untested claims.

So `Pace` carries two limits, because endpoints publish two kinds and neither implies the other.
`at_once` caps how many requests are **in flight**. `per_minute` caps how many are **started** in a
window, which is how a free tier words it and which no count of things in flight can stand in for —
eight at once against a fast endpoint is eighty a second. The window is a sliding one, because the
limit is worded as one. It also comes with a minimum gap between admissions: twenty a minute is
obeyed perfectly by firing twenty requests in the window's first instant and then idling, and that
is not a reading of the limit any endpoint's own limiter shares.

The ceiling is applied by wrapping the subject's `Provider`, which is the only place it cannot be
evaded — including by an experiment this crate has never seen. A ceiling on the ablation sweep
alone is not one: nine experiments in flight put nine live probes on the wire underneath it.
`Ablation::observe` fans its replicates out and `Ablation::observe_each` takes a whole sweep, so an
experiment gets the concurrency by calling the methods it already called, and cannot exceed what
the caller allowed however wide it fans.

What is deliberately *not* here is what to do once a limit has been exceeded anyway. A `429` and
its `Retry-After` are answered in whichever `Provider` you supplied, because that is the layer that
knows the wire format they arrived in. These are about not provoking one.

The `bench` example takes `-j` and `--per-minute` for the two, and writes its report after every
experiment rather than once at the end — atomically, via a rename, so a reader or a killed process
sees one whole checkpoint or the other and never the flushed half of one. A suite is hours long,
and a run killed partway keeps every experiment it finished.

---

## 📈 reading a sweep back

Two more examples, and neither asks a model anything: they read the saved `report.json` files, so
the analysis of a sweep costs nothing and can be repeated months later by somebody who was not
there. That is what `--json` holding every question and every answer verbatim is *for* — a figure
in a paper should be recomputable from the record.

```console
$ cargo run -p nachalnik-eval --example compare -- eval-runs/*/*/report.json
$ cargo run -p nachalnik-eval --example pool    -- eval-runs/*/*/report.json
```

**`compare`** puts runs side by side and refuses to pretend that runs asked different questions
are comparable. Everything else it does is arithmetic anybody could do in a spreadsheet; what a
spreadsheet will not do is notice that one of the files came from an instrument with a word
changed in it. Runs are grouped by `Instrument::digest`, and where one experiment's rows come from
more than one instrument they are printed under a line saying they are not comparable.

**`pool`** computes the figures that are about *models*. `bench` measures one model, and every
figure it prints is computed over items that share a dossier and are therefore not independent.
So the only test `pool` applies is the sign test, over one run per model, which is honest there
and nowhere else in this crate: models are independent of each other in a way that items never
are. `Cohort::is_unanimous` is unanimity and not significance, and three models agreeing is
unanimous at `p = 0.125`, which is why the cohort size is a decision a study registers in advance.

---

## 🧫 how the harness itself is checked

The obvious problem with a benchmark for introspection is that a run against a real model cannot
tell you whether the *harness* was right: nobody knows what item 4 was doing.

So `tests/harness.rs` runs the whole loop against a provider whose causal structure the test
wrote — a rulebook that answers `kirov` when a phrase is in the request and `omsk` when it is not.
Exactly one of the planted notes is then load-bearing, and it is known which, in advance. A
run that reports any other ranking has a bug in it. `tests/machinery.rs` checks the arithmetic
against numbers worked out by hand, and `tests/live.rs` checks the one thing neither can: that a
real model answers in the shape the probes ask for, and that the record comes back complete.
