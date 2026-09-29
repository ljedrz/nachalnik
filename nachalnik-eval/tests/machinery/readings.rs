//! What an answer is read as.

use nachalnik::ContextId;
use nachalnik_eval::{Answer, Probe, Reading};

#[test]
fn a_choice_is_read_off_the_tag() {
    let probe = Probe::choice("which?", ["kirov", "omsk", "tara"]);

    assert_eq!(
        probe.read("working: omsk fills fast.\nANSWER: kirov"),
        Answer::Choice("kirov".to_owned())
    );
    // the decoration models put round their own answers
    assert_eq!(
        probe.read("- **ANSWER:** `tara`."),
        Answer::Choice("tara".to_owned())
    );
    // the last one wins: a draft in the prose, then the line that was asked for
    assert_eq!(
        probe.read("ANSWER: omsk\n\nno, wait.\nANSWER: tara"),
        Answer::Choice("tara".to_owned())
    );
}

#[test]
fn a_choice_with_no_tag_falls_back_and_ambiguity_does_not() {
    let probe = Probe::choice("which?", ["kirov", "omsk", "tara"]);

    assert_eq!(probe.read("kirov"), Answer::Choice("kirov".to_owned()));
    assert_eq!(
        probe.read("it has to be kirov"),
        Answer::Choice("kirov".to_owned())
    );
    // two of the alternatives and no tag is not an answer, and must not be resolved by position
    assert_eq!(probe.read("either omsk or kirov"), Answer::Unreadable);
    assert_eq!(probe.read("no idea"), Answer::Unreadable);
    // a word that merely contains an alternative is not that alternative
    assert_eq!(probe.read("kirovsk"), Answer::Unreadable);
}

#[test]
fn a_claim_carries_its_confidence_in_whichever_form_it_arrives() {
    let probe = Probe::claim("would it change?");

    assert_eq!(
        probe.read("ANSWER: yes\nCONFIDENCE: 90"),
        Answer::Claim {
            yes: true,
            confidence: Some(0.9)
        }
    );
    assert_eq!(
        probe.read("ANSWER: no\nCONFIDENCE: 0.8"),
        Answer::Claim {
            yes: false,
            confidence: Some(0.8)
        }
    );
    assert_eq!(
        probe.read("ANSWER: yes\nCONFIDENCE: 100%"),
        Answer::Claim {
            yes: true,
            confidence: Some(1.0)
        }
    );
    // asked for a number out of a hundred, `1` is certainty rather than one percent
    assert_eq!(
        probe.read("ANSWER: yes\nCONFIDENCE: 1"),
        Answer::Claim {
            yes: true,
            confidence: Some(1.0)
        }
    );
    // the question was answered; the confidence was not, and is not invented
    assert_eq!(
        probe.read("No, it would not."),
        Answer::Claim {
            yes: false,
            confidence: None
        }
    );
    assert_eq!(probe.read("it depends"), Answer::Unreadable);
}

#[test]
fn an_item_number_is_read_and_a_confidence_is_not_a_key() {
    assert_eq!(
        Probe::item("which item?").read("ITEM: 7"),
        Answer::Item(ContextId(7))
    );
    assert_eq!(
        Probe::item("which item?").read("**ITEM:** item 12 (the AGENTS.md read)"),
        Answer::Item(ContextId(12))
    );
    assert_eq!(
        Probe::item("which item?").read("the third one"),
        Answer::Unreadable
    );

    // two claims that say the same thing with different conviction are the same answer, said
    // with different conviction
    let sure = Answer::Claim {
        yes: true,
        confidence: Some(0.99),
    };
    let unsure = Answer::Claim {
        yes: true,
        confidence: Some(0.51),
    };
    assert_ne!(sure, unsure);
    assert!(sure.agrees_with(&unsure));
    assert!(!Answer::Unreadable.agrees_with(&Answer::Unreadable));
}

#[test]
fn the_shape_of_the_answer_is_part_of_the_question() {
    let probe = Probe::choice("which?", ["kirov", "omsk"]);
    let asked = probe.asked();

    assert!(asked.starts_with("which?"));
    assert!(asked.contains("ANSWER: <one of: kirov | omsk>"));
    assert!(Reading::Claim.instructions().contains("CONFIDENCE"));
}

#[test]
fn an_answer_naming_two_of_the_options_commits_to_neither() {
    // note: the reading requires a unique match, so a subject hedging across two options is
    // unreadable rather than scored on whichever came first in the list - and an option that is
    // a prefix of a longer word is not a match at all
    let among = ["kirov", "omsk", "ufa"].map(str::to_owned).to_vec();
    let choice = |said: &str| Probe::new("q", Reading::Choice(among.clone())).read(said);

    assert_eq!(choice("ANSWER: omsk"), Answer::Choice("omsk".to_owned()));
    assert_eq!(choice("ANSWER: kirov or omsk"), Answer::Unreadable);
    assert_eq!(
        choice("it is not kirov\nANSWER: ufa"),
        Answer::Choice("ufa".to_owned()),
        "the tagged line is read on its own, so prose above it cannot outvote it"
    );
    assert_eq!(
        choice("ANSWER: kirovsk"),
        Answer::Unreadable,
        "an option inside a longer word is not that option"
    );
}
