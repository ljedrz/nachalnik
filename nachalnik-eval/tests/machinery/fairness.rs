//! What a question needs to be fair.

use nachalnik_eval::{Answer, Experiment, Probe, Reading, Subject, Trial, async_trait, suite};

/// An experiment written outside this crate, which the trait says needs neither an instrument nor
/// a list of templates to compile.
struct ThirdParty;

#[async_trait]
impl Experiment for ThirdParty {
    fn name(&self) -> &str {
        "third-party"
    }

    async fn run(&self, _subject: &Subject, _trial: &Trial) -> nachalnik_eval::Result<()> {
        Ok(())
    }
}

#[test]
fn a_question_that_needs_an_address_comes_with_a_way_to_look() {
    // note: this test exists because the suite got it wrong and nothing caught it. `attribution`
    // asks "What number is the note labelled X in your own context?" of a subject running under
    // `script::BRIEF`, which tells it in as many words that it has no tools - and grants it no
    // `inspect`, so it has no way to see the numbering it is being asked about. The projector
    // renders items as `label:` and their content; item numbers appear nowhere. Six models scored
    // 2 of 108 on it, an order of magnitude *below* the majority baseline for the row, and the
    // null was written up as a fact about models until somebody asked what the subject could
    // actually see.
    //
    // note: the rule is mechanical and worth stating as one. Every other probe in the suite is
    // answerable by reasoning about the notes; `LOCATION` is the only one that needs an *address*
    // rather than an argument, and an address has to be granted. So: a template list carrying
    // `LOCATION` must carry `handles::INSPECT` as well.
    let unanswerable: Vec<String> = suite::all()
        .iter()
        .filter(|experiment| {
            let asks = experiment.asks();
            asks.contains(&suite::script::LOCATION) && !asks.contains(&suite::handles::INSPECT)
        })
        .map(|experiment| experiment.name().to_owned())
        .collect();

    // note: this listed `attribution` and `lie` until v5, and pinning them rather than asserting
    // empty is what made the fix a deliberate act: turning the probe off failed this test, which
    // is what a fence is for. Both now default to `locating(false)`, the two digests moved, and
    // `VERSION` went with them. An experiment that turns it back on has to install the handles.
    assert!(
        unanswerable.is_empty(),
        "these ask for an item number and grant no handle to look at the numbering: {unanswerable:?}. \
         Either grant `handles::INSPECT` or leave `locating` off - a subject cannot answer from \
         where it sits, and v4 scored the resulting noise as a finding"
    );
}

#[test]
fn every_experiment_states_the_templates_it_asks() {
    // note: `asks` and `instrument` must be the same list or the fence above measures nothing:
    // a location probe left out of the template list is invisible to it *and* to the digest.
    // This checks that the list is there to be read. That the digest covers it is held by
    // `the_instrument_is_pinned_so_that_it_cannot_change_quietly`: every experiment's
    // `instrument` reads `asks()`, so a template dropped from what it fingerprints moves a pinned
    // digest. Nothing catches a new experiment whose `instrument` reads a second list.
    for experiment in suite::all() {
        let stated = experiment.asks();
        assert!(
            !stated.is_empty(),
            "{} states no templates, so nothing can be checked about what it asks",
            experiment.name()
        );
        for template in stated {
            assert!(
                !template.trim().is_empty(),
                "{} lists an empty template",
                experiment.name()
            );
        }
    }
}

/// An experiment that states no instrument is reported as unstated, and asks nothing.
///
/// note: `instrument` is read back into every report, so an unstated one has to say so rather
/// than read as blank; and a default `asks` naming any template would put one of this crate's own
/// into a third party's list.
///
/// note: and a default `about` naming anything at all puts a claim into a third party's report
/// that names something this experiment knows nothing about. Empty is what an experiment that has
/// not said what it measures has to read back as.
#[test]
fn an_experiment_that_states_nothing_asks_nothing_and_is_told_so() {
    let unstated = ThirdParty.instrument();

    assert!(!unstated.is_stated());
    assert_eq!(unstated, Default::default());
    assert_eq!(unstated.to_string(), "an unstated instrument");

    let asked = ThirdParty.asks();

    assert!(asked.is_empty(), "{asked:?}");

    assert_eq!(ThirdParty.about(), "");
}

/// An experiment of this crate's says what it measures, and says it as a line.
///
/// note: the empty default is right for a third-party experiment, which has not said what it
/// measures, and wrong for one this crate wrote: every experiment in `suite::all` is run by
/// somebody who did not read the source, and this line is what they get instead of the source. A
/// blank there reads as an experiment that measures nothing, and a bare word reads as no better.
/// The check is that there is a line and that it is words rather than a token - what it says is
/// the suite's to say.
#[test]
fn an_experiment_of_this_crates_says_what_it_measures() {
    for experiment in suite::all() {
        let about = experiment.about().trim();

        assert!(
            !about.is_empty(),
            "{} states nothing about what it measures, which is what an experiment that has not \
             said anything reads as",
            experiment.name()
        );
        assert!(
            about.contains(' '),
            "{} measures things in one word: {about}",
            experiment.name()
        );
        assert!(
            !about.contains('\n'),
            "{} measures things in more than the one line: {about}",
            experiment.name()
        );
    }
}

/// What the suite calls each of its experiments.
///
/// note: pinned, because a name is what a report is filed under and what `bench -e` selects by,
/// so an experiment renamed is one the runs that measured it cannot be read back by. A rename is
/// somebody's decision, and this is where it says so.
#[test]
fn the_suite_names_each_of_its_experiments() {
    let names: Vec<String> = suite::all()
        .iter()
        .map(|experiment| experiment.name().to_owned())
        .collect();

    assert_eq!(
        names,
        [
            "attribution",
            "recursion",
            "lie",
            "conflict",
            "provenance",
            "privilege",
            "instrumented",
            "repair",
            "feedback",
        ]
    );
}

#[test]
fn a_bare_confidence_of_one_is_read_as_certainty_and_that_is_a_choice() {
    // note: pinned rather than fixed. `CONFIDENCE:` is asked for on a 0-100 scale, and the reader
    // treats anything at or below 1.0 as a fraction - so a subject answering `1` meaning "one
    // percent" is recorded as certain. Both readings are defensible for a bare `1` and neither is
    // knowable from the line alone.
    //
    // note: it never happened. Across both collections every confidence recorded is between 33
    // and 100 and none is a bare 0 or 1, so no figure in the study depends on which way this
    // goes. It is pinned here so that changing it is a decision somebody takes on purpose: the
    // digest covers the *questions* and not the reading of the answers, so a quiet edit here
    // would change what old records mean without moving any fingerprint.
    let claim = |said: &str| Probe::new("q", Reading::Claim).read(said);

    assert_eq!(
        claim("ANSWER: yes\nCONFIDENCE: 1"),
        Answer::Claim {
            yes: true,
            confidence: Some(1.0)
        },
        "a bare 1 reads as certainty, not as one percent"
    );
    assert_eq!(
        claim("ANSWER: yes\nCONFIDENCE: 0.9"),
        Answer::Claim {
            yes: true,
            confidence: Some(0.9)
        },
        "a fraction is taken as written"
    );
    assert_eq!(
        claim("ANSWER: yes\nCONFIDENCE: 90"),
        Answer::Claim {
            yes: true,
            confidence: Some(0.9)
        },
        "a percentage is divided"
    );
    assert_eq!(
        claim("ANSWER: yes\nCONFIDENCE: 150"),
        Answer::Claim {
            yes: true,
            confidence: Some(1.0)
        },
        "and one over a hundred is clamped rather than refused"
    );
    // a claim with no readable confidence is still a claim: it is scored for accuracy and left
    // out of the calibration figures, which is what `Scores::scored` counts
    assert_eq!(
        claim("ANSWER: no\nCONFIDENCE:"),
        Answer::Claim {
            yes: false,
            confidence: None
        },
    );
}
