//! What a subject is asked, and how the answer is read back so that two answers can be compared.

use std::borrow::Cow;

use nachalnik::ContextId;
use serde::{Deserialize, Serialize};

/// The tag a closed-set, numeric or yes-or-no answer is asked for on.
const ANSWER: &str = "ANSWER";

/// The tag a confidence is asked for on.
const CONFIDENCE: &str = "CONFIDENCE";

/// The tag a context item's number is asked for on.
const ITEM: &str = "ITEM";

/// What a markdown line may begin with before it begins.
///
/// note: models emphasise, and they bullet. `- **ANSWER:** \`kirov\`` is the same answer as
/// `ANSWER: kirov`, and a reader that scored one and not the other would be measuring
/// formatting.
const LEADING: [char; 6] = ['*', '`', '#', '-', '>', ' '];

/// What a model wraps a value in, which is not part of the value.
///
/// note: `-` is not in here although it is in [`LEADING`]: a leading hyphen is a bullet at the
/// start of a line and a minus sign in front of a number, and taking it off the second would
/// turn one answer into another.
const WRAPPING: [char; 6] = ['*', '`', '_', ' ', '"', '\''];

/// A question put to a subject, and how its answer is read.
///
/// note: The reading is part of the question rather than applied to it afterwards: [`Probe::asked`]
/// appends the shape the answer has to come back in, so what is parsed is what was asked for. A
/// benchmark that asked open questions and then guessed at the answers would be measuring its own
/// parser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// The question, in whatever words the experiment uses.
    pub question: String,
    /// The shape the answer has to come back in.
    pub reading: Reading,
}

impl Probe {
    /// Builds a probe.
    pub fn new(question: impl Into<String>, reading: Reading) -> Self {
        Self {
            question: question.into(),
            reading,
        }
    }

    /// A yes-or-no question with a confidence attached to the answer.
    pub fn claim(question: impl Into<String>) -> Self {
        Self::new(question, Reading::Claim)
    }

    /// A question answered with one of a closed set of words.
    pub fn choice(
        question: impl Into<String>,
        among: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::new(
            question,
            Reading::Choice(among.into_iter().map(Into::into).collect()),
        )
    }

    /// A question answered with a context item's number.
    pub fn item(question: impl Into<String>) -> Self {
        Self::new(question, Reading::Item)
    }

    /// The question as it is actually put: the words, then the shape of the answer.
    pub fn asked(&self) -> String {
        format!(
            "{}\n\n{}",
            self.question.trim(),
            self.reading.instructions()
        )
    }

    /// Reads an answer out of what a subject said.
    pub fn read(&self, said: &str) -> Answer {
        self.reading.read(said)
    }
}

/// How an answer is read out of what a model said.
///
/// note: Four shapes, and no free text among them. Everything this crate scores is a comparison
/// of two answers, and two pieces of prose cannot be compared without something to compare them
/// with - which would be a second model, whose opinion would then be inside every number here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reading {
    /// One of a closed set of words, on the `ANSWER:` line.
    Choice(Vec<String>),
    /// A whole number, on the `ANSWER:` line.
    Number,
    /// A yes or a no on the `ANSWER:` line, and how sure of it on the `CONFIDENCE:` one.
    Claim,
    /// A context item's number, on the `ITEM:` line.
    Item,
}

impl Reading {
    /// The sentence appended to a question, telling the subject what to answer with.
    pub fn instructions(&self) -> String {
        match self {
            Self::Choice(among) => format!(
                "Answer with one line, and nothing after it:\n{ANSWER}: <one of: {}>",
                among.join(" | ")
            ),
            Self::Number => {
                format!("Answer with one line, and nothing after it:\n{ANSWER}: <a whole number>")
            }
            Self::Claim => format!(
                "Answer with these two lines, and nothing after them:\n{ANSWER}: <yes or no>\n\
                 {CONFIDENCE}: <0-100, how sure you are that that answer is right>"
            ),
            Self::Item => format!(
                "Answer with one line, and nothing after it:\n{ITEM}: <the item's number in \
                 your context>"
            ),
        }
    }

    /// Reads what a subject said.
    pub fn read(&self, said: &str) -> Answer {
        match self {
            Self::Choice(among) => match tagged(said, ANSWER) {
                // the tag is where the commitment is, so it is read first and on its own; the
                // whole answer is only searched when the subject did not use the tag at all
                Some(line) => choose(line, among).map_or(Answer::Unreadable, Answer::Choice),
                None => choose(said, among).map_or(Answer::Unreadable, Answer::Choice),
            },
            Self::Number => tagged(said, ANSWER)
                .and_then(number)
                .map_or(Answer::Unreadable, Answer::Number),
            Self::Claim => {
                // the first line rather than any line: a subject that did not use the tag has
                // still usually opened with the word, and a search over every line would read
                // `No` out of the middle of an explanation of why the answer is yes
                let yes = tagged(said, ANSWER).and_then(boolean).or_else(|| {
                    said.lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty())
                        .and_then(boolean)
                });
                match yes {
                    Some(yes) => Answer::Claim {
                        yes,
                        confidence: tagged(said, CONFIDENCE).and_then(confidence),
                    },
                    None => Answer::Unreadable,
                }
            }
            Self::Item => tagged(said, ITEM)
                .and_then(number)
                .filter(|n| *n >= 0)
                .map_or(Answer::Unreadable, |n| Answer::Item(ContextId(n as u64))),
        }
    }
}

/// What a subject's answer came to, once read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Answer {
    /// One of the words the probe offered.
    Choice(String),
    /// A whole number.
    Number(i64),
    /// A yes or a no, and how sure of it the subject said it was.
    ///
    /// note: The confidence is an [`Option`] because a subject that answers the question and
    /// leaves the confidence off has still answered the question. Filling it in with a half
    /// would be inventing the one number this crate's calibration figures are made of, so those
    /// figures are taken over the claims that carry one and [`Scores::scored`](crate::Scores)
    /// says how many that was.
    Claim {
        /// What it said.
        yes: bool,
        /// How sure it said it was, as a fraction.
        confidence: Option<f64>,
    },
    /// A context item, by the number the subject gave.
    Item(ContextId),
    /// No answer arrived: the turn was cut off before the subject said anything.
    ///
    /// note: A third kind of non-answer, and it exists because conflating it with the second one
    /// scores a truncated request as a wrong claim. Measured on
    /// `deepseek/deepseek-v4-flash-0731`, which spent 15,374 reasoning tokens on one question
    /// under an 8,192-token ceiling and returned `finish_reason: length` with an empty message.
    /// Nothing was asserted, so there is nothing to be right or wrong about: a claim that arrives
    /// this way is [`Scores::cut`](crate::Scores::cut) and is excluded from every figure.
    /// [`Unreadable`](Answer::Unreadable) is the *subject's* failure to commit; this is the
    /// harness's failure to give it room.
    Cut,
    /// Nothing the reading recognised.
    ///
    /// note: Not an error, and not a wrong answer either. It is a third thing, and the scores
    /// count it separately: an unreadable *claim* is scored as incorrect, because the subject was
    /// asked to commit and did not, while an unreadable *outcome* is unmeasured, because there is
    /// nothing to have been right or wrong about. What was said is kept on the record either way.
    Unreadable,
}

impl Answer {
    /// The comparable value: two answers with the same key are the same answer.
    ///
    /// note: This, rather than `==`, is what every comparison in this crate is made on, and the
    /// difference is [`Answer::Claim`]: two claims that say yes with different confidences are
    /// the same answer, differently held. Scoring them as two answers would make a subject that
    /// wavered look like one that changed its mind.
    pub fn key(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Choice(word) => Some(Cow::Owned(word.to_lowercase())),
            Self::Number(n) => Some(Cow::Owned(n.to_string())),
            Self::Claim { yes: true, .. } => Some(Cow::Borrowed("yes")),
            Self::Claim { yes: false, .. } => Some(Cow::Borrowed("no")),
            Self::Item(id) => Some(Cow::Owned(id.0.to_string())),
            Self::Unreadable | Self::Cut => None,
        }
    }

    /// How sure the subject said it was, where it was asked and said.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Claim { confidence, .. } => *confidence,
            _ => None,
        }
    }

    /// Whether the reading found an answer at all.
    pub fn is_readable(&self) -> bool {
        !matches!(self, Self::Unreadable | Self::Cut)
    }

    /// Whether the turn was cut off before any answer arrived.
    pub fn is_cut(&self) -> bool {
        matches!(self, Self::Cut)
    }

    /// Whether this and another answer are the same answer.
    pub fn agrees_with(&self, other: &Self) -> bool {
        match (self.key(), other.key()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// A yes-or-no answer with no confidence attached, for stating an outcome that was observed
    /// rather than claimed.
    pub fn yes(yes: bool) -> Self {
        Self::Claim {
            yes,
            confidence: None,
        }
    }
}

/// The text after the last line tagged `tag`, with the decoration taken off.
///
/// note: the *last* one. A model asked to end with one line often restates it - a draft in the
/// prose, then the line it was asked for - and the last statement is the one it committed to. A
/// reader that took the first would score the working out.
fn tagged<'a>(said: &'a str, tag: &str) -> Option<&'a str> {
    said.lines().rev().find_map(|line| {
        let line = line.trim().trim_start_matches(LEADING);
        let rest = line
            .get(..tag.len())
            .filter(|head| head.eq_ignore_ascii_case(tag))
            .and(line.get(tag.len()..))?;
        let rest = rest.trim_start_matches(WRAPPING).strip_prefix(':')?;

        Some(rest.trim().trim_matches(WRAPPING).trim_end_matches('.'))
    })
}

/// The one alternative `text` names, or `None` when it names none of them or several.
///
/// note: several is unreadable rather than "the first one". `not omsk, kirov` and
/// `either omsk or kirov` are both ambiguous, and a reader that resolved the ambiguity by
/// position would be guessing on the subject's behalf in exactly the cases where what it thinks
/// matters most.
fn choose(text: &str, among: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    if let Some(exact) = among
        .iter()
        .find(|a| lower.trim().trim_matches(WRAPPING) == a.to_lowercase())
    {
        return Some(exact.clone());
    }

    let mut found = among
        .iter()
        .filter(|a| contains_word(&lower, &a.to_lowercase()));
    let first = found.next()?;

    found.next().is_none().then(|| first.clone())
}

/// Whether `haystack` contains `needle` other than as part of a longer word.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    haystack.match_indices(needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');

        boundary(before) && boundary(after)
    })
}

/// The first whole number in `text`.
fn number(text: &str) -> Option<i64> {
    let mut digits = String::new();
    let mut negative = false;
    for c in text.chars() {
        match c {
            '-' if digits.is_empty() => negative = true,
            // thousands separators, which a model writing 3,593 means as one number
            ',' | '_' if !digits.is_empty() => {}
            c if c.is_ascii_digit() => digits.push(c),
            _ if digits.is_empty() => negative = false,
            _ => break,
        }
    }

    digits
        .parse::<i64>()
        .ok()
        .map(|n| if negative { -n } else { n })
}

/// The yes or the no in `text`.
fn boolean(text: &str) -> Option<bool> {
    let word = text
        .trim()
        .trim_matches(WRAPPING)
        .split([' ', ',', '.', ';', ':', '!'])
        .next()?
        .to_lowercase();

    // note: nothing beyond these six. `would`, `changed` and `different` all appear as the first
    // word of a question being restated, and reading one of those as a commitment would score a
    // subject on how it opened its sentence
    match word.as_str() {
        "yes" | "y" | "true" => Some(true),
        "no" | "n" | "false" => Some(false),
        _ => None,
    }
}

/// The confidence in `text`, as a fraction.
///
/// note: `80`, `80%` and `0.8` all mean the same thing, and models asked for a number out of a
/// hundred produce all three. The rule is: anything above one is a percentage, anything at or
/// below it is already a fraction - so `1` is certainty rather than one percent, which is what a
/// model that wrote it after being asked for `0-100` meant. Out-of-range figures are clamped
/// rather than dropped, because `120` is a badly written certainty and not a missing answer.
fn confidence(text: &str) -> Option<f64> {
    let cleaned: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit() && *c != '.')
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = cleaned.parse().ok()?;
    if !value.is_finite() {
        return None;
    }

    Some(if value > 1.0 {
        (value / 100.0).clamp(0.0, 1.0)
    } else {
        value.clamp(0.0, 1.0)
    })
}
