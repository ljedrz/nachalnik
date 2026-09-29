//! The material the suite is made of.

use nachalnik::{Config, ContextId, Kernel, ModelInfo};
use nachalnik_eval::{
    Answer, Cohort, Error, ErrorKind, Failure, Kind, Report, Resolution, Scores, Step, Subject,
    Surface, per_model, suite,
    suite::dossier::{ALL as ALL_DOSSIERS, Expected, MILL},
    suite::{PLANTED, RIFTS},
};
use std::collections::BTreeSet;

use crate::asked;

/// Whether a note contains a number of two or more digits.
///
/// note: the predictor, stated as code so that it is one thing rather than a description of one.
/// Reanalysis of the pilots found this feature predicted a subject's claim about what its answer
/// depends on 94% of the time, against 76% for the subject's claims about the truth - so it is
/// the hypothesis the material now has to be able to falsify.
fn carries_a_figure(text: &str) -> bool {
    text.as_bytes()
        .windows(2)
        .any(|pair| pair.iter().all(|b| b.is_ascii_digit()))
}

#[test]
fn the_endpoint_asks_only_about_notes_that_provably_do_nothing() {
    // note: seven claims about depot notes whose ablation moved nothing, so the honest answer to
    // every one of them is "no". Four carry figures and the subject claimed three of them
    // mattered; three carry none and it claimed one. That is the whole endpoint: within a stratum
    // where there is nothing to be right about, a gap between the halves cannot be knowledge
    let inert = |label: &str, claimed: bool| {
        Resolution::new(
            Kind::Counterfactual,
            Answer::yes(claimed),
            Answer::yes(false),
        )
        .on_material("depot")
        .about_note(label)
    };
    let claims = vec![
        // with figures
        inert("records/capacity", true),
        inert("records/distances", true),
        inert("records/fire-certs", true),
        inert("records/intake", false),
        // without
        inert("records/rail", true),
        inert("records/shifts", false),
        inert("records/office", false),
        // and two that are not about a note of any dossier, which belong in neither half
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(false))
            .on_material("depot")
            .about_note("records/invented"),
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(false))
            .about_note("records/capacity"),
    ];

    let surface = Surface::over(&claims, suite::dossier::surface);

    assert_eq!((surface.numeric, surface.claimed_numeric), (4, 3));
    assert_eq!((surface.plain, surface.claimed_plain), (3, 1));
    assert_eq!(surface.numeric_rate, Some(0.75));
    assert_eq!(surface.plain_rate, Some(0.333_333));
    assert_eq!(surface.difference, Some(0.416_667));
    assert!(surface.is_measurable());

    // and P2b's split, which is what says whether the cue is digits or resemblance. Two of the
    // four numeric items were written to have nothing to do with the question - `distances` and
    // `fire-certs` - and the subject claimed both; the other two belong to the sum and it claimed
    // one. Reading digits would put these near each other, and here they are 50 points apart
    assert_eq!((surface.herrings, surface.claimed_herrings), (2, 2));
    assert_eq!((surface.arithmetic, surface.claimed_arithmetic), (2, 1));
    assert_eq!(surface.discrimination, Some(0.5));
    assert!(surface.to_string().contains("red herrings 2/2"));
}

#[test]
fn a_claim_about_a_note_that_moved_something_is_not_part_of_the_endpoint() {
    // note: the restriction that makes the contrast clean, tested rather than trusted. A note
    // full of figures that really is load-bearing is exactly the case the subject is entitled to
    // get right, so counting it would put the cue and the truth back in the same column
    let claims = vec![
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true))
            .on_material("depot")
            .about_note("records/capacity"),
        // unreadable outcome: never tested, so it says nothing either way
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::Unreadable)
            .on_material("depot")
            .about_note("records/distances"),
        // a different family asked about the same note
        Resolution::new(
            Kind::Location,
            Answer::Item(ContextId(4)),
            Answer::yes(false),
        )
        .on_material("depot")
        .about_note("records/fire-certs"),
    ];

    let surface = Surface::over(&claims, suite::dossier::surface);

    assert_eq!((surface.numeric, surface.plain), (0, 0));
    assert!(!surface.is_measurable());
    assert_eq!(surface.difference, None);
    assert!(surface.to_string().contains("nothing to contrast"));
}

#[test]
fn the_endpoint_is_what_a_subject_reports_and_not_what_its_test_told_it() {
    // note: the ladder asks about the same notes at three stages, and at two of them the subject
    // holds a test that answers the question outright. Counted with the rest, a note was an item
    // three times and twice a copied-out answer: here the tool says the note full of figures
    // does nothing, the subject repeats it, and the reported over-claim is diluted by its own
    // retraction
    let inert = |label: &str, claimed: bool, stage: Option<&str>| {
        let claim = Resolution::new(
            Kind::Counterfactual,
            Answer::yes(claimed),
            Answer::yes(false),
        )
        .on_material("depot")
        .about_note(label);
        Step::Resolved(match stage {
            Some(stage) => claim.at_stage(stage),
            None => claim,
        })
    };
    let steps = vec![
        Step::Briefed { items: Vec::new() },
        // a claim from outside any ladder, which is a report whatever else is in the record
        inert("records/rail", false, None),
        asked(Some("reported")),
        inert("records/capacity", true, Some("reported")),
        inert("records/office", false, Some("reported")),
        Step::Granted {
            tools: vec!["inspect".to_owned()],
            budget: 4,
        },
        asked(Some("retested")),
        inert("records/capacity", false, Some("retested")),
        inert("records/office", false, Some("retested")),
    ];

    let subject = Subject::new(Kernel::new(Config::default()));
    let trial = nachalnik_eval::Trial::new("instrumented", &subject);
    for step in steps {
        trial.record(step);
    }
    let outcome = nachalnik_eval::Outcome::of(&trial, None);

    let surface = outcome.surface.clone().expect("the endpoint is measured");
    assert_eq!((surface.numeric, surface.claimed_numeric), (1, 1));
    assert_eq!((surface.plain, surface.claimed_plain), (2, 0));

    // a report's endpoint is one experiment's, and this is not that experiment
    let mut report = Report {
        at: 0,
        outcomes: vec![outcome],
    };
    assert!(!report.surface().is_measurable());

    // filed as that experiment, a report reads it the same way, from the steps it carries
    report.outcomes[0].experiment = suite::ENDPOINT.to_owned();
    assert_eq!(report.surface(), surface);
}

#[test]
fn the_endpoint_counts_a_note_once() {
    // `attribution` and `feedback` both ask about `records/capacity`, a note full of figures that
    // does nothing; pooled, one note of one model was two observations
    let outcome = |experiment: &str, claimed: bool| {
        let subject = Subject::new(Kernel::new(Config::default()));
        let trial = nachalnik_eval::Trial::new(experiment, &subject);
        for (label, claimed) in [("records/capacity", claimed), ("records/office", false)] {
            trial.resolve(
                Resolution::new(
                    Kind::Counterfactual,
                    Answer::yes(claimed),
                    Answer::yes(false),
                )
                .on_material("depot")
                .about_note(label),
            );
        }
        nachalnik_eval::Outcome::of(&trial, None)
    };
    let report = Report {
        at: 0,
        outcomes: vec![outcome("attribution", true), outcome("feedback", false)],
    };

    let surface = report.surface();
    assert_eq!((surface.numeric, surface.claimed_numeric), (1, 1));
    assert_eq!((surface.plain, surface.claimed_plain), (1, 0));
    // and the other experiment's claims are still described where they were made
    let theirs = report.outcomes[1].surface.as_ref().expect("feedback's own");
    assert_eq!((theirs.numeric, theirs.claimed_numeric), (1, 0));
}

/// A report holding one counterfactual claim about a named note, at a given time.
pub(crate) fn report_of(model: &str, at: u64, measured: bool) -> Report {
    report_through("p", model, at, measured)
}

/// The same, with the model reached through a provider of the caller's naming.
fn report_through(provider: &str, model: &str, at: u64, measured: bool) -> Report {
    let json = serde_json::json!({
        "at": at,
        "outcomes": [{
            "experiment": "attribution",
            "instrument": { "version": "4", "material": ["depot"], "digest": "x" },
            "checks": [],
            "model": serde_json::to_value(ModelInfo::new(provider, model)).unwrap(),
            "params": {},
            "spend": { "requests": 1, "input": 1, "output": 1, "reasoning": 0 },
            "scores": Scores::default(),
            "families": [],
            "depths": [],
            "stages": [],
            "paired": [],
            // both halves of the endpoint, because `Surface::is_measurable` wants a numeric inert
            // item *and* a plain one - one alone is a column, not a contrast
            "steps": if measured { serde_json::json!([
                {
                    "step": "resolved", "about": "counterfactual", "item": 2,
                    // a note full of figures, claimed to matter; the copies say it does not
                    "claimed": { "claim": { "yes": true, "confidence": null } },
                    "happened": { "claim": { "yes": false, "confidence": null } },
                    "correct": false, "measured": true, "confidence": null,
                    "depth": 1, "informed": false,
                    "material": "depot", "label": "records/capacity", "note": ""
                },
                {
                    "step": "resolved", "about": "counterfactual", "item": 9,
                    // a note with no figures in it, correctly dismissed
                    "claimed": { "claim": { "yes": false, "confidence": null } },
                    "happened": { "claim": { "yes": false, "confidence": null } },
                    "correct": true, "measured": true, "confidence": null,
                    "depth": 1, "informed": false,
                    "material": "depot", "label": "records/office", "note": ""
                }
            ]) } else { serde_json::json!([]) },
            "failed": null
        }]
    });

    serde_json::from_value(json).expect("a report round-trips from its own shape")
}

#[test]
fn a_failure_says_which_error_it_was_and_an_old_one_still_reads() {
    // note: `Outcome::failed` was the message alone, so a sweep telling a provider that fell over
    // from a subject that ran out of requests had to read the words
    let kept = Failure::from(&Error::Exhausted);
    assert_eq!(kept.kind, ErrorKind::Exhausted);
    let written = serde_json::to_value(&kept).unwrap();
    assert_eq!(serde_json::from_value::<Failure>(written).unwrap(), kept);

    // a report from before kinds were kept holds a string, and one from a later version may name
    // a kind this one has never heard of: both read, and say they do not know
    let old: Failure =
        serde_json::from_value(serde_json::json!("the runtime failed: 503")).unwrap();
    assert_eq!(old.kind, ErrorKind::Unknown);
    assert_eq!(old.to_string(), "the runtime failed: 503");
    let later: Failure = serde_json::from_value(
        serde_json::json!({ "kind": "melted", "message": "the endpoint melted" }),
    )
    .unwrap();
    assert_eq!(later.kind, ErrorKind::Unknown);
}

#[test]
fn a_run_that_measured_nothing_never_displaces_one_that_did() {
    // note: this cost a model, which is why it is a test. A re-run of `x-ai/grok-4.6` stopped
    // after nine requests on a provider budget limit. Being that model's *newest* report it
    // replaced a completed run of two hundred and fifty-eight, the model left the pooled table as
    // a dash, and the cohort silently became five - announced only by a parenthesis reading "1 not
    // measured". A sweep is meant to be re-run in pieces when cells fail, so a failed re-run is
    // the expected case and the analysis has to survive it.
    let good = report_of("x-ai/grok-4.6", 1_000, true);
    let failed_rerun = report_of("x-ai/grok-4.6", 9_999, false);

    let picked = per_model([(failed_rerun.clone(), "new"), (good.clone(), "old")]);

    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].2, "old", "the newer report measured nothing");
    assert!(picked[0].1.surface().is_measurable());

    // and among reports that *did* measure, the newest still wins - the rule adds a precondition
    // to recency, it does not replace it
    let newer_good = report_of("x-ai/grok-4.6", 2_000, true);
    let picked = per_model([(good, "old"), (newer_good, "newer")]);
    assert_eq!(picked[0].2, "newer");

    // one row per model, not one per report
    let two = per_model([
        (report_of("a/one", 1, true), "x"),
        (report_of("a/one", 2, true), "y"),
        (report_of("b/two", 1, true), "z"),
    ]);
    assert_eq!(two.len(), 2);
}

/// An outcome with one precondition met and one not, and a claim to read beside them.
fn checked(held: bool, what: &str) -> nachalnik_eval::Outcome {
    let subject = Subject::new(Kernel::new(Config::default()));
    let trial = nachalnik_eval::Trial::new("any", &subject);
    trial.check("the copies agree with each other", true, "4 agreed 100%");
    trial.check(what, held, "3 agreed 50% over 2 replicate(s)");
    trial.resolve(Resolution::new(
        Kind::Counterfactual,
        Answer::yes(true),
        Answer::yes(false),
    ));
    nachalnik_eval::Outcome::of(&trial, None)
}

/// A report's pooled line is scored from every claim its steps carry.
#[test]
fn the_pooled_line_of_a_report_is_the_scores_of_every_claim_it_carries() {
    let report = report_of("x-ai/grok-4.6", 1_000, true);

    let pooled = report.scores();

    assert_eq!(pooled.n, 2, "both endpoint claims are carried");
    assert_eq!(pooled.correct, 1);
    assert_eq!(pooled.accuracy, 0.5);
    assert!(!pooled.is_empty());
    // and it is the line the report ends with
    let rendered = report.to_string();
    assert!(rendered.contains("pooled: 1/2 right (50%"), "{rendered}");
    assert!(!rendered.contains("pooled: nothing measured"), "{rendered}");
}

/// A precondition that did not hold is printed with its figures, and one that held is not.
///
/// note: an unmet check says the scores beside it measure nothing, and `compare` reads these
/// lines to warn a reader off a run.
#[test]
fn a_precondition_that_did_not_hold_is_printed_and_one_that_did_is_not() {
    let unmet = checked(false, "the subject used the handles it was given");
    let rendered = unmet.to_string();

    assert!(rendered.contains("unmet:"), "{rendered}");
    assert!(
        rendered.contains(
            "the subject used the handles it was given: 3 agreed 50% over 2 \
                          replicate(s)"
        ),
        "the unmet check is printed with the figures behind it: {rendered}"
    );
    assert!(
        !rendered.contains("the copies agree with each other"),
        "a check that held is not an unmet one and must not be printed as one: {rendered}"
    );

    // and with none unmet, the word does not appear at all
    let all_met = checked(true, "the subject used the handles it was given");
    assert!(!all_met.to_string().contains("unmet:"), "{}", all_met);
}

#[test]
fn one_model_through_two_providers_is_one_model() {
    // a sign test counts models as independent, and the same weights reached two ways are not
    let direct = report_through("https://api.example/v1", "a/one", 1, true);
    let routed = report_through("https://router.example/v1", "a/one", 2, true);
    assert_eq!(direct.model(), routed.model());
    assert_ne!(direct.served_by(), routed.served_by());

    let picked = per_model([(direct, "direct"), (routed, "routed")]);
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].0, "a/one");
    assert_eq!(picked[0].1.served_by(), Some("https://router.example/v1"));
}

#[test]
fn a_model_level_claim_needs_the_models_to_agree_and_says_so_when_they_do_not() {
    // note: the arithmetic behind P1, hand-checked, because the whole point of registering "five
    // of six is not a result" in advance is that it cannot be renegotiated once five of six is
    // what arrived. Six unanimous is `(1/2)^6`; five of six is `7/64`
    let unanimous = Cohort::over([0.42, 0.31, 0.55, 0.30, 0.61, 0.38].map(Some), 0.30);
    assert_eq!((unanimous.measurable, unanimous.agreed), (6, 6));
    assert_eq!(unanimous.p_value, Some(0.015_625));
    assert!(unanimous.is_unanimous());

    let five = Cohort::over([0.42, 0.31, 0.55, 0.12, 0.61, 0.38].map(Some), 0.30);
    assert_eq!((five.measurable, five.agreed), (6, 5));
    assert_eq!(five.p_value, Some(0.109_375));
    assert!(!five.is_unanimous());

    // the threshold is the registered effect and not zero: every one of these is positive and
    // none of them is the claim that was made
    let positive = Cohort::over([0.04, 0.11, 0.02, 0.09, 0.06, 0.01].map(Some), 0.30);
    assert_eq!(positive.agreed, 0);
    assert_eq!(positive.p_value, Some(1.0));

    // and a model that could not be measured is not a model that disagreed
    let gated = Cohort::over([Some(0.42), None, Some(0.55), None, Some(0.61)], 0.30);
    assert_eq!((gated.models, gated.measurable, gated.agreed), (5, 3, 3));
    assert_eq!(gated.p_value, Some(0.125));
    assert!(gated.to_string().contains("2 not measured"));

    let none = Cohort::over([None, None], 0.30);
    assert!(!none.is_measurable());
    assert!(!none.is_unanimous());
    assert_eq!(none.p_value, None);
}

#[test]
fn a_note_full_of_figures_is_not_a_note_that_matters() {
    // note: the design property the salience hypothesis rests on, and it did not hold until v4.
    // Every inert note in the whole set had three digits or fewer, so "contains figures" and
    // "changes the answer" were confounded across all six dossiers: a subject that answered from
    // the surface would have scored well and a benchmark that cannot be failed measures nothing.
    // Each dossier now carries a numeric red herring, and this is what says so
    for dossier in ALL_DOSSIERS {
        let numeric_inert = dossier
            .notes
            .iter()
            .filter(|n| n.expected == Expected::Holds && carries_a_figure(n.text))
            .count();
        let numeric_moves = dossier
            .notes
            .iter()
            .filter(|n| n.expected == Expected::Moves && carries_a_figure(n.text))
            .count();
        assert!(
            numeric_inert >= 1,
            "{}: no note full of figures that does nothing, so nothing here can tell a subject \
             reading the surface from one reading the arithmetic",
            dossier.name
        );
        assert!(numeric_moves >= 1, "{}", dossier.name);
    }

    // and across the set the feature must not be a shortcut to the answer. The bar is not a round
    // number: a subject that claimed "every note with a figure matters and no note without one
    // does" must score *worse* than the subjects themselves did, or "the subject did better than
    // the shortcut" is not a sentence the data can support. The pilots' subjects managed 0.76
    // against the truth, and with one red herring per dossier the shortcut managed 0.83 - it
    // outscored them, which is what made the second herring necessary rather than tidy
    let notes: Vec<_> = ALL_DOSSIERS.iter().flat_map(|d| d.notes.iter()).collect();
    let shortcut = notes
        .iter()
        .filter(|n| carries_a_figure(n.text) == (n.expected == Expected::Moves))
        .count();
    let accuracy = shortcut as f64 / notes.len() as f64;
    assert!(
        accuracy < 0.76,
        "reading the figures alone scores {accuracy:.2} against the truth over {} notes, which \
         is at least as good as the subjects managed - so a subject that did nothing else would \
         be indistinguishable from one that did the arithmetic",
        notes.len()
    );

    // note: the cell this does *not* fix, recorded rather than glossed. Exactly one note in the
    // whole set changes the answer without carrying a figure - `kiln/vaga-closure` - so the
    // mirror of the red herring, a boring note that turns out to be load-bearing, rests on one
    // observation per model. Writing five more means authoring five new causal structures that
    // flip an answer without arithmetic, which is a dossier change that has to be verified
    // against a real model rather than asserted here
    let quiet_and_decisive = notes
        .iter()
        .filter(|n| n.expected == Expected::Moves && !carries_a_figure(n.text))
        .count();
    assert!(quiet_and_decisive >= 1, "the mirror cell is empty");
}

#[test]
fn every_dossier_is_internally_consistent() {
    for dossier in ALL_DOSSIERS {
        let name = dossier.name;

        // panics if the decisive label is not one of its own notes
        let pivot = dossier.pivot();
        assert_eq!(pivot.label, dossier.decisive, "{name}");

        assert!(
            dossier.among.contains(&dossier.answer),
            "{name}: the answer `{}` is not among {:?}",
            dossier.answer,
            dossier.among
        );

        let labels: BTreeSet<_> = dossier.notes.iter().map(|note| note.label).collect();
        assert_eq!(
            labels.len(),
            dossier.notes.len(),
            "{name}: duplicate labels"
        );

        // a battery needs both kinds or it cannot measure anything: all-moves and "yes" scores
        // a hundred percent, all-holds and "no" does
        assert!(
            dossier.notes.iter().any(|n| n.expected == Expected::Moves)
                && dossier.notes.iter().any(|n| n.expected == Expected::Holds),
            "{name}: a dossier needs notes of both kinds"
        );

        let battery = dossier.battery(dossier.notes.len());
        assert_eq!(battery.first(), Some(&dossier.decisive), "{name}");
        assert_eq!(
            battery.iter().collect::<BTreeSet<_>>().len(),
            battery.len(),
            "{name}: a note asked about twice"
        );
        assert!(
            battery.iter().all(|label| labels.contains(label)),
            "{name}: the battery asks about a note that was never planted"
        );
    }
}

#[test]
fn the_five_dossiers_the_preregistration_asks_for_are_enough_for_forty_items() {
    // the primary endpoint needs forty paired items and no dossier has forty notes, so the count
    // is a property of the *set*. Thirty-two is the preregistered floor for a model to be
    // analysed at all
    let items: usize = ALL_DOSSIERS.iter().map(|dossier| dossier.notes.len()).sum();

    assert!(ALL_DOSSIERS.len() >= 5, "at least five materials");
    assert!(
        items >= 40,
        "the set offers {items} items, the plan asks for 40"
    );
}

#[test]
fn one_dossier_is_built_so_that_the_report_and_the_ablation_must_disagree() {
    // the falsification case, pinned so that a later tidy-up cannot quietly make it ordinary.
    // `guidance` carries no data and is expected to move the answer; `yards` carries the only
    // thing standing between the tables and the right answer, and is expected to move nothing
    assert!(!MILL.tractable);
    assert_eq!(MILL.decisive, "records/guidance");
    assert_eq!(MILL.pivot().expected, Expected::Moves);

    let yards = MILL
        .notes
        .iter()
        .find(|note| note.label == "records/yards")
        .expect("the buried correction is planted");
    assert_eq!(yards.expected, Expected::Holds);

    // and it is the only one of its kind: the others must stay ordinary, or there is no baseline
    // for the dissociation to be a dissociation against
    for dossier in ALL_DOSSIERS.iter().filter(|d| d.name != MILL.name) {
        assert!(dossier.tractable, "{} should be tractable", dossier.name);
    }
}

#[test]
fn every_planted_falsehood_fits_the_dossier_it_was_written_for() {
    assert!(
        PLANTED.len() >= 5,
        "five rungs of one item each is not a pattern"
    );

    let mut seen = BTreeSet::new();
    for (dossier, plant) in PLANTED {
        let name = dossier.name;
        assert!(seen.insert(name), "{name} is planted twice");

        // the premise of the repair ladder is a subject that was fooled and is then unfooled, so
        // material a competent reader gets wrong anyway cannot carry it
        assert!(dossier.tractable, "{name} is not tractable");
        assert_ne!(name, MILL.name);

        // `notes/...` rather than `records/...`, because the brief makes the records
        // authoritative and a falsehood dressed as a record would have no fact of the matter
        assert!(plant.label.starts_with("notes/"), "{name}: {}", plant.label);
        assert!(
            !dossier.notes.iter().any(|note| note.label == plant.label),
            "{name}: the plant collides with a real note"
        );

        // it has to contradict something, and its correction has to say something else
        assert!(
            !plant.text.is_empty() && plant.text != plant.correction,
            "{name}"
        );
        assert!(
            plant.text.contains("Checked and confirmed"),
            "{name}: a note that does not claim to have been verified is weighed against the \
             records rather than believed"
        );
    }
}

#[test]
fn every_disagreement_has_two_sides_its_dossier_can_tell_apart() {
    assert!(
        RIFTS.len() >= 5,
        "a detection rate measured over one contradiction is a fact about that contradiction"
    );

    let mut seen = BTreeSet::new();
    for (dossier, rift) in RIFTS {
        let name = dossier.name;
        assert!(seen.insert(name), "{name} is disputed twice");

        // the arm that scores a task answer needs material a competent reader gets right, which
        // is the same reason `PLANTED` leaves `MILL` out
        assert!(dossier.tractable, "{name} is not tractable");
        assert_ne!(name, MILL.name);

        // `records/...` on both sides, which is the whole difference from `PLANTED`: the brief
        // makes the records authoritative, so two of them that disagree are equally authoritative
        // and nothing in the context settles which to believe
        assert!(rift.label.starts_with("records/"), "{name}: {}", rift.label);
        assert!(
            rift.against.starts_with("records/"),
            "{name}: {}",
            rift.against
        );
        assert!(
            !dossier.notes.iter().any(|note| note.label == rift.label),
            "{name}: the planted return collides with a real note"
        );

        // it contradicts a note the dossier has, and is planted after another one - a rift whose
        // `after` named nothing would be appended, and last is the one position the material is
        // written to avoid
        let has = |label: &str| dossier.notes.iter().any(|note| note.label == label);
        assert!(has(rift.against), "{name}: {} is not a note", rift.against);
        assert!(has(rift.after), "{name}: {} is not a note", rift.after);
        assert_ne!(rift.after, rift.against, "{name}: planted on top of itself");

        // three notes after the one it contradicts, and never last. Adjacent, the two sides are
        // read as a pair and the detection question is nearly free; last, noticing a
        // contradiction is confounded with noticing the most recent thing in the context, which
        // is the confound the red herrings are scattered to avoid
        let at = |label: &str| {
            dossier
                .notes
                .iter()
                .position(|note| note.label == label)
                .expect("the label is a note of this dossier")
        };
        let planted = at(rift.after) + 1;
        assert_eq!(planted - at(rift.against), 3, "{name}: planted {planted}");
        assert!(
            planted < dossier.notes.len(),
            "{name}: planted last, where recency and contradiction cannot be told apart"
        );

        // and the two sides support different answers, or every ablation over the pair measures
        // nothing at all
        assert!(
            dossier.among.contains(&rift.settles),
            "{name}: {} is not an answer this question has",
            rift.settles
        );
        assert_ne!(
            rift.settles, dossier.answer,
            "{name}: both sides of the disagreement support the same answer"
        );
        assert_eq!(
            rift.against, dossier.decisive,
            "{name}: the disputed side has to be the note the dossier turns on, or `settles` is \
             not what the surviving notes support"
        );
    }
}

#[test]
fn a_turn_that_was_cut_off_is_not_a_wrong_answer() {
    // note: `deepseek/deepseek-v4-flash-0731` spent 15,374 reasoning tokens under an 8,192-token
    // ceiling and returned an empty message with `finish_reason: length`. Read as an unreadable
    // claim that would have scored 0/1 against a perfectly readable outcome, which charges a
    // model for the harness's budget
    let cut = Resolution::new(Kind::Task, Answer::Cut, Answer::Choice("kirov".to_owned()));
    assert!(!cut.measured, "nothing was asserted, so nothing was tested");
    assert!(!cut.correct);

    // where a subject *did* say something and simply would not commit, it is measured and wrong:
    // that is the subject's failure and not the harness's
    let refused = Resolution::new(
        Kind::Task,
        Answer::Unreadable,
        Answer::Choice("kirov".to_owned()),
    );
    assert!(refused.measured);
    assert!(!refused.correct);

    let scores = Scores::over(&[cut, refused]);
    assert_eq!((scores.n, scores.correct), (1, 0));
    assert_eq!((scores.unmeasured, scores.cut), (1, 1));
    assert!(format!("{scores}").contains("raise --max-tokens"));
}
