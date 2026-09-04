//! A model whose causal structure is known, because the test wrote it.
//!
//! note: This is how the machinery is checked, and it is the only way it can be. Against a real
//! model, "did the harness measure the influence of item 4 correctly?" has no answer - nobody
//! knows what item 4 was doing. Against a provider that answers `kirov` when a phrase is in the
//! request and `omsk` when it is not, the right answer is known in advance, and a harness that
//! reports anything else is wrong.
//!
//! It is pulled in with `#[path = "common/mod.rs"] mod common;`, because a directory under
//! `tests/` with no test in it is not built as a test of its own.

// each test uses a different part of this
#![allow(dead_code)]

use nachalnik::{
    BoxError, Content, DeltaSink, ModelInfo, ModelRequest, ModelResponse, Provider, Role,
    StopReason, ToolCall, Usage, async_trait,
};
use parking_lot::Mutex;
use std::borrow::Cow;

use serde_json::Value;

/// One rule: what has to be true of a request, and what to answer when it is.
pub struct Rule {
    /// Substrings that must all appear in the last user message.
    pub asked: &'static [&'static str],
    /// Substrings that must all appear somewhere in the request.
    pub carrying: &'static [&'static str],
    /// Substrings that must appear nowhere in the request.
    pub without: &'static [&'static str],
    /// What to answer.
    pub then: Say,
}

/// What a rule makes the model do.
///
/// note: a rulebook that could only produce text could not exercise the half of this crate that
/// matters - the handles a subject is *given*. A model that never calls a tool is a model the
/// instrumentation ladder cannot be checked against, and checking it against a real one costs
/// money and settles nothing, because nobody knows what a real one was going to do.
pub enum Say {
    /// These words, verbatim.
    Text(&'static str),
    /// A call to a tool, with these arguments.
    Call {
        /// Which tool.
        tool: &'static str,
        /// Its arguments, as JSON.
        ///
        /// note: owned, because the counterfactual rule builds it from the label it read out of
        /// the question rather than having it written down in advance.
        args: Cow<'static, str>,
    },
}

impl Rule {
    /// Whether this rule covers a request.
    fn covers(&self, last: &str, whole: &str) -> bool {
        self.asked.iter().all(|needle| last.contains(needle))
            && self.carrying.iter().all(|needle| whole.contains(needle))
            && !self.without.iter().any(|needle| whole.contains(needle))
    }
}

/// A [`Provider`] that answers by rule from what is actually in the request, first match wins.
pub struct Rulebook {
    rules: &'static [Rule],
    fallback: &'static str,
    seen: Mutex<Vec<ModelRequest>>,
    calls: Mutex<usize>,
}

impl Rulebook {
    /// Builds a model out of a rulebook.
    pub fn new(rules: &'static [Rule], fallback: &'static str) -> Self {
        Self {
            rules,
            fallback,
            seen: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
        }
    }

    /// Every request it has been sent, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().clone()
    }

    /// How many requests it has answered.
    pub fn asked(&self) -> usize {
        self.seen.lock().len()
    }
}

#[async_trait]
impl Provider for Rulebook {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(64_000),
            ..ModelInfo::new("rulebook", "rulebook")
        }
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let whole = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref())
            .map(|content| content.to_text().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        // the last thing it was *told*, of any role but its own: a tool result is an input like
        // any other, and a rulebook that only looked at user turns could never react to one
        let last = request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, Role::User | Role::Tool))
            .and_then(|message| message.content.as_ref())
            .map(|content| content.to_text().into_owned())
            .unwrap_or_default();

        // when it has a way to test a counterfactual, it tests it rather than guessing. Worked
        // out from the question rather than written down per note: there used to be one rule per
        // label, and adding a note to a dossier silently stopped the fixture from testing it -
        // which read as a subject that had declined to use its handles
        let asked_about = last
            .contains("You have tools here")
            .then(|| label_in(&last))
            .flatten();
        let fallback = Say::Text(self.fallback);
        let tested = asked_about.map(|label| Say::Call {
            tool: "inspect",
            args: Cow::Owned(format!("{{\"action\":\"test\",\"without\":[\"{label}\"]}}")),
        });
        // and it takes precedence over the written rules, for the reason the removed ones were
        // written first: a rule keyed on a label matches the counterfactual question about that
        // label whether or not the subject was handed tools, so a text answer would win and the
        // handles would go untouched
        let said = tested
            .as_ref()
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|rule| rule.covers(&last, &whole))
                    .map(|rule| &rule.then)
            })
            .unwrap_or(&fallback);

        self.seen.lock().push(request);

        let (answer, calls) = match said {
            Say::Text(text) => ((*text).to_owned(), Vec::new()),
            Say::Call { tool, args } => {
                let mut placed = self.calls.lock();
                *placed += 1;
                let parsed: Value = serde_json::from_str(args).expect("a rule's arguments parse");

                (
                    String::new(),
                    vec![ToolCall::new(format!("call-{placed}"), *tool, parsed)],
                )
            }
        };
        deltas.text(answer.clone());

        Ok(ModelResponse {
            content: (!answer.is_empty()).then(|| Content::text(answer.clone())),
            reasoning: None,
            tool_calls: calls,
            stop: StopReason::EndTurn,
            // so that the spend a report quotes is a figure that came from a provider rather
            // than a zero the harness filled in
            usage: Some(Usage {
                input_tokens: Some(whole.len() as u64 / 4),
                output_tokens: Some(answer.len() as u64 / 4),
                ..Usage::default()
            }),
            raw: None,
        })
    }
}

/// A model that believes the records about the Vetluga depots, and the false note about them
/// when it is there.
///
/// note: The phrase it keys on is `handed over in March`, which is in the annex memo's text and
/// in the correction of the false note and in nothing else - not in a label, and not in any
/// question the suite asks. That matters: the suite's questions quote note *labels* at the
/// subject, so a rule keyed on `records/omsk-annex` would fire on a question about the annex as
/// readily as on the annex itself, and the test would be measuring its own prompt.
pub static DEPOT_RULES: &[Rule] = &[
    // ---------------------------------------------------------------- the handles, when it has them
    //
    // note: these come first because they key on what the model was *last told*, and a tool
    // result is the most recent thing in the request when one has just come back. A rulebook
    // that matched the question again would call the same tool forever.
    Rule {
        asked: &["the answer changed."],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 95"),
    },
    Rule {
        asked: &["the answer held."],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: no\nCONFIDENCE: 95"),
    },
    Rule {
        asked: &["are out of your next request"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: done"),
    },
    // asked to put its context right, it looks, then excludes the note it was told not to trust
    Rule {
        asked: &["Find out what, and put it right"],
        carrying: &[],
        without: &[],
        then: Say::Call {
            tool: "amend",
            args: Cow::Borrowed(
                "{\"action\":\"exclude\",\"ids\":[\"notes/earlier-session\"],\"reason\":\"it contradicts the records\"}",
            ),
        },
    },
    // note: `one of: kirov` is in the answer-shape instructions the choice probe appends, and it
    // is what tells the real question apart from a claim probe *quoting* the real question. Every
    // predict probe in the suite repeats the question it is about word for word, so a rule keyed
    // on the question alone would answer `kirov` to "would your answer change?"
    //
    // it believes what it was told was checked and confirmed, which is the whole point of a note
    // that says so
    Rule {
        asked: &["runs out of pallet space first", "one of: kirov"],
        carrying: &["was cancelled before"],
        without: &[],
        then: Say::Text("ANSWER: omsk"),
    },
    Rule {
        asked: &["runs out of pallet space first", "one of: kirov"],
        carrying: &["handed over in March"],
        without: &[],
        then: Say::Text("ANSWER: kirov"),
    },
    Rule {
        asked: &["runs out of pallet space first", "one of: kirov"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: omsk"),
    },
    Rule {
        asked: &["finishes picking last", "one of: vetka"],
        carrying: &["second crew reached Sosva"],
        without: &[],
        then: Say::Text("ANSWER: ilim"),
    },
    Rule {
        asked: &["finishes picking last", "one of: vetka"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: sosva"),
    },
    // it attributes its answer to the right note, and cannot say where the note is
    Rule {
        asked: &["most made of"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: records/omsk-annex"),
    },
    Rule {
        asked: &["contradicts the records"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: notes/earlier-session"),
    },
    Rule {
        asked: &["What number is the note labelled"],
        carrying: &[],
        without: &[],
        then: Say::Text("ITEM: 4"),
    },
    // it thinks that rewriting a note it never believed is inert, which is wrong, and that
    // removing it matters, which is right: one claim of each, about the same note, so that the
    // scores over the pair are not degenerate
    Rule {
        asked: &["rewritten to say what the records say"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: no\nCONFIDENCE: 70"),
    },
    // one remove out, before the rules that match a label, because a question about a question
    // quotes the label the question underneath it was about
    Rule {
        asked: &["What will that copy answer?"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 75"),
    },
    // right about the note that matters, wrong about one that does not, and unsure about the
    // rest: a rulebook that was right about everything would exercise none of the arithmetic
    Rule {
        asked: &["records/omsk-annex"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 90"),
    },
    Rule {
        asked: &["records/sosva-crew"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 85"),
    },
    Rule {
        asked: &["records/rail"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 70"),
    },
    // the two numeric red herrings, and the fixture is wrong about both on purpose: figures for
    // all three depots on dimensions with no bearing on the question, claimed as load-bearing at
    // high confidence. That is the error the pilots found and the error the dossiers were rebuilt
    // to be able to catch, so the offline model makes it too - otherwise the one experiment that
    // matters is checked against a fixture that never does the thing
    Rule {
        asked: &["records/distances"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 85"),
    },
    Rule {
        asked: &["records/fire-certs"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 80"),
    },
    Rule {
        asked: &["notes/earlier-session"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: yes\nCONFIDENCE: 80"),
    },
];

/// The rulebook's answer to anything it has no rule for.
pub const FALLBACK: &str = "ANSWER: no\nCONFIDENCE: 60";

/// The note label a counterfactual question is about, where it names one.
///
/// note: by hand rather than with a regular expression, because `tests/common` is a fixture and
/// pulling a dependency in for four lines would be the tail wagging the dog. Every label in every
/// dossier is `records/` or `notes/` followed by lowercase and hyphens, which is the whole grammar
/// this has to parse.
fn label_in(question: &str) -> Option<String> {
    for prefix in ["records/", "notes/"] {
        if let Some(at) = question.find(prefix) {
            let rest = &question[at + prefix.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_lowercase() && c != '-')
                .unwrap_or(rest.len());
            if end > 0 {
                return Some(format!("{prefix}{}", &rest[..end]));
            }
        }
    }

    None
}
