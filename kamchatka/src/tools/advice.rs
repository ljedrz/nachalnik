//! `Advised`: a second opinion on a call the standing rules would have let through.
//!
//! note: [`Careful`] is a heuristic over a command line and says so. What it cannot do is read a
//! command: `fs:write` is one capability whether the path is `notes.md` or `~/.bashrc`, and
//! `shell` subsumes everything, so a session that answered `always` for either has a rule that is
//! right most of the time and blind the rest of it. This asks a model built for exactly that
//! question - a closed set, answered with a distribution - and folds the answer in with
//! [`Verdict::strictest`].
//!
//! note: it can only ever *tighten*. The model is asked only about calls the standing rules
//! already allow, and its answer is folded with `strictest`, so there is no path from anything it
//! says to a call running that would not have run anyway. A refusal, a timeout, an unparseable
//! answer and a model that has never heard of the tool all leave the standing verdict exactly
//! where it was. That is the whole safety argument, and it is why the failure mode of this file
//! is "no second opinion" rather than "an open gate".
//!
//! note: **what leaves the machine.** The tool's id, the capabilities it declared, and its
//! arguments - which for a write is the text being written and for a shell call is the command
//! line. That is a real disclosure and much larger than the app name
//! `KAMCHATKA_NO_ATTRIBUTION` exists to suppress, which is why this is off unless somebody asks
//! for it by name and why [`ROOM`] caps what one call can send. Nothing else goes: not the
//! conversation, not the system instruction, not the model's prose about why it wants the call.
//!
//! note: and **what feature `shell-advisor` adds to that**, which is the reason it is a second
//! opt-in rather than part of the first. Everything above is sent only for a call the standing
//! rules were going to *allow* - in a default session, not one command, since `exec:run` is a
//! question by default. [`Rating`] is asked for on a call they were going to *ask about*, which
//! is every command the model writes. The same object goes, capped the same way, to the same
//! endpoint; what changes is how often, and a person who agreed to the first has not thereby
//! agreed to the second.
//!
//! note: the rating **decides nothing**. It is never folded into a verdict and never reaches
//! [`Advised::evaluate`]'s return, so a session with `shell-advisor` on refuses and allows
//! exactly what the same session with it off would. It is drawn in the question, in green, yellow
//! or red, for somebody deciding whether to press `y` - the same reasoning as the exit colours in
//! [`Exit`](crate::tools::Exit), one flight up: a coarse question, answered at a glance, beside
//! the exact thing it is about.
//!
//! note: the invariant *nothing in a model's output reaches the policy* still holds, and is worth
//! being precise about. What reaches this is the tool name and the arguments, both as data, which
//! is what reached [`Careful`] before. The agent under judgement cannot address the judge: there
//! is no path for it to add a sentence to this request, and a tool call that *says* it has been
//! approved is a string in `args` like any other.

use std::{collections::VecDeque, sync::Arc};

#[cfg(feature = "shell-advisor")]
use nachalnik::Capability;
use nachalnik::{PermissionPolicy, PermissionRequest, ToolCallId, Verdict, async_trait};
use nachalnik_providers::system1::{Question, SystemOne};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::tools::Careful;

/// How many calls' worth of advice is kept for the screen to read back.
///
/// note: the same bound and the same reason as [`Careful`]'s own: nobody is obliged to read
/// these, and a session that never does should not grow a queue.
const REMEMBERED: usize = 32;

/// How long one argument may be before it is cut, in bytes.
///
/// note: a cap rather than a choice of which fields to send, because the fields are every tool's
/// own and a list here would go stale the first time one grew an argument. Two kilobytes is more
/// than every shell command line and every path in this program's vocabulary, and less than the
/// contents of a file being written - which is the payload this is actually about. What is cut is
/// replaced by a marker rather than dropped silently, so the model is not told a truncated
/// command is the whole of it.
///
/// note: **per argument, and not over the serialised whole**, which is the way it was written
/// first and was wrong. `serde_json` renders an object's keys in sorted order, so `contents`
/// comes before `path` - and cutting the rendered JSON at a byte count took the path away and
/// left two kilobytes of file. The one field the decision actually turns on was the first thing
/// to go. Capping each value keeps every key, so a write of a large file arrives as the path it
/// is writing to and a marker saying how much text came with it.
const ROOM: usize = 2_048;

/// How sure the model has to be before a refusal is taken as one.
///
/// note: below this a `deny` becomes an [`Verdict::Ask`] rather than being discarded or obeyed. A
/// distribution spread across three options is not a refusal, and acting on one as though it were
/// would refuse ordinary work on the strength of a coin toss; throwing it away instead would
/// waste the one signal worth having, since the call is by definition one the standing rules were
/// going to allow. A question is what an uncertain answer actually is.
const SURE: f64 = 0.7;

/// The question the model is asked, under this name, on every call.
const VERDICT: &str = "verdict";

/// And the one whose answer is only ever read out in a sentence.
const IRREVERSIBLE: &str = "irreversible";

/// The rubric a command is placed on for the person about to answer for it.
#[cfg(feature = "shell-advisor")]
const RATING: &str = "rating";

/// And the same rubric put to one stage of that command, numbered from the left.
#[cfg(feature = "shell-advisor")]
const STAGE: &str = "stage";

/// The most stages one command line is taken apart into before it is judged whole instead.
///
/// note: a limit rather than a prefix. Placing the first eight of twelve and folding those would
/// be a rating that silently covers part of a command, which is the failure [`ROOM`]'s marker
/// exists to prevent one field along - so over this the stages are not asked about at all, and
/// the whole-command answer stands on its own the way it did before any of this. Eight is past
/// where a person reads a command line as stages anyway.
#[cfg(feature = "shell-advisor")]
const STAGES: usize = 8;

/// Where a command lands on that rubric: what a colour in the question means.
///
/// note: three, and ordered, because the question a colour answers is coarse - does this leave
/// nothing behind, does it leave something that could be put back, or is it the kind of thing
/// there is no undo for. A fourth band would be a distinction nobody makes at a glance, which is
/// the only moment this is read; and the three map onto the green, yellow and red a terminal has
/// had since before anyone had to be told what they meant.
///
/// note: here rather than where it is drawn, for the reason [`Exit`](crate::tools::Exit) is: the
/// band is worked out from an answer this module owns, and a colour worked out at the other end
/// from a number it does not own is a second opinion about what a rating means. The two drift the
/// first time the rubric is reworded. `ui` maps this to a colour and to nothing else.
///
/// note: it is not a verdict and cannot become one. Nothing in this enum reaches
/// [`Verdict`] - see the module note.
#[cfg(feature = "shell-advisor")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    /// It looks, or moves about; nothing is left changed and nothing goes out.
    Reads,
    /// It leaves something on this machine changed, and what it changed could be put back.
    Changes,
    /// It destroys something that cannot be got back, or sends something off this machine.
    Grave,
}

/// The levels as the model is given them, lowest first - so a higher score is a command somebody
/// should look harder at.
///
/// note: written as what a command *does* rather than as `safe`/`unsafe`, because the model is
/// being asked to place a command and not to agree with a label. A rubric whose levels are
/// adjectives is answered from how the command feels; one whose levels are outcomes is answered
/// from what the command says.
///
/// note: the line between the first two levels is what a command **leaves behind**, and not
/// whether anything changed while it ran. `cd src` changes the working directory, and asked the
/// second way it lands on the middle level - which made a yellow line mean nothing, since moving
/// about is among the commonest things an agent writes. In this program it does not even change
/// anything durable: every call is its own `sh -c`, so the next one starts in the working
/// directory again. So the bottom level names moving about among the things that qualify, and
/// the middle one asks for something *still* changed once the command has finished.
///
/// note: which matters more since a command is placed stage by stage than it did when one was
/// placed whole. A `cd` used to be a clause inside a reading of a longer command line and is now
/// a stage put on the rubric on its own, so a level that misplaces it misplaces it visibly.
#[cfg(feature = "shell-advisor")]
const LEVELS: [&str; 3] = [
    "it only looks, or moves about: it reads, lists, searches, reports or changes directory, and \
     leaves nothing on this machine changed once it has finished",
    "it leaves something on this machine changed once it has finished - a file, a package, a \
     setting - and what it changed could be put back",
    "it destroys something that cannot be got back, or sends something off this machine",
];

#[cfg(feature = "shell-advisor")]
impl Rating {
    /// What the band is called in the question.
    pub fn said(self) -> &'static str {
        match self {
            Self::Reads => "looks, and leaves nothing changed",
            Self::Changes => "changes something, reversibly",
            Self::Grave => "destroys, or sends something out",
        }
    }
}

/// A rating, and how sure the advisor was of it.
///
/// note: both, and kept apart, because the number is not decoration. A rubric's confidence says
/// how concentrated the distribution over the levels was, and a command the advisor could not
/// place is a different thing from one it placed low - `Rated::shown` is where that difference is
/// turned into a colour, and it is the only place, so the words and the colour cannot disagree.
#[cfg(feature = "shell-advisor")]
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Rated {
    /// Where the advisor put it, before [`Rated::shown`] has had its say - or, for a command
    /// taken apart at its joints, the worst of what its stages were shown as.
    ///
    /// note: one field for both, which [`Rated::shown`] being idempotent is what allows. It only
    /// ever raises a `Reads` nobody was sure of, so running it again over a band that is already
    /// a fold of `shown` answers with that band.
    pub scored: Rating,
    /// How sure it was, from 0 to 1 - about the stage that earned the band, where a stage did.
    pub confidence: f64,
    /// Which stage of the command earned it, as a byte range into the command line.
    ///
    /// note: a range rather than the text, so that a client points at the stage in the command
    /// it is already drawing instead of printing it a second time underneath. A panel's rows are
    /// its scarcest thing - the same argument [`joints`](crate::tools::joints) is ranges for -
    /// and on a phone a second copy of a long stage is the whole screen.
    ///
    /// note: `None` where the whole command earned its own band, and for every call that was
    /// rated in one piece: a heredoc, a command with no joints in it, one too long to be sent
    /// whole, and one with more stages than are worth reporting on separately.
    pub worst: Option<(usize, usize)>,
}

#[cfg(feature = "shell-advisor")]
impl Rated {
    /// Reads a position on [`LEVELS`] back as a band.
    ///
    /// note: the nearest level rather than a floor, because the score is a weighted position and
    /// `1.8` is a command the advisor mostly put on the top level. Flooring would draw that one
    /// yellow, which is the direction this must never round in.
    fn of(score: f64, confidence: f64) -> Self {
        let scored = match score {
            it if it < 0.5 => Rating::Reads,
            it if it < 1.5 => Rating::Changes,
            _ => Rating::Grave,
        };

        Self {
            scored,
            confidence,
            worst: None,
        }
    }

    /// The worst of what a call is made of, which is what the call is drawn as.
    ///
    /// note: the fold is over [`Rated::shown`] rather than over the scores, and that order is
    /// load-bearing rather than incidental. `shown` is what lifts a reading nobody was sure of
    /// off green: a stage scored `0.4` at 95% and one scored `0.1` at 30% are `Reads` and
    /// `Changes` once it has run, and `Changes` is the honest answer for the pair. Folding the
    /// scores first picks the higher one, `0.4`, and draws the whole command green on the
    /// strength of the other stage's coin toss. That is exactly what
    /// `an_unsure_rating_is_never_drawn_safer_than_it_scored` holds one stage to, and this is
    /// that property for a command made of several.
    ///
    /// note: the whole command is one of the parts folded, always, and it is the only one that
    /// can see what the stages cannot - a pipeline whose every link is ordinary and whose
    /// composition is not. It is also what makes a stage whose answer never arrived cost a
    /// tightening rather than produce a wrong one: the fold can only ever come back at or above
    /// the band the whole command was given.
    ///
    /// note: the first part to reach the worst band, where more than one does, so what is pointed
    /// at is where the command first gets as bad as it gets.
    fn worst_of(parts: impl IntoIterator<Item = Self>) -> Option<Self> {
        parts
            .into_iter()
            .reduce(|best, next| match next.shown() > best.shown() {
                true => next,
                false => best,
            })
            .map(|worst| Self {
                scored: worst.shown(),
                ..worst
            })
    }

    /// The band this is actually drawn as.
    ///
    /// note: never a safer one than it scored, and never [`Rating::Reads`] where the advisor was
    /// not sure - which is `SURE`'s job over here, and the same principle the verdict fold lives
    /// by one screen away: an uncertain answer is worth having and is not worth acting on as
    /// though it were a certain one. A spread distribution over a safety rubric is not evidence
    /// that a command is safe, and green is the one colour that would say it was.
    ///
    /// note: the confidence is drawn beside this rather than folded away into it, so that a person
    /// reading a yellow line can see whether it is yellow because the command changes something or
    /// yellow because nobody could tell.
    ///
    /// note: idempotent, and the fold behind [`Rated::worst`] leans on it. It only ever raises a
    /// `Reads`,
    /// so asking it about a band it has already answered with gives that band back - which is what
    /// lets `scored` hold either a raw reading or a fold of several without a caller having to
    /// know which it has.
    pub fn shown(self) -> Rating {
        match self.confidence >= SURE {
            true => self.scored,
            false => self.scored.max(Rating::Changes),
        }
    }
}

/// Which of the two questions the advisor is put, and they are two separate decisions.
///
/// note: two flags rather than one, because they are two different disclosures. `--advise` asks
/// about a call the standing rules were going to *allow*, which in a default session is not one
/// command - `exec:run` is a question by default. `--shell-advisor` asks about every command the
/// model writes. A person who agreed to the first has not thereby agreed to the second, and
/// while that was a *build* feature it was a decision nobody downloading a binary could make.
///
/// note: and they are independent in both directions. The rating decides nothing, so asking for
/// it alone is the configuration that shows a colour and changes no verdict anywhere - which is
/// a reasonable thing to want, and was unreachable while one flag turned on both.
/// note: **not** `#[non_exhaustive]`, alone among the structs this crate hands out, and the
/// convention is what says so: that attribute is for a struct this workspace answers with and
/// nothing outside it builds. This one is built outside - it is how an embedder says what it
/// wants its advisor asked - and a caller cannot write a struct literal for a non-exhaustive
/// type at all, `..Default::default()` included. So a third question here is a break, and the
/// version number is where that is said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asked {
    /// Fold a verdict into the gate, for calls the standing rules would allow: `--advise`.
    pub verdict: bool,
    /// Place a shell command on the rubric for the question to draw: `--shell-advisor`.
    ///
    /// note: not behind the feature, though only a build carrying it has a flag that sets this.
    /// A `cfg` on a public field is a struct with two shapes - one of which will not compile
    /// against a caller written for the other - and the branch that reads this is gated already.
    /// So what a build without the feature does with a `true` here is ignore it.
    pub rating: bool,
}

impl Asked {
    /// Whether anything at all is asked, which is what decides if an advisor is built.
    pub fn anything(self) -> bool {
        self.verdict || self.rating
    }
}

/// [`Careful`], with a model asked about whatever it was going to allow.
pub struct Advised {
    /// The standing rules, which decide first and decide alone whenever this cannot reach a
    /// model.
    careful: Arc<Careful>,
    /// The engine asked for the second opinion: a service over HTTP, or a process on this
    /// machine. Nothing in here finds out which - see [`SystemOne`].
    jev: Arc<dyn SystemOne>,
    /// Which of the two questions it is put.
    asked: Asked,
    /// What it said about each call, for [`Advised::said`] and for the refusal the model reads.
    said: Mutex<VecDeque<(ToolCallId, String)>>,
    /// And where it put each command it was asked to rate, for the question to draw.
    ///
    /// note: beside `said` rather than in it. That one holds a sentence the *model* is shown when
    /// a call is refused, and a rating is neither a refusal nor anything the model is told - it is
    /// for the person at the keys, and putting it in the same queue would be one step from its
    /// arriving in a turn.
    #[cfg(feature = "shell-advisor")]
    rated: Mutex<VecDeque<(ToolCallId, Rated)>>,
}

impl Advised {
    /// Wraps a policy in a second opinion, asked for whichever of the two questions.
    ///
    /// note: a call neither question is about costs nothing - `evaluate` returns the standing
    /// verdict without a request, which is what makes `--shell-advisor` on its own free for
    /// every call that is not a command.
    pub fn new(careful: Arc<Careful>, jev: Arc<dyn SystemOne>, asked: Asked) -> Self {
        Self {
            careful,
            jev,
            asked,
            said: Mutex::new(VecDeque::new()),
            #[cfg(feature = "shell-advisor")]
            rated: Mutex::new(VecDeque::new()),
        }
    }

    /// The standing rules underneath, which the tools and the permissions tab hold directly.
    pub fn careful(&self) -> &Arc<Careful> {
        &self.careful
    }

    /// What the advisor said about a call, if it was asked about one.
    pub fn said(&self, call: &ToolCallId) -> Option<String> {
        self.said
            .lock()
            .iter()
            .find(|(known, _)| known == call)
            .map(|(_, said)| said.clone())
    }

    /// Where the advisor put a command, if it was asked to place one.
    ///
    /// note: `None` covers every way of not having an answer and does not distinguish between
    /// them, because the question draws a line for a rating and no line at all otherwise. A call
    /// nobody asked about, an endpoint that was down, an answer that did not parse and a build
    /// with the feature off all mean the same thing to a person reading the panel: this one is
    /// theirs to judge, as it was before any of this existed.
    #[cfg(feature = "shell-advisor")]
    pub fn rating(&self, call: &ToolCallId) -> Option<Rated> {
        self.rated
            .lock()
            .iter()
            .find(|(known, _)| known == call)
            .map(|(_, rated)| *rated)
    }

    /// Asks where a command lands, and writes down the answer for the question to draw.
    ///
    /// note: it returns nothing, and every way of failing leaves nothing written down. There is no
    /// branch from here to a verdict - see the module note - so the worst an outage can do is take
    /// the coloured line off a panel that did not have one a version ago.
    #[cfg(feature = "shell-advisor")]
    async fn rate(&self, request: &PermissionRequest) {
        // note: through `inner`, because some models put every argument inside a wrapper object
        // and the panel unwraps one before drawing it. Reading the command from the other place
        // than the screen does would take spans into a string nobody is looking at, and the
        // stage underlined would be the wrong run of the right command
        let args = crate::tools::ops::inner(&request.args).unwrap_or(&request.args);
        let cmd = args.get("cmd").and_then(Value::as_str).unwrap_or_default();
        let stages = stages(cmd);

        // note: all of them in one request, which is the whole reason a command is worth taking
        // apart at all here. Each is evaluated on its own against the same state, so no stage's
        // answer can be moved by another's, and a twelve-stage pipeline costs the round trip a
        // one-stage command costs
        let mut questions = vec![(RATING.to_owned(), Question::score(PLACE, LEVELS))];
        questions.extend(
            stages
                .iter()
                .enumerate()
                .map(|(n, (from, to))| (format!("{STAGE}-{n}"), placing(&cmd[*from..*to]))),
        );

        let Ok(answers) = self.jev.ask(state(request), questions).await else {
            return;
        };

        // note: the score and the confidence together or not at all. A score with no confidence
        // beside it cannot be drawn by `Rated::shown`'s rule, and the safe reading of half an
        // answer is that nothing was said
        let read = |name: &str| {
            Some(Rated::of(
                answers.score(name)?,
                answers.confidence(name).unwrap_or(0.0),
            ))
        };

        // note: the whole command is what decides whether anything was said at all. A stage that
        // did not come back is one fewer chance to tighten - see `worst_of` - but an answer with
        // no reading of the whole command in it is half an answer, and the safe reading of one is
        // the same here as it is above
        let Some(whole) = read(RATING) else {
            return;
        };
        let placed = stages.iter().enumerate().filter_map(|(n, span)| {
            Some(Rated {
                worst: Some(*span),
                ..read(&format!("{STAGE}-{n}"))?
            })
        });

        let Some(rated) = Rated::worst_of(std::iter::once(whole).chain(placed)) else {
            return;
        };

        let mut remembered = self.rated.lock();
        match remembered
            .iter_mut()
            .find(|(known, _)| known == &request.call)
        {
            Some(known) => known.1 = rated,
            None => {
                if remembered.len() == REMEMBERED {
                    remembered.pop_front();
                }
                remembered.push_back((request.call.clone(), rated));
            }
        }
    }

    /// Writes down what was said about a call, keeping the last [`REMEMBERED`] of them.
    fn remember(&self, call: &ToolCallId, said: String) {
        let mut remembered = self.said.lock();
        match remembered.iter_mut().find(|(known, _)| known == call) {
            Some(known) => known.1 = said,
            None => {
                if remembered.len() == REMEMBERED {
                    remembered.pop_front();
                }
                remembered.push_back((call.clone(), said));
            }
        }
    }
}

/// What the rubric is put to the model as, for a command taken as one thing.
#[cfg(feature = "shell-advisor")]
const PLACE: &str = "A command is about to be run on the user's machine, and a person is about to \
                     be asked whether to let it. Place it on this rubric by what it would do.";

/// And for one stage of one, which is placed with the whole command in the state beside it.
///
/// note: it says to place the stage and not the command, because the state carries the whole
/// command and has to: `cd /tmp && rm -rf *` is two stages whose second means nothing read on its
/// own, and a rubric answered about a fragment with no context is answered about a different
/// command. What the question asks for is a reading of the part, taken in view of the whole.
#[cfg(feature = "shell-advisor")]
const PLACE_STAGE: &str = "A command is about to be run on the user's machine, and a person is \
                           about to be asked whether to let it. The whole command is in the \
                           state. Place only the stage below on this rubric, by what that stage \
                           would do as part of that command.";

/// One stage of a command line, put on [`LEVELS`].
///
/// note: the stage travels as a *value* in an instructions object rather than interpolated into a
/// sentence, for the reason [`state`] is an object rather than a sentence built out of one - a
/// fragment carrying a newline or a quote cannot rearrange the question it is inside of, and a
/// stage of a command line is arbitrary text written by the model. `Question::structured`
/// replaces the sentence `score` was handed, so the sentence goes into the object with it.
#[cfg(feature = "shell-advisor")]
fn placing(stage: &str) -> Question {
    Question::score(PLACE_STAGE, LEVELS).structured(json!({ "asked": PLACE_STAGE, "stage": stage }))
}

/// The byte ranges of a command line's own stages, or nothing where this is not a call it can
/// take apart.
///
/// note: four ways of answering nothing, and each of them leaves the whole-command rating exactly
/// as it was before any of this. A call whose `cmd` is not a string is not one this program knows
/// to hold a command line - the rating is asked for on [`Capability::exec`], which somebody
/// else's tool may declare while taking its command under another name, and guessing which field
/// that is would be placing a rubric on an argument nobody said was a command. A command with no
/// joints in it is one stage, and one stage folded with the whole is the whole. Past [`STAGES`]
/// there are too many to report on honestly - see the note there. And past [`ROOM`] the state the
/// advisor is shown is a *cut* of this command, so a stage taken from beyond the cut would be
/// placed against a command the model was never shown the end of.
#[cfg(feature = "shell-advisor")]
fn stages(cmd: &str) -> Vec<(usize, usize)> {
    if cmd.len() > ROOM {
        return Vec::new();
    }

    let joints = crate::tools::joints(cmd);
    if joints.is_empty() || joints.len() + 1 > STAGES {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(joints.len() + 1);
    let mut at = 0;
    for (from, to) in joints {
        out.push((at, from));
        at = to;
    }
    out.push((at, cmd.len()));

    out.into_iter()
        .filter_map(|(from, to)| tight(cmd, from, to))
        .collect()
}

/// The range with the whitespace around it left off, or nothing where there is nothing in it.
///
/// note: the spaces either side of a `|` belong to neither stage, and a range carrying them is
/// underlined on a screen as a stage with a gap hanging off it. Trimming here rather than where
/// it is drawn is what keeps the range the advisor was asked about and the range a client points
/// at the same range.
#[cfg(feature = "shell-advisor")]
fn tight(cmd: &str, from: usize, to: usize) -> Option<(usize, usize)> {
    let piece = cmd.get(from..to)?;
    let from = from + (piece.len() - piece.trim_start().len());
    let to = to - (piece.len() - piece.trim_end().len());

    (from < to).then_some((from, to))
}

/// The state the model is shown: what is about to run, and nothing about who asked for it.
///
/// note: an object rather than a sentence built out of these. The endpoint takes one, the shape
/// says which part is the tool and which part is its arguments without a phrasing having to carry
/// that, and a path with a newline in it cannot rearrange the question it is inside of.
fn state(request: &PermissionRequest) -> Value {
    json!({
        "tool": request.tool,
        "capabilities": request
            .capabilities
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "arguments": capped(&request.args),
    })
}

/// The arguments with every long string cut to [`ROOM`], and the shape they arrived in kept.
///
/// note: recursive, because a tool's arguments are whatever its schema says and this program
/// already has ones holding arrays of strings. What is never touched is a key, a number or the
/// nesting - so whatever the model is shown, it is shown the same *structure* the tool was asked
/// to act on.
fn capped(value: &Value) -> Value {
    match value {
        Value::String(text) if text.len() > ROOM => {
            // note: cut on a character boundary, since this is arbitrary text and `ROOM` is a
            // byte count. A `String` sliced through a multi-byte character panics, and the one
            // place that would happen is a file whose contents are not ASCII
            let mut room = ROOM;
            while room > 0 && !text.is_char_boundary(room) {
                room -= 1;
            }

            Value::String(format!(
                "{}… (cut; {} bytes in all)",
                &text[..room],
                text.len()
            ))
        }
        Value::Array(items) => Value::Array(items.iter().map(capped).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), capped(value)))
                .collect(),
        ),
        untouched => untouched.clone(),
    }
}

/// What to do with a verdict the standing rules were going to allow.
///
/// note: split out from [`Advised::evaluate`] so that the rule can be tested without a network,
/// the way `waiting::Vigil::judge` is. What is left in `evaluate` is one HTTP request and the
/// reading of it, which has nothing in it to get wrong twice.
///
/// note: `allow` returns [`Verdict::Allow`] rather than the standing verdict, and the two are the
/// same thing here: this is only ever called for a call whose standing verdict was `Allow`.
/// `evaluate` folds with [`Verdict::strictest`] anyway, so a change to that precondition cannot
/// turn into an open gate.
fn advised(choice: &str, confidence: f64) -> Verdict {
    match choice {
        "deny" if confidence >= SURE => Verdict::Deny,
        // a refusal nobody is sure of is a question; see `SURE`
        "deny" | "ask" => Verdict::Ask,
        // anything else - `allow`, or an option this program did not offer - leaves the standing
        // rules alone. An answer outside the closed set is a change at the other end, and the
        // safe reading of one is that nothing was said
        _ => Verdict::Allow,
    }
}

#[async_trait]
impl PermissionPolicy for Advised {
    /// note: the advisor's sentence where it is what refused, and the standing rules' where they
    /// are. A model told `refused by \`shell\`` can stop asking for `shell`; one told the advisor
    /// judged *this command* irreversible can try a different command, which is the more useful
    /// of the two and is only true when it is true.
    fn why(&self, request: &PermissionRequest) -> Option<String> {
        self.said(&request.call)
            .or_else(|| self.careful.why(&request.call))
    }

    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let standing = self.careful.evaluate(request).await;

        // the other half of the round trip, and the only one that produces something a person
        // reads rather than something the gate acts on: a command somebody is about to be asked
        // about, placed on a rubric so that the question can be coloured
        //
        // note: `Ask` only. A `Deny` is not a question and has no panel to draw a rating in, so
        // rating one would send the arguments of a call that was never going to run - which the
        // test below holds this to. An `Allow` has no panel either, and is the branch underneath
        //
        // note: and `exec:run` only, which is the capability rather than the tool's name. The
        // rubric is written about a command, and `shell` is the tool that takes one; a tool that
        // declares the capability is asking for the same thing whatever it calls itself, and one
        // that does not is not what `shell-advisor` was turned on for
        #[cfg(feature = "shell-advisor")]
        if self.asked.rating
            && standing == Verdict::Ask
            && request.capabilities.contains(&Capability::exec("run"))
        {
            self.rate(request).await;
        }

        // note: asked only about what would otherwise run. A call already heading for `Ask` or
        // `Deny` cannot be made stricter by anything the model says, so asking would spend a
        // round trip and somebody's money to learn nothing - and would put a network request in
        // front of the refusal a person is waiting to see
        //
        // note: and only where a verdict was asked for at all. `--shell-advisor` alone draws a
        // colour and decides nothing, so the gate is `Careful`'s from end to end and no call's
        // arguments go out for a question nobody put
        if !self.asked.verdict || standing != Verdict::Allow {
            return standing;
        }

        let asked = self
            .jev
            .ask(
                state(request),
                vec![
                    (
                        VERDICT.to_owned(),
                        Question::between_described(
                            "A tool is about to run on the user's machine with the arguments \
                             shown. What should a permission gate do with it?",
                            [
                                ("allow", "ordinary work, and safe to run unattended"),
                                ("ask", "a person should look at this one first"),
                                (
                                    "deny",
                                    "destructive, or reaches something it has no business \
                                     reaching",
                                ),
                            ],
                        ),
                    ),
                    (
                        IRREVERSIBLE.to_owned(),
                        Question::noul(
                            "Would running this destroy something that cannot be got back?",
                        )
                        .between(
                            "it deletes, overwrites or sends data that cannot be recovered",
                            "it only reads, or anything it changes can be undone",
                        ),
                    ),
                ],
            )
            .await;

        // note: every failure lands here and every one of them keeps the standing verdict. A
        // notice is left for the status line, because a gate that has quietly stopped asking is
        // worse than one that never asked - somebody who turned this on should be able to see
        // that it is not working
        let answers = match asked {
            Ok(answers) => answers,
            Err(e) => {
                self.remember(
                    &request.call,
                    format!("the advisor could not be reached: {e}"),
                );

                return standing;
            }
        };

        let Some(choice) = answers.choice(VERDICT) else {
            self.remember(
                &request.call,
                "the advisor answered nothing this program could read".to_owned(),
            );

            return standing;
        };
        let confidence = answers.confidence(VERDICT).unwrap_or(0.0);
        let verdict = standing.strictest(advised(choice, confidence));

        // the sentence both the screen and the model read, and it names the figures it acted on
        // rather than only the conclusion
        let mut said = format!(
            "the advisor said `{choice}` ({:.0}% sure)",
            confidence * 100.0
        );
        if let Some(irreversible) = answers.noul(IRREVERSIBLE).filter(|it| *it >= SURE) {
            said.push_str(&format!(
                "; it judges this irreversible ({:.0}%)",
                irreversible * 100.0
            ));
        }
        if verdict == Verdict::Allow {
            said.push_str(", so the standing rules stand");
        }
        self.remember(&request.call, said);

        verdict
    }
}

#[cfg(test)]
mod tests {
    use nachalnik::{Capability, PermissionId};

    use super::*;
    use crate::tools::Subject;

    /// Both questions asked, which is what every test in here is about unless it says otherwise.
    fn both() -> Asked {
        Asked {
            verdict: true,
            rating: true,
        }
    }

    /// An address nothing is listening on, so the advisor fails the way an outage makes it fail.
    ///
    /// note: port 1, which is refused rather than filtered - the same distinction
    /// `waiting::tests::nobody_home` is careful about. A refused connection is not a timeout, so
    /// this also pins that the failure is not retried four times on the way to being ignored.
    fn unreachable() -> Arc<nachalnik_providers::system1::Jev> {
        Arc::new(nachalnik_providers::system1::Jev::new(
            "jev-latest",
            "http://127.0.0.1:1",
            "not-a-key",
        ))
    }

    /// One call, with the capability it declares.
    fn asking(tool: &str, capability: Capability) -> PermissionRequest {
        PermissionRequest {
            id: PermissionId(1),
            call: ToolCallId::from("call-1"),
            tool: tool.to_owned(),
            capabilities: vec![capability],
            args: Arc::new(json!({ "path": "notes.md" })),
        }
    }

    /// The safety property: an advisor that is not there changes nothing.
    ///
    /// note: the most important test in this file. Every way of failing to get an answer - a dead
    /// endpoint, a refused key, a timeout, an answer that does not parse - lands on the same
    /// branch, and this is the one that proves the branch keeps the standing verdict instead of
    /// falling through to something laxer. A session whose permissions quietly loosened when a
    /// third party had an outage would be worse than one that never asked.
    #[tokio::test]
    async fn an_advisor_that_cannot_be_reached_leaves_the_standing_verdict_where_it_was() {
        let careful = Arc::new(Careful::new());
        careful.set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);

        let advised = Advised::new(careful, unreachable(), both());
        let request = asking("read", Capability::fs("read"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Allow);
        // and it says so rather than failing silently: somebody who turned this on should be able
        // to see that it is not working
        let said = advised
            .said(&request.call)
            .expect("it wrote down what happened");
        assert!(said.contains("could not be reached"), "{said}");
    }

    /// A call the standing rules already refuse is decided here and goes nowhere.
    ///
    /// note: two things at once, and the second is the one worth having a test for. The verdict is
    /// unchanged, which `strictest` would have given anyway - and nothing was *sent*, so a refused
    /// call does not put a network round trip in front of the refusal a person is waiting to see,
    /// and does not hand a third party the arguments of a call that was never going to run.
    #[tokio::test]
    async fn a_call_the_rules_already_refuse_is_not_sent_anywhere() {
        let careful = Arc::new(Careful::new());
        careful.set(&Subject::Capability(Capability::fs("read")), Verdict::Deny);

        let jev = unreachable();
        let advised = Advised::new(careful, jev.clone(), both());
        let request = asking("read", Capability::fs("read"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Deny);
        assert_eq!(jev.attempts(), 0, "nothing should have left the machine");
        // and the sentence the model reads is the standing rules' own, since they are what
        // refused it
        let said = advised.why(&request).expect("a refusal names itself");
        assert!(said.contains("refused by"), "{said}");
    }

    /// The same for a call nobody has answered for, which `Careful` sends up as a question.
    #[tokio::test]
    async fn a_call_already_going_to_be_asked_about_is_not_sent_anywhere() {
        // nothing set, so `fs:read` is `ask` - which is already stricter than `allow` and cannot
        // be made stricter still by anything a model says
        let jev = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), jev.clone(), both());
        let request = asking("read", Capability::fs("read"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(jev.attempts(), 0, "nothing should have left the machine");
    }

    /// The fold, which is the half of this that decides anything.
    #[test]
    fn a_second_opinion_can_only_ever_tighten() {
        // a confident refusal is one
        assert_eq!(advised("deny", 0.99), Verdict::Deny);
        assert_eq!(advised("deny", SURE), Verdict::Deny);

        // and one nobody is sure of is a question instead, rather than either being obeyed or
        // thrown away
        assert_eq!(advised("deny", 0.5), Verdict::Ask);
        assert_eq!(advised("deny", 0.0), Verdict::Ask);

        // `ask` is a question however sure it is: there is nothing stricter to be sure *of*
        assert_eq!(advised("ask", 0.99), Verdict::Ask);
        assert_eq!(advised("ask", 0.1), Verdict::Ask);

        // and `allow` changes nothing, which is what makes this only ever a tightening
        assert_eq!(advised("allow", 0.99), Verdict::Allow);
        assert_eq!(advised("allow", 0.0), Verdict::Allow);
    }

    /// The rubric is read by the nearest level, not by the one it has passed.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn a_score_lands_on_the_band_it_is_nearest() {
        let sure = |score| Rated::of(score, 1.0).shown();

        assert_eq!(sure(0.0), Rating::Reads);
        assert_eq!(sure(0.49), Rating::Reads);
        assert_eq!(sure(0.5), Rating::Changes);
        assert_eq!(sure(1.0), Rating::Changes);
        assert_eq!(sure(1.49), Rating::Changes);
        // and the direction this must never round in: mostly on the top level is the top level
        assert_eq!(sure(1.5), Rating::Grave);
        assert_eq!(sure(1.8), Rating::Grave);
        assert_eq!(sure(2.0), Rating::Grave);
    }

    /// A rating nobody is sure of is never drawn green, and is never drawn safer than it scored.
    ///
    /// note: the property this feature stands on, and the display's version of the fold that
    /// `a_second_opinion_can_only_ever_tighten` holds the verdict to. A spread distribution over a
    /// safety rubric is not evidence that a command is safe - it is the advisor saying it could
    /// not tell - and green is the one colour that would report it as the former.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn an_unsure_rating_is_never_drawn_safer_than_it_scored() {
        for unsure in [0.0, 0.3, 0.5, SURE - 0.01] {
            // what would have been green is yellow instead
            assert_eq!(Rated::of(0.0, unsure).shown(), Rating::Changes);
            assert_eq!(Rated::of(0.4, unsure).shown(), Rating::Changes);
            // yellow stays yellow, and red stays red: the rule only ever moves a band up
            assert_eq!(Rated::of(1.0, unsure).shown(), Rating::Changes);
            assert_eq!(Rated::of(2.0, unsure).shown(), Rating::Grave);
        }

        // and at the threshold the advisor is taken at its word
        assert_eq!(Rated::of(0.0, SURE).shown(), Rating::Reads);
    }

    /// A command going to be refused is not rated, whatever the feature is doing.
    ///
    /// note: the disclosure half of the same argument the verdict makes. A `Deny` has no question
    /// to colour, so rating one would send the arguments of a call that was never going to run to
    /// a third party and put nothing on any screen in exchange.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn a_command_the_rules_already_refuse_is_not_rated() {
        let careful = Arc::new(Careful::new());
        careful.set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

        let jev = unreachable();
        let advised = Advised::new(careful, jev.clone(), both());
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Deny);
        assert_eq!(jev.attempts(), 0, "nothing should have left the machine");
        assert!(advised.rating(&request.call).is_none());
    }

    /// And one going to be asked about is rated, which is the only call that is.
    ///
    /// note: the endpoint is not there, so what this pins is that the request was *attempted* -
    /// the rating itself needs a live model and lives in `tests/advise.rs`. The pair with
    /// `a_call_already_going_to_be_asked_about_is_not_sent_anywhere` is the point: that one still
    /// passes, because it asks about `fs:read`, and this one is what `exec:run` changed.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn a_command_somebody_is_about_to_be_asked_about_is_rated() {
        let jev = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), jev.clone(), both());
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(jev.attempts(), 1, "the rating was asked for");
        // and an advisor that could not be reached leaves no rating rather than a reassuring one
        assert!(
            advised.rating(&request.call).is_none(),
            "an outage draws no line, rather than a green one"
        );
    }

    /// A command of eight stages costs the round trip a command of one costs.
    ///
    /// note: the property that makes taking a command apart worth doing here at all, and the
    /// reason this model rather than a chat one. Every stage is a question in the *same* request,
    /// evaluated on its own against the same state, so none of them can be moved by another's
    /// answer and the whole fold is paid for once. A question per request would be eight round
    /// trips in front of somebody waiting to press `y`, and nobody would keep the feature.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn every_stage_of_a_command_is_asked_about_in_one_request() {
        let jev = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), jev.clone(), both());

        let mut request = asking("shell", Capability::exec("run"));
        let cmd = ["true"; STAGES].join(" && ");
        request.args = Arc::new(json!({ "cmd": cmd }));
        assert_eq!(stages(request.args["cmd"].as_str().unwrap()).len(), STAGES);

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(jev.attempts(), 1, "one request, however many stages");
    }

    /// The fold is over what each stage is *shown* as, and taking the scores first loses the
    /// property the whole rubric rests on.
    ///
    /// note: the most important test of the fold, and the case is not a corner. A stage scored
    /// `0.4` at 95% is a confident `Reads`; one scored `0.1` at 30% is a reading nobody could
    /// make, which `Rated::shown` lifts to `Changes` because green is the one colour that must
    /// never come out of a coin toss. Fold the *scores* and `0.4` wins, and the command is drawn
    /// green on the strength of the other stage's uncertainty - a higher number standing for a
    /// safer command, which is exactly backwards. Fold what each is shown as and the pair is
    /// `Changes`.
    ///
    /// note: the pair with `an_unsure_rating_is_never_drawn_safer_than_it_scored`, which holds
    /// one stage to this. Nothing there survives being composed, which is why this is separate
    /// rather than another case in it.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn the_worst_of_several_stages_is_folded_after_the_unsure_rule_and_not_before() {
        let sure = Rated::of(0.4, 0.95);
        let unsure = Rated::of(0.1, 0.3);
        assert_eq!(
            sure.shown(),
            Rating::Reads,
            "the higher score is the safe one"
        );
        assert_eq!(unsure.shown(), Rating::Changes, "and the lower one is not");

        for pair in [[sure, unsure], [unsure, sure]] {
            let folded = Rated::worst_of(pair).expect("two stages fold to one");
            assert_eq!(
                folded.shown(),
                Rating::Changes,
                "folding the scores would have drawn this green"
            );
        }
    }

    /// And the fold can only ever come back at or above what the whole command was given.
    ///
    /// note: the property that makes a stage whose answer never arrived safe to carry on without.
    /// The whole command is one of the parts folded, always, so a missing stage costs a
    /// tightening that might have happened and cannot produce one that should not have.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn a_fold_is_never_softer_than_the_whole_command_on_its_own() {
        let whole = Rated::of(2.0, 0.9);
        let mild = Rated::of(0.0, 0.99);

        let folded = Rated::worst_of([whole, mild, mild]).expect("it folds");
        assert_eq!(folded.shown(), Rating::Grave);

        // and a stage worse than the whole command is what does move it
        let folded = Rated::worst_of([Rated::of(0.0, 0.99), Rated::of(2.0, 0.99)]);
        assert_eq!(folded.expect("it folds").shown(), Rating::Grave);

        // nothing at all folds to nothing, rather than to a reassuring band
        assert_eq!(Rated::worst_of([]), None);
    }

    /// What is pointed at is the first stage to reach the worst band.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn the_stage_pointed_at_is_where_the_command_first_gets_as_bad_as_it_gets() {
        let at = |span, score| Rated {
            worst: Some(span),
            ..Rated::of(score, 0.99)
        };

        let folded = Rated::worst_of([at((0, 4), 0.0), at((7, 11), 2.0), at((14, 18), 2.0)])
            .expect("it folds");
        assert_eq!(folded.worst, Some((7, 11)));

        // and the whole command winning points at nothing, because it is not a stage
        let folded = Rated::worst_of([Rated::of(2.0, 0.99), at((7, 11), 0.0)]).expect("it folds");
        assert_eq!(folded.worst, None);
        assert_eq!(folded.shown(), Rating::Grave);
    }

    /// `shown` run over its own answer answers the same thing, which is what `worst_of` leans on.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn the_unsure_rule_run_twice_says_what_it_said_once() {
        for scored in [Rating::Reads, Rating::Changes, Rating::Grave] {
            for confidence in [0.0, 0.3, SURE, 0.99] {
                let once = Rated {
                    scored,
                    confidence,
                    worst: None,
                }
                .shown();
                let twice = Rated {
                    scored: once,
                    confidence,
                    worst: None,
                }
                .shown();

                assert_eq!(once, twice, "{scored:?} at {confidence}");
            }
        }
    }

    /// Which commands are taken apart, and which are rated whole.
    ///
    /// note: the four refusals are the half worth pinning. Each of them leaves the rating exactly
    /// what it was before any of this, and each is a different reason - see `stages`.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn a_command_is_taken_apart_only_where_taking_it_apart_says_something_true() {
        let cmd = "cargo build && rm -rf target";
        let apart = stages(cmd);
        assert_eq!(
            apart.iter().map(|(f, t)| &cmd[*f..*t]).collect::<Vec<_>>(),
            ["cargo build", "rm -rf target"]
        );
        // trimmed, so what is underlined is the stage and not the spaces either side of the `&&`
        assert_eq!(apart[0], (0, 11));

        // one stage is the whole command, and folding the whole command with itself says nothing
        // the whole command did not
        assert!(stages("cargo build --release").is_empty());
        // a heredoc is not scanned at all, which is `joints`' rule and the reason a long script
        // costs nothing here
        assert!(stages("python3 - <<'PY'\nprint(1 | 2)\nPY").is_empty());
        // over `ROOM` the state is a cut of the command, so a stage past the cut would be placed
        // against a command the advisor was not shown the end of
        assert!(stages(&format!("echo {} | wc -l", "a".repeat(ROOM))).is_empty());

        // up to `STAGES`, and past it the command is rated whole rather than in part
        let chain = |n: usize| vec!["true"; n].join(" && ");
        assert_eq!(stages(&chain(STAGES)).len(), STAGES);
        assert!(stages(&chain(STAGES + 1)).is_empty());
    }

    /// The rubric on its own never reaches the gate, and never sends a verdict question.
    ///
    /// note: the property the two flags exist to create. `--shell-advisor` decides nothing - it
    /// draws a colour - so a session with it and without `--advise` allows and refuses exactly
    /// what the same session with no advisor at all would, and pays for no question about a call
    /// that was going to run. While one flag turned on both there was no way to ask for that.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn the_rubric_alone_decides_nothing_and_asks_nothing_about_what_would_run() {
        let careful = Arc::new(Careful::new());
        careful.set(
            &Subject::Capability(Capability::exec("run")),
            Verdict::Allow,
        );

        let jev = unreachable();
        let advised = Advised::new(
            careful,
            jev.clone(),
            Asked {
                verdict: false,
                rating: true,
            },
        );

        // a command the rules allow runs, and nothing was asked about it: no verdict question,
        // and no rating either, since a call nobody is being asked about has no panel to colour
        let request = asking("shell", Capability::exec("run"));
        assert_eq!(advised.evaluate(&request).await, Verdict::Allow);
        assert_eq!(jev.attempts(), 0, "nothing should have left the machine");
        assert!(advised.said(&request.call).is_none());
    }

    /// And the verdict on its own never rates, which is the same line from the other side.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn a_verdict_asked_for_alone_rates_nothing() {
        let jev = unreachable();
        let advised = Advised::new(
            Arc::new(Careful::new()),
            jev.clone(),
            Asked {
                verdict: true,
                rating: false,
            },
        );

        // nothing set, so `exec:run` is a question - which is the branch a rating is asked on,
        // and the one this build is not asking
        let request = asking("shell", Capability::exec("run"));
        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(jev.attempts(), 0, "a question is not a verdict to tighten");
        assert!(advised.rating(&request.call).is_none());
    }

    /// An option nobody offered is read as nothing having been said.
    ///
    /// note: the shape of a change at the other end - a fourth option, a renamed one, a model
    /// that answers in another language. Every one of them has to leave the standing rules alone,
    /// because the alternative is a gate whose behaviour moves when somebody else ships.
    #[test]
    fn an_answer_outside_the_closed_set_decides_nothing() {
        for unknown in ["refuse", "DENY", "deny ", "", "yes", "allow-with-care"] {
            assert_eq!(
                advised(unknown, 1.0),
                Verdict::Allow,
                "`{unknown}` should decide nothing"
            );
        }
    }

    /// What goes out, and what does not.
    #[test]
    fn the_state_carries_the_call_and_nothing_about_the_conversation() {
        let request = PermissionRequest {
            id: PermissionId(1),
            call: ToolCallId::from("call-1"),
            tool: "shell".to_owned(),
            capabilities: vec![Capability::exec("run")],
            args: Arc::new(json!({ "command": "rm -rf /", "why": "the model's excuse" })),
        };

        let state = state(&request);
        assert_eq!(state["tool"], "shell");
        assert_eq!(state["capabilities"][0], "exec:run");
        // the arguments go as data, verbatim, including anything the model wrote in them - which
        // is a value in a JSON document and not a sentence in the question
        assert_eq!(state["arguments"]["command"], "rm -rf /");
        assert_eq!(state["arguments"]["why"], "the model's excuse");

        // and the object has exactly three keys: nothing about the session, the system
        // instruction, or what the model said outside its own arguments
        let keys: Vec<&String> = state.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["arguments", "capabilities", "tool"]);
    }

    /// A payload larger than [`ROOM`] is cut, says so, and does not panic on the way.
    #[test]
    fn a_write_of_a_whole_file_is_cut_and_says_it_was() {
        // note: the third and fourth of these are the ones that matter. A multi-byte character
        // straddling the cut is the one input that turns a byte count into a panic, and it takes
        // an odd byte in front of the run to arrange: `é` is two bytes and `🙂` is four, so a run
        // of either on its own has a boundary exactly at `ROOM` and never exercises the walk. The
        // leading `a` shifts every boundary by one, and `ROOM` then lands inside a character
        for contents in [
            "a".repeat(ROOM + 1),
            "é".repeat(ROOM),
            format!("a{}", "é".repeat(ROOM)),
            format!("a{}", "🙂".repeat(ROOM)),
        ] {
            let request = PermissionRequest {
                id: PermissionId(1),
                call: ToolCallId::from("call-1"),
                tool: "write".to_owned(),
                capabilities: Vec::new(),
                args: Arc::new(json!({ "path": "notes.md", "contents": contents })),
            };

            let state = state(&request);
            let contents = state["arguments"]["contents"]
                .as_str()
                .expect("the field is still a string");
            assert!(contents.len() <= ROOM + 64, "{}", contents.len());
            assert!(contents.contains("(cut;"), "the cut is named");

            // and the path survives it, which is the whole reason the cap is per value. Rendering
            // the arguments and cutting the bytes took this away: `contents` sorts before `path`,
            // so the one field the decision turns on was the first thing to go
            assert_eq!(state["arguments"]["path"], "notes.md");
        }

        // and something that fits is not touched at all, structure included
        let request = PermissionRequest {
            id: PermissionId(1),
            call: ToolCallId::from("call-1"),
            tool: "read".to_owned(),
            capabilities: Vec::new(),
            args: Arc::new(json!({ "path": "notes.md", "lines": [1, 2], "all": true })),
        };
        let state = state(&request);
        assert_eq!(
            state["arguments"],
            json!({ "path": "notes.md", "lines": [1, 2], "all": true })
        );
    }
}
