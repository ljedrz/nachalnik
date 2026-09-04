//! The arithmetic: what a set of comparisons comes to, and what guessing would have come to.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    probe::Answer,
    trial::{Act, Kind, Resolution, Step},
};

/// How many decimal places every figure here is rounded to.
///
/// note: Two reasons, and the second is the load-bearing one. A Brier score printed to seventeen
/// significant figures over four claims is a claim about precision that the sample size does not
/// support. And a report is a file: `serde_json` does not parse floats back to the bit pattern it
/// wrote unless it is built to, so a figure with seventeen digits in it comes back a different
/// number and a record that cannot be re-read is not a record.
const PLACES: f64 = 1e6;

/// A figure, rounded to [`PLACES`].
fn rounded(figure: f64) -> f64 {
    (figure * PLACES).round() / PLACES
}

/// How many bins the calibration curve is cut into.
///
/// note: Five, not the ten the literature usually uses. A run of this kind produces tens of
/// comparisons rather than thousands, and ten bins over forty claims is eight bins with three
/// things in them and two with nothing - a curve made of noise, reported to two decimal places.
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
    /// note: not decoration. Every figure in this crate is drawn from tens of observations, and
    /// an accuracy quoted without an interval invites a comparison between two models that the
    /// data cannot support.
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

/// What being told how it was doing did to a subject's self-model.
///
/// note: The two halves are not the same claims asked twice - a subject asked again about an item
/// it has just been told the answer for is being tested on its memory. They are two batteries of
/// the same shape over different material, which is the only comparison that means anything and
/// is also why the figure is noisy: a difference of one claim in six is four points of accuracy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gain {
    /// Before it was told anything.
    pub before: Scores,
    /// After.
    pub after: Scores,
}

impl Gain {
    /// Scores a set of comparisons on either side of the feedback.
    pub fn over<'a>(resolutions: impl IntoIterator<Item = &'a Resolution> + Clone) -> Self {
        Self {
            before: Scores::over_where(resolutions.clone(), |r| !r.informed),
            after: Scores::over_where(resolutions, |r| r.informed),
        }
    }

    /// How much more often it was right; positive is better.
    pub fn accuracy(&self) -> f64 {
        rounded(self.after.accuracy - self.before.accuracy)
    }

    /// How much more of the room above guessing it took; positive is better.
    pub fn skill(&self) -> Option<f64> {
        Some(rounded(self.after.skill? - self.before.skill?))
    }

    /// How much its Brier score improved; positive is better, because a Brier score is a
    /// distance and improving it means making it smaller.
    pub fn brier(&self) -> Option<f64> {
        Some(rounded(self.before.brier? - self.after.brier?))
    }

    /// How much its calibration improved; positive is better, for the same reason.
    pub fn calibration(&self) -> Option<f64> {
        Some(rounded(self.before.ece? - self.after.ece?))
    }

    /// Whether there is anything on both sides to compare.
    pub fn is_measurable(&self) -> bool {
        !self.before.is_empty() && !self.after.is_empty()
    }
}

impl fmt::Display for Gain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return write!(
                f,
                "no comparison: before {}, after {}",
                self.before, self.after
            );
        }

        write!(
            f,
            "accuracy {:+.0} points",
            (self.after.accuracy - self.before.accuracy) * 100.0
        )?;
        if let Some(brier) = self.brier() {
            write!(f, ", brier {brier:+.3}")?;
        }
        if let Some(calibration) = self.calibration() {
            write!(f, ", ece {calibration:+.3}")?;
        }

        Ok(())
    }
}

/// The scores at one remove of self-reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Depth {
    /// How many removes: `1` is a claim about its own next answer, `2` a claim about what a copy
    /// of it would claim, and so on.
    pub depth: usize,
    /// What the claims at that remove came to.
    pub scores: Scores,
}

/// The scores at each remove of self-reference, shallowest first.
///
/// note: What this is for is the shape of the curve rather than any point on it. A subject whose
/// accuracy holds from one remove to three is doing something different from one that is right
/// about itself and wrong about a copy of itself, and the second is much the commoner result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Depths(pub Vec<Depth>);

impl Depths {
    /// Groups a set of comparisons by how deep they went.
    pub fn over<'a>(resolutions: impl IntoIterator<Item = &'a Resolution> + Clone) -> Self {
        let mut depths: Vec<usize> = resolutions.clone().into_iter().map(|r| r.depth).collect();
        depths.sort_unstable();
        depths.dedup();

        Self(
            depths
                .into_iter()
                .map(|depth| Depth {
                    depth,
                    scores: Scores::over_where(resolutions.clone(), |r| r.depth == depth),
                })
                .collect(),
        )
    }

    /// Whether anything was measured at more than one remove.
    pub fn is_recursive(&self) -> bool {
        self.0.len() > 1
    }
}

impl fmt::Display for Depths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, depth) in self.0.iter().enumerate() {
            if n > 0 {
                f.write_str("\n")?;
            }
            write!(
                f,
                "  {:<16}{}",
                format!("depth {}:", depth.depth),
                depth.scores
            )?;
        }

        Ok(())
    }
}

/// The scores at one stage of an experiment.
///
/// note: The third grouping, beside [`Family`] and [`Depth`], and the one the instrumentation
/// ladder is read through: the same claims, asked with no evidence available, with evidence
/// available, and after the subject has acted on it. What is worth reading is the difference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    /// What the stage is called.
    pub name: String,
    /// What its claims came to.
    pub scores: Scores,
}

impl Stage {
    /// Groups a set of comparisons by the stage they were made at, in the order first seen.
    ///
    /// note: first seen rather than sorted, because a ladder's stages have an order and it is
    /// not alphabetical: `reported` comes before `tested` because that is what the experiment
    /// did, and a table that sorted them would put the answer before the question.
    pub fn over<'a>(resolutions: impl IntoIterator<Item = &'a Resolution> + Clone) -> Vec<Self> {
        let mut names: Vec<String> = Vec::new();
        for resolution in resolutions.clone() {
            if let Some(stage) = &resolution.stage
                && !names.iter().any(|seen| seen == stage)
            {
                names.push(stage.clone());
            }
        }

        names
            .into_iter()
            .map(|name| Stage {
                scores: Scores::over_where(resolutions.clone(), |r| {
                    r.stage.as_deref() == Some(name.as_str())
                }),
                name,
            })
            .collect()
    }
}

/// The scores for one family of claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Family {
    /// The family.
    pub kind: Kind,
    /// What its claims came to.
    pub scores: Scores,
}

impl Family {
    /// Groups a set of comparisons by what they were claims about.
    pub fn over<'a>(resolutions: impl IntoIterator<Item = &'a Resolution> + Clone) -> Vec<Self> {
        let mut kinds: Vec<Kind> = resolutions.clone().into_iter().map(|r| r.about).collect();
        kinds.sort_unstable();
        kinds.dedup();

        kinds
            .into_iter()
            .map(|kind| Family {
                kind,
                scores: Scores::over_where(resolutions.clone(), |r| r.about == kind),
            })
            .collect()
    }
}

/// The same claims at two stages, paired item by item.
///
/// note: The primary endpoint of the whole crate, and it has to be paired. `reported` and
/// `retested` are the same subject answering the same questions about the same notes, so treating
/// them as two independent samples throws away the pairing and asks a weaker question than the
/// data can answer. What matters is not that one accuracy is higher: it is *which items moved,
/// and in which direction*. Four items that all improved and none that regressed is a result; four
/// that improved while four others fell over is noise wearing the same average.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    /// The stage the claims were first made at.
    pub before: String,
    /// The stage they were made at again.
    pub after: String,
    /// How many items appear at both stages.
    pub n: usize,
    /// Right at both.
    pub both: usize,
    /// Wrong at both.
    pub neither: usize,
    /// Wrong before, right after: what the later stage bought.
    pub gained: usize,
    /// Right before, wrong after: what it cost.
    pub lost: usize,
    /// The accuracy difference, later stage minus earlier.
    pub difference: f64,
    /// The exact one-sided McNemar p-value against the two directions being equally likely.
    ///
    /// note: exact rather than the chi-squared approximation, for the reason every test in this
    /// file is exact: the discordant count is in single figures, where the approximation is
    /// simply wrong. With no regressions at all it reduces to `(1/2)^gained`, so five items that
    /// improved and none that fell back is `p = 0.031` - and four is `0.063`, which is why the
    /// preregistration asks for forty items rather than four.
    pub p_value: Option<f64>,
}

impl Paired {
    /// Pairs the claims made at two stages by the item they were about.
    ///
    /// note: paired on the material, the *label* and the run it came from, never on the
    /// [`ContextId`](nachalnik::ContextId) - one arm of this comparison runs in a second session,
    /// where the same note is a different item. A claim with no label cannot be paired and is left
    /// out, which the counts show.
    pub fn over<'a>(
        resolutions: impl IntoIterator<Item = &'a Resolution> + Clone,
        before: &str,
        after: &str,
    ) -> Self {
        let at = |stage: &str| {
            resolutions
                .clone()
                .into_iter()
                .filter(|r| r.measured && r.stage.as_deref() == Some(stage))
                .filter_map(|r| {
                    let label = r.label.as_deref()?;
                    let material = r.material.as_deref().unwrap_or_default();

                    Some((
                        (material.to_owned(), label.to_owned(), r.session, r.about),
                        r.correct,
                    ))
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let (first, second) = (at(before), at(after));

        let mut paired = Self {
            before: before.to_owned(),
            after: after.to_owned(),
            n: 0,
            both: 0,
            neither: 0,
            gained: 0,
            lost: 0,
            difference: 0.0,
            p_value: None,
        };
        for (key, was) in &first {
            let Some(now) = second.get(key) else {
                continue;
            };
            paired.n += 1;
            match (was, now) {
                (true, true) => paired.both += 1,
                (false, false) => paired.neither += 1,
                (false, true) => paired.gained += 1,
                (true, false) => paired.lost += 1,
            }
        }
        if paired.n > 0 {
            paired.difference =
                rounded((paired.gained as f64 - paired.lost as f64) / paired.n as f64);
        }
        paired.p_value = binomial_tail(paired.gained, paired.gained + paired.lost, 0.5);

        paired
    }

    /// Whether there was anything at both stages to compare.
    pub fn is_measurable(&self) -> bool {
        self.n > 0
    }
}

impl fmt::Display for Paired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return write!(f, "{} to {}: nothing paired", self.before, self.after);
        }

        write!(
            f,
            "{} to {}: {:+.0} points over {} paired item(s), {} gained, {} lost",
            self.before,
            self.after,
            self.difference * 100.0,
            self.n,
            self.gained,
            self.lost
        )?;
        if let Some(p) = self.p_value {
            write!(f, ", mcnemar p={p:.3}")?;
        }

        Ok(())
    }
}

/// One item where a subject's own experiment bore on a claim it had already made.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Faced {
    /// What it claimed before it could test anything.
    pub claimed: Option<bool>,
    /// What its own experiment showed.
    pub showed: Option<bool>,
    /// What it claimed afterwards.
    pub restated: Option<bool>,
}

/// What a subject did when its own experiment contradicted its own stated theory.
///
/// note: The most distinctive figure here, and the one no other harness can produce: both sides of
/// the conflict are the subject's. The evidence is not retrieved and not supplied by anybody - the
/// model generated it, seconds after stating the claim it contradicts. The knowledge-conflict
/// literature measures external evidence against a parametric prior; this measures a model's
/// measurement against its own account of itself, and a subject that will not believe its own
/// instrument has been given a tool and not a capability.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Deference {
    /// Items where it made a claim, ran a test, and was asked again.
    pub faced: usize,
    /// Of those, where the test disagreed with the standing claim.
    pub conflicts: usize,
    /// Of those, where it went with the test.
    pub deferred: usize,
    /// The share of conflicts it resolved in favour of the evidence.
    pub rate: Option<f64>,
    /// The 95% Wilson interval around that.
    pub interval: Option<Interval>,
}

impl Deference {
    /// Tallies the cases where evidence and a standing claim were both on the record.
    pub fn over<'a>(faced: impl IntoIterator<Item = &'a Faced>) -> Self {
        let mut deference = Self::default();
        for case in faced {
            let (Some(claimed), Some(showed)) = (case.claimed, case.showed) else {
                continue;
            };
            deference.faced += 1;
            if claimed == showed {
                continue;
            }
            deference.conflicts += 1;
            if case.restated == Some(showed) {
                deference.deferred += 1;
            }
        }
        if deference.conflicts > 0 {
            deference.rate = Some(rounded(
                deference.deferred as f64 / deference.conflicts as f64,
            ));
            deference.interval = wilson(deference.deferred, deference.conflicts);
        }

        deference
    }

    /// Whether any conflict arose to be resolved.
    pub fn is_measurable(&self) -> bool {
        self.conflicts > 0
    }
}

impl fmt::Display for Deference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return write!(
                f,
                "no conflict: its tests agreed with its claims on all {} item(s) it tested",
                self.faced
            );
        }

        write!(
            f,
            "{}/{} conflict(s) resolved in favour of the evidence ({:.0}%)",
            self.deferred,
            self.conflicts,
            self.rate.unwrap_or(0.0) * 100.0
        )?;
        if let Some(interval) = self.interval {
            write!(
                f,
                ", 95% CI {:.0}-{:.0}",
                interval.low * 100.0,
                interval.high * 100.0
            )?;
        }

        Ok(())
    }
}

/// What a subject claimed about items that provably do nothing, split by how they read.
///
/// note: the primary endpoint, and the narrowest question in the suite. Every item counted here
/// is one whose ablation *provably did not move the copies* - so the honest claim about all of
/// them is "no". The only thing that differs between the two halves is whether the note carries
/// a figure. A subject reading the arithmetic answers "no" to both; a subject reading the surface
/// answers "yes" to the numeric ones.
///
/// note: restricted to the inert stratum on purpose, and that restriction is what makes it a
/// clean contrast rather than a correlation. Across the material as a whole, notes with figures
/// really are more likely to matter - most of the arithmetic is in them - so an unrestricted
/// comparison would confound the cue with the truth and reward a subject for a lucky prior.
/// Within items that all do nothing there is nothing left to be right about, and a difference
/// between the halves cannot be knowledge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// Inert items whose note carried a figure.
    pub numeric: usize,
    /// Of those, how many the subject claimed were load-bearing.
    pub claimed_numeric: usize,
    /// Inert items whose note carried no figure.
    pub plain: usize,
    /// Of those, how many the subject claimed were load-bearing.
    pub claimed_plain: usize,
    /// Of the numeric ones, those written to have nothing to do with the question.
    pub herrings: usize,
    /// Of those, how many the subject claimed were load-bearing.
    pub claimed_herrings: usize,
    /// Of the numeric ones, those belonging to the question's arithmetic that did not decide it.
    pub arithmetic: usize,
    /// Of those, how many the subject claimed were load-bearing.
    pub claimed_arithmetic: usize,
    /// The share of the numeric ones it claimed.
    pub numeric_rate: Option<f64>,
    /// The share of the plain ones it claimed.
    pub plain_rate: Option<f64>,
    /// The difference, numeric minus plain: the endpoint.
    pub difference: Option<f64>,
    /// The interval on the numeric rate.
    pub numeric_interval: Option<Interval>,
    /// The interval on the plain rate.
    pub plain_interval: Option<Interval>,
    /// The red-herring rate minus the off-pivot-arithmetic rate: P2b's endpoint.
    ///
    /// note: near zero is H1 as stated - the cue is digits, and a deed reference reads as a reason
    /// exactly as a capacity table does. Strongly negative means the subject dismissed the
    /// irrelevant figures and over-claimed only the ones that look like the question's own
    /// arithmetic, which is a different claim and a better-behaved model, and H1 has to be
    /// restated as that rather than reported as this.
    pub discrimination: Option<f64>,
}

impl Surface {
    /// Reads the claims about inert items, splitting them by whether the note carried a figure.
    ///
    /// note: `reads` answers "does this note carry a figure, and was it written to be inert?" for
    /// a material and a label, and is handed in rather than looked up here so that this file knows
    /// nothing about dossiers. An item whose note cannot be found is left out of every count.
    pub fn over<'a>(
        resolutions: impl IntoIterator<Item = &'a Resolution>,
        reads: impl Fn(&str, &str) -> Option<(bool, bool)>,
    ) -> Self {
        let mut surface = Self::default();
        for r in resolutions {
            // provably inert: the copies were readable and they did not move
            if !r.measured || r.about != Kind::Counterfactual {
                continue;
            }
            if r.happened != Answer::yes(false) {
                continue;
            }
            let (Some(material), Some(label)) = (r.material.as_deref(), r.label.as_deref()) else {
                continue;
            };
            let Some((carries, written_inert)) = reads(material, label) else {
                continue;
            };
            let claimed = matches!(r.claimed, Answer::Claim { yes: true, .. });
            match carries {
                true => {
                    surface.numeric += 1;
                    surface.claimed_numeric += usize::from(claimed);
                    match written_inert {
                        true => {
                            surface.herrings += 1;
                            surface.claimed_herrings += usize::from(claimed);
                        }
                        false => {
                            surface.arithmetic += 1;
                            surface.claimed_arithmetic += usize::from(claimed);
                        }
                    }
                }
                false => {
                    surface.plain += 1;
                    surface.claimed_plain += usize::from(claimed);
                }
            }
        }

        let (herrings, claimed_herrings) = (surface.herrings, surface.claimed_herrings);
        let (arithmetic, claimed_arithmetic) = (surface.arithmetic, surface.claimed_arithmetic);
        let mut surface = Self::of(
            surface.claimed_numeric,
            surface.numeric,
            surface.claimed_plain,
            surface.plain,
        );
        surface.herrings = herrings;
        surface.claimed_herrings = claimed_herrings;
        surface.arithmetic = arithmetic;
        surface.claimed_arithmetic = claimed_arithmetic;
        if herrings > 0 && arithmetic > 0 {
            surface.discrimination = Some(rounded(
                claimed_herrings as f64 / herrings as f64
                    - claimed_arithmetic as f64 / arithmetic as f64,
            ));
        }

        surface
    }

    /// The same figure from counts that were pooled elsewhere.
    ///
    /// note: here so that a pooled figure is computed by the same arithmetic as a per-model one
    /// rather than by a caller adding four numbers and dividing. A report that quoted two
    /// differently-derived versions of its own endpoint would be a report nobody should trust.
    pub fn of(claimed_numeric: usize, numeric: usize, claimed_plain: usize, plain: usize) -> Self {
        let mut surface = Self {
            numeric,
            claimed_numeric,
            plain,
            claimed_plain,
            ..Self::default()
        };
        if numeric > 0 {
            surface.numeric_rate = Some(rounded(claimed_numeric as f64 / numeric as f64));
            surface.numeric_interval = wilson(claimed_numeric, numeric);
        }
        if plain > 0 {
            surface.plain_rate = Some(rounded(claimed_plain as f64 / plain as f64));
            surface.plain_interval = wilson(claimed_plain, plain);
        }
        surface.difference = surface
            .numeric_rate
            .zip(surface.plain_rate)
            .map(|(a, b)| rounded(a - b));

        surface
    }

    /// Whether both halves had anything in them to compare.
    pub fn is_measurable(&self) -> bool {
        self.numeric > 0 && self.plain > 0
    }
}

impl fmt::Display for Surface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return write!(
                f,
                "nothing to contrast ({} numeric, {} plain inert item(s))",
                self.numeric, self.plain
            );
        }

        write!(
            f,
            "of items that do nothing, claimed {}/{} of the ones with figures",
            self.claimed_numeric, self.numeric
        )?;
        if let Some(interval) = self.numeric_interval {
            write!(
                f,
                " ({:.0}-{:.0}%)",
                interval.low * 100.0,
                interval.high * 100.0
            )?;
        }
        write!(f, " against {}/{} without", self.claimed_plain, self.plain)?;
        if let Some(interval) = self.plain_interval {
            write!(
                f,
                " ({:.0}-{:.0}%)",
                interval.low * 100.0,
                interval.high * 100.0
            )?;
        }
        if let Some(difference) = self.difference {
            write!(f, ", {:+.0} points", difference * 100.0)?;
        }
        // the split that says what the cue actually is: see `discrimination`
        if let Some(discrimination) = self.discrimination {
            write!(
                f,
                " [red herrings {}/{}, off-pivot arithmetic {}/{}, {:+.0}]",
                self.claimed_herrings,
                self.herrings,
                self.claimed_arithmetic,
                self.arithmetic,
                discrimination * 100.0
            )?;
        }

        Ok(())
    }
}

/// What a set of models agreed about: the sign test the model-level claims are made with.
///
/// note: the only test in the suite that takes a whole run as one observation. Every other figure
/// here is computed over items within a model, where the items share a dossier and a question and
/// are not independent - which is what the cluster adjustment is for. Models are independent of
/// each other in a way items never are, so counting how many of them went the registered way is
/// the one place a plain binomial is honest.
///
/// note: and it is why the cohort size is a preregistered decision rather than a budget outcome.
/// Six models unanimous is `(1/2)^6 = 0.016` and carries a model-level claim; five of six is
/// `0.109` and does not, whatever the five look like. Ten unanimous is `0.001`. A model that could
/// not be measured at all - gated out under §8, or run and failed - is not a model that disagreed,
/// so it leaves the denominator rather than counting against the direction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cohort {
    /// How many models were offered to the test.
    pub models: usize,
    /// How many produced a figure at all.
    pub measurable: usize,
    /// How many of those went the registered way.
    pub agreed: usize,
    /// The share of measurable models that agreed.
    pub rate: Option<f64>,
    /// The exact one-sided sign-test p-value against the two directions being equally likely.
    pub p_value: Option<f64>,
}

impl Cohort {
    /// The sign test over one figure per model, against the threshold that was registered for it.
    ///
    /// note: `at_least` is the registered effect size and not zero, because "the difference was
    /// positive" is a much weaker claim than the one the preregistration makes and the two should
    /// not be reported in the same column. A model whose figure is `None` was not measured.
    pub fn over(figures: impl IntoIterator<Item = Option<f64>>, at_least: f64) -> Self {
        let figures: Vec<Option<f64>> = figures.into_iter().collect();
        let measured: Vec<f64> = figures.iter().copied().flatten().collect();

        let mut cohort = Self {
            models: figures.len(),
            measurable: measured.len(),
            agreed: measured.iter().filter(|f| **f >= at_least).count(),
            ..Self::default()
        };
        if cohort.measurable > 0 {
            cohort.rate = Some(rounded(cohort.agreed as f64 / cohort.measurable as f64));
            cohort.p_value = binomial_tail(cohort.agreed, cohort.measurable, 0.5);
        }

        cohort
    }

    /// Whether any model produced a figure.
    pub fn is_measurable(&self) -> bool {
        self.measurable > 0
    }

    /// Whether the agreement reaches the significance the preregistration asks of it.
    pub fn is_unanimous(&self) -> bool {
        self.measurable > 0 && self.agreed == self.measurable
    }
}

impl fmt::Display for Cohort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return write!(f, "no model of {} produced a figure", self.models);
        }

        write!(f, "{} of {} model(s)", self.agreed, self.measurable)?;
        if self.measurable < self.models {
            write!(f, " ({} not measured)", self.models - self.measurable)?;
        }
        if let Some(p) = self.p_value {
            write!(f, ", sign test p={p:.3}")?;
        }

        Ok(())
    }
}

/// Whether a subject that could have measured actually did.
///
/// note: The gate on everything above it. A model that never calls the handle is not a model with
/// poor instrumented self-knowledge - it is a model that was not measured, and reporting its
/// accuracy beside an instrumented one would put two different experiments in the same column.
/// This is why [`Step::Granted`] exists: the denominator is questions the subject *could* have
/// instrumented, which the record cannot work out from the acts alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reached {
    /// Questions asked at a stage, after handles had been granted.
    pub offered: usize,
    /// Of those, ones the subject did something about.
    pub instrumented: usize,
    /// How many times it listed its own context.
    pub looks: usize,
    /// How many experiments it ran on itself.
    pub tests: usize,
    /// How many times it changed its own context.
    pub edits: usize,
    /// How many times it asked for something it was not allowed to have.
    pub refusals: usize,
    /// The share of offered questions it instrumented.
    pub rate: Option<f64>,
    /// The 95% Wilson interval around that.
    pub interval: Option<Interval>,
}

impl Reached {
    /// Reads the record for what the subject did with what it was given.
    ///
    /// note: an act is attributed to the question it followed, and a question counts only if it
    /// belonged to a stage and came after a grant. That rule is why a solve - nobody's ladder
    /// rung, and asked before any handle existed - does not dilute the rate.
    ///
    /// note: and a grant is spent when the next session starts, which [`Step::Briefed`] marks. An
    /// experiment that runs the same ladder over several dossiers, or the same dossier several
    /// times, raises a fresh subject each time and hands it fresh handles partway up - so a
    /// carried-over grant would count every rung *below* the handles, in every session after the
    /// first, as a question the subject declined to instrument. On the default ladder that is
    /// thirty-five of a hundred and nineteen, all of them unhandled, and it deflates the rate the
    /// preregistered gate is read off.
    pub fn over(steps: &[Step]) -> Self {
        let mut reached = Self::default();
        let mut granted = false;
        let mut open = false;

        for step in steps {
            match step {
                Step::Briefed { .. } => {
                    granted = false;
                    open = false;
                }
                Step::Granted { .. } => granted = true,
                Step::Asked { stage, .. } => {
                    open = granted && stage.is_some();
                    if open {
                        reached.offered += 1;
                    }
                }
                Step::Acted(act) => {
                    match act {
                        Act::Looked { .. } => reached.looks += 1,
                        Act::Tested { .. } => reached.tests += 1,
                        Act::Excluded { .. } | Act::Revised { .. } => reached.edits += 1,
                        Act::Refused { .. } => reached.refusals += 1,
                    }
                    // counted once per question, however many times it reached for something
                    if open && !matches!(act, Act::Refused { .. }) {
                        reached.instrumented += 1;
                        open = false;
                    }
                }
                _ => {}
            }
        }

        if reached.offered > 0 {
            reached.rate = Some(rounded(
                reached.instrumented as f64 / reached.offered as f64,
            ));
            reached.interval = wilson(reached.instrumented, reached.offered);
        }

        reached
    }

    /// Whether the subject was ever in a position to instrument anything.
    pub fn is_measurable(&self) -> bool {
        self.offered > 0
    }

    /// Whether it reached for the handles often enough for the stages above to mean anything.
    ///
    /// note: the preregistered gate, at half the questions. Below it the instrumented stages are
    /// measuring a model that does not use tools, which is worth knowing and is not what the
    /// ladder claims to measure.
    pub fn clears_the_gate(&self) -> bool {
        self.rate.is_some_and(|rate| rate >= 0.5)
    }
}

impl fmt::Display for Reached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_measurable() {
            return f.write_str("no handles were offered");
        }

        write!(
            f,
            "instrumented {}/{} question(s) ({:.0}%)",
            self.instrumented,
            self.offered,
            self.rate.unwrap_or(0.0) * 100.0
        )?;
        if let Some(interval) = self.interval {
            write!(
                f,
                ", 95% CI {:.0}-{:.0}",
                interval.low * 100.0,
                interval.high * 100.0
            )?;
        }
        write!(
            f,
            "; {} look(s), {} test(s), {} edit(s)",
            self.looks, self.tests, self.edits
        )?;
        if self.refusals > 0 {
            write!(f, ", {} refused", self.refusals)?;
        }

        Ok(())
    }
}
