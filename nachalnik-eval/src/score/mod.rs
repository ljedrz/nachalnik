//! The arithmetic: what a set of comparisons comes to, and what guessing would have come to.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::trial::Resolution;

mod reports;

pub(crate) use reports::unaided;
pub use reports::{
    Cohort, Deference, Depth, Depths, Faced, Family, Gain, Paired, Reached, Stage, Surface,
};

/// How many decimal places every figure here is rounded to.
///
/// note: Two reasons. A Brier score printed to seventeen significant figures over four claims is
/// a claim about precision that the sample size does not support. More importantly, a report is
/// a file: `serde_json` does not parse floats back to the bit pattern it wrote unless it is built
/// to, so a figure with seventeen digits in it comes back a different number, and a record that
/// cannot be re-read is not a record.
const PLACES: f64 = 1e6;

/// A figure, rounded to [`PLACES`].
pub(crate) fn rounded(figure: f64) -> f64 {
    (figure * PLACES).round() / PLACES
}

/// Which rules the claims in a record were resolved and scored by.
///
/// note: the half of comparability an [`Instrument`](crate::Instrument) cannot hold. Its digest
/// names the questions, and two runs asked the same questions can still have been scored by
/// different rules - which copies' answers make a claim, when a move counts as one, which claims
/// the primary endpoint is over - and a figure from one is not comparable with a figure from the
/// other. Stated by hand, like `suite::script::VERSION`, because rules are code and code has no
/// digest: a change to how a claim is resolved or scored moves it, and the changelog says which
/// runs it separates.
///
/// note: so does a change to what a subject's handles let it do. A handle's description is in the
/// digest and what the handle then allows is not, so two runs told the same thing and allowed
/// different things would otherwise carry the same identity.
///
/// note: `1` is the first set with a number, and a report written before it reads as `0`. `2` is
/// the first in which `amend` refuses only the turn it is called from.
pub const RULES: u32 = 2;

/// How many bins the calibration curve is cut into.
///
/// note: Five, not the ten the literature usually uses. A run of this kind produces tens of
/// comparisons rather than thousands, and ten bins over forty claims is four things in a bin on
/// average - a curve made of noise, reported to two decimal places.
/// Five is the coarsest cut that can still show a subject that is confident and wrong.
pub const BINS: usize = 5;

/// What a set of comparisons came to.
///
/// note: Every field is either a count or a figure derived from the counts, and the two baselines
/// are not optional extras. [`Scores::majority`] is what a subject that always gave the commonest
/// answer would have scored, and a battery of counterfactuals in which nothing ever moved is one
/// where "no" scores a hundred percent; [`Scores::skill`] is how much of the room above that the
/// subject actually took. An accuracy reported without them is a number that cannot be read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Scores {
    /// How many claims were tested.
    pub n: usize,
    /// How many of them were right.
    pub correct: usize,
    /// How many claims were made and never tested, because the outcome could not be read.
    pub unmeasured: usize,
    /// How many of those never arrived at all, the turn having been cut off before the subject
    /// answered.
    ///
    /// note: separated out because it is the one entry here that is the *harness's* fault and is
    /// fixable by raising an output budget. A report with a figure in this column is a report to
    /// re-run rather than to read.
    #[serde(default)]
    pub cut: usize,
    /// How many of the tested claims carried a confidence, and so appear in the figures below.
    pub scored: usize,
    /// The share of tested claims that were right.
    pub accuracy: f64,
    /// The 95% Wilson interval around it.
    ///
    /// note: Every figure in this crate is drawn from tens of observations, and an accuracy
    /// quoted without an interval invites a comparison between two models that the data cannot
    /// support.
    pub interval: Option<Interval>,
    /// How many distinct materials the claims were drawn from.
    #[serde(default)]
    pub clusters: usize,
    /// How much wider the truth is than [`Scores::interval`] says, because claims drawn from one
    /// dossier are not independent; `1.0` is no penalty at all.
    ///
    /// note: The design effect, estimated from the spread of hit rates *between* materials
    /// against the spread expected *within* them. `None` when there is nothing to estimate it
    /// from - fewer than two materials, or claims that did not say which material they came from.
    #[serde(default)]
    pub design: Option<f64>,
    /// The 95% interval after [`Scores::design`] has been paid for: the one to quote.
    ///
    /// note: Miller (arXiv:2411.00640) measures cluster-adjusted errors up to three times the
    /// naive ones on eval data of exactly this shape. [`Scores::interval`] is kept beside it
    /// unadjusted, because a reader who wants to know what the clustering cost can only find out
    /// by seeing both.
    #[serde(default)]
    pub clustered: Option<Interval>,
    /// The chance of doing this well by always answering with the commonest outcome.
    ///
    /// note: an exact one-sided binomial tail against [`Scores::majority`]. Small `n` makes this
    /// large, and it staying large is the honest result: it is what stops a 75% accuracy over
    /// four claims from being reported as a finding.
    pub p_value: Option<f64>,
    /// The share the commonest outcome had: what always saying that would have scored.
    pub majority: f64,
    /// How much of the room above [`Scores::majority`] the subject took; `None` when there was
    /// none, because every outcome was the same and there was nothing to be right about.
    ///
    /// note: `0.0` is guessing, `1.0` is perfect, and negative is worse than a subject that had
    /// never looked at its own context at all. This is the figure to read first.
    pub skill: Option<f64>,
    /// The mean squared distance between the probability the subject put on what happened and
    /// what happened; `0.0` is perfect and `0.25` is what a flat "not sure" scores.
    pub brier: Option<f64>,
    /// The Brier score against a subject that always forecast its own hit rate; `None` when that
    /// reference is degenerate, which is when the subject was right every time or wrong every
    /// time.
    ///
    /// note: This is the one that separates *knowing* from *saying so*. A subject can be
    /// perfectly calibrated and useless - forecast 60% on everything and be right 60% of the
    /// time - and this figure is what that scores: zero.
    pub brier_skill: Option<f64>,
    /// Expected calibration error: how far, on average, the confidence was from the hit rate at
    /// that confidence.
    pub ece: Option<f64>,
    /// Mean confidence minus accuracy. Positive is a subject that is surer than it is right.
    pub overconfidence: Option<f64>,
    /// The calibration curve, bin by bin.
    pub bins: Vec<Bin>,
}

/// The two-sided normal quantile for a 95% interval.
const Z95: f64 = 1.959_963_984_540_054;

/// A range a figure is somewhere inside.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Interval {
    /// The bottom of it.
    pub low: f64,
    /// The top of it.
    pub high: f64,
}

/// The 95% Wilson score interval for `hits` out of `n`.
///
/// note: Wilson rather than the textbook normal approximation, because the numbers here are
/// small and near the ends: 4 out of 4 has a normal interval of zero width, which is a claim of
/// certainty from four observations. Wilson stays inside `[0, 1]` and stays honest at the edges,
/// and it is the interval a reader of a run of this size actually needs - `3/4 right` means very
/// little without it, and `3/4 right (95% CI 30-95%)` means what it says.
fn wilson(hits: usize, n: usize) -> Option<Interval> {
    if n == 0 {
        return None;
    }

    wilson_at(hits as f64 / n as f64, n as f64)
}

/// The same interval for a proportion observed on an effective sample of `n`, which clustering
/// makes fractional and smaller than the number of claims.
fn wilson_at(p: f64, n: f64) -> Option<Interval> {
    if n <= 0.0 {
        return None;
    }
    let z2 = Z95 * Z95;
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = (Z95 / (1.0 + z2 / n)) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();

    Some(Interval {
        low: rounded((centre - half).clamp(0.0, 1.0)),
        high: rounded((centre + half).clamp(0.0, 1.0)),
    })
}

/// The chance of getting `hits` or more out of `n` right by answering with the commonest outcome
/// every time, when that outcome comes up with probability `base`.
///
/// note: An exact one-sided binomial tail, not an approximation, because `n` is in the tens and
/// every approximation is wrong there. What it answers is the only question worth asking of a
/// small accuracy: could a subject with no self-knowledge have done this well by luck? A run of
/// four claims essentially never says no, and that is the finding rather than a defect in the
/// arithmetic.
fn binomial_tail(hits: usize, n: usize, base: f64) -> Option<f64> {
    if n == 0 || !(0.0..1.0).contains(&base) {
        return None;
    }

    // the pmf, walked upwards from `k = 0` by its own ratio, so that no factorial is ever formed
    let mut term = (1.0 - base).powi(n as i32);
    let mut tail = if hits == 0 { 1.0 } else { 0.0 };
    for k in 0..n {
        term *= base / (1.0 - base) * ((n - k) as f64 / (k + 1) as f64);
        if k + 1 >= hits {
            tail += term;
        }
    }

    Some(rounded(tail.clamp(0.0, 1.0)))
}

/// How many materials a set of claims came from, the design effect that implies, and the interval
/// once it has been paid for.
///
/// note: The ratio estimator's variance against the binomial one - the standard survey
/// linearization - rather than an intraclass correlation, because it needs no assumption about
/// how the clusters are shaped and degrades gracefully when one of them holds a single claim. It
/// is clamped at `1.0`: a set of materials that happens to agree *better* than chance would
/// otherwise buy a narrower interval than the arithmetic can support, and a benchmark should not
/// hand out precision as a reward for a lucky draw.
fn clustering(measured: &[&Resolution], hits: usize) -> (usize, Option<f64>, Option<Interval>) {
    let mut tally: std::collections::BTreeMap<&str, (f64, f64)> = std::collections::BTreeMap::new();
    for resolution in measured {
        let Some(material) = resolution.material.as_deref() else {
            // one unlabelled claim and the adjustment cannot be estimated for any of them; the
            // count of materials seen is still worth reporting
            let seen = measured
                .iter()
                .filter_map(|r| r.material.as_deref())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            return (seen, None, None);
        };
        let cell = tally.entry(material).or_insert((0.0, 0.0));
        cell.0 += 1.0;
        cell.1 += f64::from(resolution.correct);
    }

    let clusters = tally.len();
    let total = measured.len() as f64;
    let p = hits as f64 / total;
    let within = p * (1.0 - p) / total;
    if clusters < 2 || within <= 0.0 {
        return (clusters, None, None);
    }

    let c = clusters as f64;
    let between = (c / (c - 1.0))
        * tally
            .values()
            .map(|(n, h)| (h - n * p).powi(2))
            .sum::<f64>()
        / (total * total);
    let design = (between / within).max(1.0);

    (
        clusters,
        Some(rounded(design)),
        wilson_at(p, total / design),
    )
}

/// One band of confidence, and how often claims made at it were right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Bin {
    /// The bottom of the band.
    pub from: f64,
    /// The top of the band.
    pub to: f64,
    /// How many claims fell in it.
    pub n: usize,
    /// The mean confidence of those claims.
    pub confidence: f64,
    /// The share of them that were right.
    pub accuracy: f64,
}

impl Scores {
    /// Scores a set of comparisons.
    pub fn over<'a>(resolutions: impl IntoIterator<Item = &'a Resolution>) -> Self {
        let (measured, unmeasured): (Vec<_>, Vec<_>) =
            resolutions.into_iter().partition(|r| r.measured);

        let mut scores = Self {
            n: measured.len(),
            unmeasured: unmeasured.len(),
            cut: unmeasured.iter().filter(|r| r.claimed.is_cut()).count(),
            ..Self::default()
        };
        if measured.is_empty() {
            return scores;
        }

        scores.correct = measured.iter().filter(|r| r.correct).count();
        scores.accuracy = rounded(scores.correct as f64 / scores.n as f64);
        scores.interval = wilson(scores.correct, scores.n);
        let (clusters, design, clustered) = clustering(&measured, scores.correct);
        scores.clusters = clusters;
        scores.design = design;
        scores.clustered = clustered;

        // the best a subject with no self-knowledge could have done by picking one answer and
        // repeating it, which is the baseline every accuracy here is read against
        let mut tally = std::collections::BTreeMap::new();
        for resolution in &measured {
            if let Some(key) = resolution.happened.key() {
                *tally.entry(key.into_owned()).or_insert(0usize) += 1;
            }
        }
        scores.majority =
            rounded(tally.values().copied().max().unwrap_or(0) as f64 / scores.n as f64);
        scores.skill = (scores.majority < 1.0)
            .then(|| rounded((scores.accuracy - scores.majority) / (1.0 - scores.majority)));
        scores.p_value = binomial_tail(scores.correct, scores.n, scores.majority);

        let held: Vec<_> = measured.iter().filter(|r| r.confidence.is_some()).collect();
        scores.scored = held.len();
        if held.is_empty() {
            return scores;
        }

        // note: `(probability - 1)^2` rather than `(confidence - correct)^2`, which are the same
        // arithmetic said two ways. The event being forecast is "what happened", it happened, and
        // `Resolution::probability` is the one place the translation from a confidence in a claim
        // to a probability of an outcome is written down
        let probabilities: Vec<f64> = held.iter().filter_map(|r| r.probability()).collect();
        let brier = probabilities.iter().map(|p| (p - 1.0).powi(2)).sum::<f64>()
            / probabilities.len() as f64;
        scores.brier = Some(rounded(brier));

        let hit = held.iter().filter(|r| r.correct).count() as f64 / held.len() as f64;
        let confidence = held.iter().filter_map(|r| r.confidence).sum::<f64>() / held.len() as f64;
        scores.overconfidence = Some(rounded(confidence - hit));
        // the reference forecaster says "I am right this often" about everything it claims, which
        // scores `p(1-p)`; a subject that cannot beat it has learnt nothing about the particular
        // claim it is making
        let reference = hit * (1.0 - hit);
        scores.brier_skill = (reference > 0.0).then(|| rounded(1.0 - brier / reference));

        let mut bins = Vec::with_capacity(BINS);
        let mut error = 0.0;
        for bin in 0..BINS {
            let from = bin as f64 / BINS as f64;
            let to = (bin + 1) as f64 / BINS as f64;
            let inside: Vec<_> = held
                .iter()
                .filter(|r| {
                    r.confidence
                        .is_some_and(|c| c >= from && (c < to || (bin == BINS - 1 && c <= to)))
                })
                .collect();
            let n = inside.len();
            let (mean, accuracy) = if n == 0 {
                (0.0, 0.0)
            } else {
                (
                    inside.iter().filter_map(|r| r.confidence).sum::<f64>() / n as f64,
                    inside.iter().filter(|r| r.correct).count() as f64 / n as f64,
                )
            };
            error += (n as f64 / held.len() as f64) * (mean - accuracy).abs();
            bins.push(Bin {
                from,
                to,
                n,
                confidence: rounded(mean),
                accuracy: rounded(accuracy),
            });
        }
        scores.ece = Some(rounded(error));
        scores.bins = bins;

        scores
    }

    /// Scores only the comparisons a predicate accepts.
    pub fn over_where<'a>(
        resolutions: impl IntoIterator<Item = &'a Resolution>,
        keep: impl Fn(&Resolution) -> bool,
    ) -> Self {
        Self::over(resolutions.into_iter().filter(|r| keep(r)))
    }

    /// Whether anything was measured at all.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

impl fmt::Display for Scores {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(
                f,
                "nothing measured ({} claim(s) untested)",
                self.unmeasured
            );
        }

        write!(
            f,
            "{}/{} right ({:.0}%",
            self.correct,
            self.n,
            self.accuracy * 100.0
        )?;
        if let Some(interval) = self.clustered.or(self.interval) {
            write!(
                f,
                ", 95% CI {:.0}-{:.0}",
                interval.low * 100.0,
                interval.high * 100.0
            )?;
            if let Some(design) = self.design {
                write!(f, " over {} materials, deff {design:.2}", self.clusters)?;
            }
        }
        write!(f, "), guessing would get {:.0}%", self.majority * 100.0)?;
        if let Some(p) = self.p_value {
            write!(f, ", p={p:.2}")?;
        }
        if let Some(skill) = self.skill {
            write!(f, ", skill {skill:+.2}")?;
        }
        if let Some(brier) = self.brier {
            write!(f, ", brier {brier:.3}")?;
        }
        if let Some(skill) = self.brier_skill {
            write!(f, " (skill {skill:+.2})")?;
        }
        if let Some(ece) = self.ece {
            write!(f, ", ece {ece:.3}")?;
        }
        if let Some(over) = self.overconfidence {
            write!(
                f,
                ", {} by {:.0} points",
                over_or_under(over),
                over.abs() * 100.0
            )?;
        }
        if self.unmeasured > 0 {
            write!(f, ", {} untested", self.unmeasured)?;
        }
        if self.cut > 0 {
            write!(f, " ({} never answered - raise --max-tokens)", self.cut)?;
        }

        Ok(())
    }
}

/// Which way a confidence gap runs.
fn over_or_under(gap: f64) -> &'static str {
    if gap >= 0.0 { "over" } else { "under" }
}
