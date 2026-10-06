//! `Advised`: [`Careful`], with each shell command it is going to ask about placed on a rubric
//! for the person about to answer.
//!
//! note: it **decides nothing**. The verdict is the standing rules' and only theirs: a call they
//! allow runs, a call they refuse is refused, and a call they ask about is asked about. An `allow`
//! is somebody's decision, and a second model does not get to reopen it. What this adds is a line
//! in the question, in green, yellow or red, for somebody deciding whether to press `y` - the same
//! reasoning as the exit colours in [`Exit`](crate::tools::Exit), one flight up: a coarse
//! question, answered at a glance, beside the exact thing it is about.
//!
//! note: and where it cannot rate a command it says so, in the rating's place - see
//! [`Advised::why_unrated`] for why a missing line is not a neutral one.
//!
//! note: **what leaves the machine.** For each shell command the rules are going to ask about,
//! which in a default session is every command the model writes: the tool's id, the capabilities
//! it declared, and its arguments - the command line. That is a real disclosure and much larger
//! than the app name `KAMCHATKA_NO_ATTRIBUTION` exists to suppress, which is why this is off
//! unless somebody asks for it by name and why [`ROOM`] caps what one call can send. Nothing else
//! goes: not the conversation, not the system instruction, not the model's prose about why it
//! wants the call.
//!
//! note: the invariant *nothing in a model's output reaches the policy* holds twice over. Nothing
//! here reaches a verdict at all, and what reaches the advisor is the tool name and the arguments,
//! both as data: a tool call that *says* it has been approved is a string in `args` like any
//! other.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use nachalnik::{
    Capability, PermissionPolicy, PermissionRequest, ToolCallId, Verdict, async_trait,
};
use nachalnik_providers::system1::{Question, SystemOne};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::tools::Careful;

/// How many calls' worth of advice is kept for the screen to read back.
///
/// note: bounded for the same reason as [`Careful`]'s own: nobody is obliged to read these, and a
/// session that never does should not grow a queue.
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
/// note: **per argument, and not over the serialised whole**. `serde_json` renders an object's
/// keys in sorted order, so `contents` comes before `path` - and cutting the rendered JSON at a
/// byte count takes the path away and leaves two kilobytes of file, so the one field the decision
/// turns on is the first thing to go. Capping each value keeps every key, so a write of a large
/// file arrives as the path it is writing to and a marker saying how much text came with it.
const ROOM: usize = 2_048;

/// Why there is no rating, where the advisor answered and nothing in the answer could be read.
const UNREAD: &str = "it answered nothing this program could read";

/// How long a reason for having no rating may be, in characters.
///
/// note: it goes in the question's pinned header, beside the command somebody is deciding about,
/// and a firewall's refusal page runs on for a paragraph after the sentence that says what it is.
const SAID: usize = 120;

/// A reason cut to [`SAID`], saying where it was cut.
fn cut(reason: &str) -> String {
    match reason.chars().count() > SAID {
        true => format!(
            "{}…",
            reason.chars().take(SAID).collect::<String>().trim_end()
        ),
        false => reason.to_owned(),
    }
}

/// How sure the model has to be before a rating is taken at its word.
///
/// note: below this a reading is drawn as the middle band whatever it scored - see
/// [`Rated::shown`]. A distribution spread across the rubric is the advisor saying it could not
/// tell, and drawing it green would report that as the opposite.
const SURE: f64 = 0.7;

/// The rubric a command is placed on for the person about to answer for it.
const RATING: &str = "rating";

/// And the same rubric put to one stage of that command, numbered from the left.
const STAGE: &str = "stage";

/// The top of that rubric, asked again as a claim rather than as a position on it.
///
/// note: both, and folded, because engines are good at different halves of it. An ordinal `score`
/// is the primitive some read worst - laya's own card calls it its weakest - and asked the same
/// reading as a `noul` they find destructive commands the rubric misses; others read the rubric
/// well and miss some of those commands when it is taken away. Folded with [`Rated::worst_of`],
/// the reading that is right about a command carries it, whichever engine is asked.
///
/// note: it costs a question and not a round trip. Every question in a call is answered in one
/// pass - which is also what makes placing a command stage by stage affordable - so doubling them
/// costs milliseconds rather than a wait.
const DANGER: &str = "danger";

/// The most stages one command line is taken apart into before it is judged whole instead.
///
/// note: a limit rather than a prefix. Placing the first eight of twelve and folding those would
/// be a rating that silently covers part of a command, which is the failure [`ROOM`]'s marker
/// exists to prevent one field along - so over this the stages are not asked about at all, and
/// the whole-command answer stands on its own. Eight is past where a person reads a command line
/// as stages anyway.
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
/// note: it is not a verdict and cannot become one - see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    /// It looks, or moves about; nothing is left changed and nothing goes out.
    Reads,
    /// It changes files inside the working directory, the way git or a rebuild could undo.
    Changes,
    /// It reaches outside the working directory, destroys something that cannot be got back, or
    /// sends something off this machine.
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
/// second way it lands on the middle level - which makes a yellow line mean nothing, since moving
/// about is among the commonest things an agent writes. In this program it does not even change
/// anything durable: every call is its own `sh -c`, so the next one starts in the working
/// directory again. So the bottom level names moving about among the things that qualify, and
/// the middle one asks for something *still* changed once the command has finished.
///
/// note: and the bottom level says what moving about is - reading, listing, searching, changing
/// directory. Said only as "moves about", a `cd` stage was placed there at barely the confidence
/// a green needs, so a `cd` drawn yellow was one answer away.
///
/// note: the line between the top two levels is **where** a change lands, and not whether it
/// could be put back. Nearly everything could be, given the effort - `chmod -R 777 /` can be
/// undone, and so can a global install - so a middle level of "reversible" puts both beside
/// `cargo build`. The working directory is where an agent is sent to work and is the line the
/// sandbox draws for writes; git or a rebuild undoes what happens inside it, and anything outside
/// it - system files, permissions, the home directory, the machine, another account, a remote
/// service - is what a person should stop on. Destroying what cannot be got back and sending
/// something off the machine stay at the top wherever they happen.
///
/// note: and a command is placed stage by stage, so a `cd` is put on the rubric on its own rather
/// than read as a clause of a longer command line. A level that misplaces it does so visibly.
///
/// note: **short, because what a model pays for every request is a toll**, and the levels are the
/// part of it a small model has to hold in mind while reading a command. Nothing is claimed for
/// the length beyond that: what a rubric scores moves with the *state* as much as with the words,
/// and a bare command is an easier question than the call the program sends. Anything added here
/// should be measured against what the program sends: [`state`] and these words, not a bare
/// command line.
pub(crate) const LEVELS: [&str; 3] = [
    "it only looks, or moves about - reads, lists, searches or changes directory - and leaves nothing changed",
    "it changes files inside the working directory - writing, building, formatting, committing - in a way git or a rebuild could undo",
    "it changes anything outside the working directory - system files, permissions, the home directory, the machine itself, other accounts, remote services - or destroys something that cannot be got back, or sends something off this machine",
];

impl Rating {
    /// What the band is called in the question.
    pub fn said(self) -> &'static str {
        match self {
            Self::Reads => "looks, and leaves nothing changed",
            Self::Changes => "changes files in the working directory",
            Self::Grave => "reaches outside, destroys, or sends out",
        }
    }
}

/// A rating, and how sure the advisor was of it.
///
/// note: both, and kept apart, because the number is not decoration. A rubric's confidence says
/// how concentrated the distribution over the levels was, and a command the advisor could not
/// place is a different thing from one it placed low - `Rated::shown` is where that difference is
/// turned into a colour, and it is the only place, so the words and the colour cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Rated {
    /// Where the advisor put it, before [`Rated::shown`] has had its say - or, for a command
    /// taken apart at its joints, the worst of what its stages were shown as.
    ///
    /// note: one field for both, which works because [`Rated::shown`] is idempotent. It only ever
    /// raises a `Reads` nobody was sure of, so running it again over a band that is already a fold
    /// of `shown` answers with that band.
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
    /// note: `None` where the whole command earned a band no stage reaches, and for every call
    /// that was rated in one piece: a heredoc, a command with no joints in it, one too long to be
    /// sent whole, and one with more stages than are worth reporting on separately.
    pub worst: Option<(usize, usize)>,
}

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

    /// And reads the same reading back off the claim, where it was asked as one.
    ///
    /// note: a claim answers the top band and nothing else, so the bottom one is what is left when
    /// it is confidently false rather than something the question said. What that costs is the
    /// difference between `ls` and `mkdir`, which no colour here was ever drawing - the middle band
    /// is what a reading nobody is sure of lands on either way.
    ///
    /// note: a `noul`'s number *is* its confidence, which is why this takes one argument where
    /// [`Rated::of`] takes two. How sure the model is of a claim it puts at 0.9 is 0.9, and of one
    /// it puts at 0.1 is also 0.9 - of the claim being false. Between the two it is sure of
    /// nothing, and the band says so rather than picking the nearer side of a coin toss.
    fn claimed(danger: f64) -> Self {
        let scored = match danger {
            it if it >= SURE => Rating::Grave,
            it if it <= 1.0 - SURE => Rating::Reads,
            _ => Rating::Changes,
        };

        Self {
            scored,
            confidence: danger.max(1.0 - danger),
            worst: None,
        }
    }

    /// The worst of what a call is made of, which is what the call is drawn as.
    ///
    /// note: the fold is over [`Rated::shown`] rather than over the scores, and the order matters.
    /// `shown` is what lifts a reading nobody was sure of off green: a stage scored `0.4` at 95%
    /// and one scored `0.1` at 30% are `Reads` and `Changes` once it has run, and `Changes` is the
    /// honest answer for the pair. Folding the scores first picks the higher one, `0.4`, and draws
    /// the whole command green on the strength of the other stage's coin toss.
    /// `an_unsure_rating_is_never_drawn_safer_than_it_scored` holds one stage to this, and the
    /// fold is the same property for a command made of several.
    ///
    /// note: the whole command is one of the parts folded, always, and it is the only one that
    /// can see what the stages cannot - a pipeline whose every link is ordinary and whose
    /// composition is not. It is also what makes a stage whose answer never arrived cost a band
    /// that might have been raised rather than a wrong one: the fold can only ever come back at or
    /// above the band the whole command was given.
    ///
    /// note: the first *stage* to reach the worst band, where more than one part does, so what is
    /// pointed at is where the command first gets as bad as it gets. The whole command is kept
    /// only where no stage reaches its band: a destructive link usually makes the whole command
    /// read as destructive too, and keeping the whole on that tie would point at nothing in
    /// exactly the chain the pointing is for.
    ///
    /// note: and nothing is pointed at where every stage reaches the band. A stage is worth
    /// pointing at for being worse than the rest, and where none is, the first of equals says
    /// nothing: `cargo fmt && cargo test` drawn yellow with `cargo fmt` underlined reads as `fmt`
    /// being the part to worry about, and a green chain had its first stage underlined for no
    /// reason at all.
    fn worst_of(parts: impl IntoIterator<Item = Self>) -> Option<Self> {
        let parts: Vec<Self> = parts.into_iter().collect();
        let folded =
            parts
                .iter()
                .copied()
                .reduce(|best, next| match next.shown().cmp(&best.shown()) {
                    std::cmp::Ordering::Greater => next,
                    std::cmp::Ordering::Equal if best.worst.is_none() && next.worst.is_some() => {
                        next
                    }
                    _ => best,
                })?;
        let band = folded.shown();
        let below = |span: (usize, usize)| {
            parts
                .iter()
                .filter(|part| part.worst == Some(span))
                .all(|part| part.shown() < band)
        };
        let singled_out = parts.iter().filter_map(|part| part.worst).any(below);

        Some(Self {
            scored: band,
            worst: folded.worst.filter(|_| singled_out),
            ..folded
        })
    }

    /// The band this is actually drawn as.
    ///
    /// note: never a safer one than it scored, and never [`Rating::Reads`] where the advisor was
    /// not sure, which is `SURE`'s job: an uncertain answer is worth having and is not worth
    /// acting on as though it were a certain one. A spread distribution over a safety rubric is
    /// not evidence that a command is safe, and green is the one colour that would say it was.
    ///
    /// note: the confidence is drawn beside this rather than folded away into it, so that a person
    /// reading a yellow line can see whether it is yellow because the command changes something or
    /// yellow because nobody could tell.
    ///
    /// note: idempotent, and the fold behind [`Rated::worst`] leans on it. It only ever raises a
    /// `Reads`, so asking it about a band it has already answered with gives that band back -
    /// which is what lets `scored` hold either a raw reading or a fold of several without a caller
    /// having to know which it has.
    pub fn shown(self) -> Rating {
        match self.confidence >= SURE {
            true => self.scored,
            false => self.scored.max(Rating::Changes),
        }
    }
}

/// Why a command has no rating when the turn it was asked about was stopped first.
const NOT_WAITED: &str = "the turn was stopped, so the advisor was not waited for";

/// [`Careful`], with the shell commands it is going to ask about rated for the person answering.
pub struct Advised {
    /// The standing rules, which decide every call.
    careful: Arc<Careful>,
    /// The engine asked for the rating: a service over HTTP, or a process on this machine.
    /// Nothing in here finds out which - see [`SystemOne`].
    engine: Arc<dyn SystemOne>,
    /// Where it put each command it was asked to rate, or why it could not, for the question to
    /// draw.
    readings: Mutex<VecDeque<(ToolCallId, Result<Rated, String>)>>,
    /// What the engine has reported its answers cost, added up: see [`Advised::spent`].
    spent: AtomicU64,
    /// Whether the turn this is rating for has been asked to stop; see [`Advised::stop`].
    stopped: tokio::sync::watch::Sender<bool>,
}

impl Advised {
    /// Wraps a policy, rating the commands it asks about.
    pub fn new(careful: Arc<Careful>, engine: Arc<dyn SystemOne>) -> Self {
        Self {
            careful,
            engine,
            readings: Mutex::new(VecDeque::new()),
            spent: AtomicU64::new(0),
            stopped: tokio::sync::watch::Sender::new(false),
        }
    }

    /// Gives up on any rating in flight, and on any asked for until [`Advised::resume`].
    ///
    /// note: what makes a stop immediate. The advisor is asked inside the permission policy, which
    /// the kernel awaits while preparing the calls and which does not read the interrupt, and an
    /// ask is up to four attempts with a wait between - so a `ctrl+c` did nothing visible for as
    /// long as the service took, two minutes at worst. A rating decides nothing, so giving up on
    /// one cannot change a verdict: the question still comes, saying the advice was not waited
    /// for, and the kernel honours the stop before any call runs.
    ///
    /// note: a flag rather than a pulse, because the stop can land before the rating starts - the
    /// policy is asked after the interrupt, on the way to the kernel noticing it - and a pulse
    /// nobody was waiting on is a stop that did nothing.
    pub fn stop(&self) {
        self.stopped.send_replace(true);
    }

    /// Rates again, for the next turn.
    pub fn resume(&self) {
        self.stopped.send_replace(false);
    }

    /// The tokens the engine has reported its answers cost, over the life of this policy.
    ///
    /// note: kept here because nothing else sees them. The advisor is asked from inside the
    /// permission policy, which writes no event, so a `--spend` that added up `model.finished`
    /// alone was a ceiling on the model and none on the advisor - and one borrowing the session's
    /// key spends out of the same account. `App` charges what this has grown by since it last
    /// looked. A local engine that reports nothing adds nothing, which is what it costs.
    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// Whatever the engine last wanted to say for itself, if anything.
    ///
    /// note: passed through rather than kept here, because the thing with something to say is
    /// the engine and this is the only handle a caller has on one. A local engine loads a
    /// checkpoint before it can answer anything and says so as it goes; a hosted one says when
    /// it is backing off. Both have to reach a screen during the session and not only at startup,
    /// or an advisor that stops working mid-session stops silently.
    pub fn notice(&self) -> Option<String> {
        self.engine.notice()
    }

    /// Where the advisor put a command, if it was asked to place one and did.
    pub fn rating(&self, call: &ToolCallId) -> Option<Rated> {
        self.read(call)?.ok()
    }

    /// Why the advisor has no rating for a command it was asked to place.
    ///
    /// note: said rather than left as a missing line, because the failure is not random. The
    /// endpoints this program knows sit behind a firewall that refuses a request by what is in it,
    /// and what it refuses - `/etc/shadow`, a secret piped to `curl` - is the command the colour
    /// is most for. A question with no line looks like one nobody had anything to say about, and a
    /// person reading it that way has been told the command was not worth a colour.
    pub fn why_unrated(&self, call: &ToolCallId) -> Option<String> {
        self.read(call)?.err()
    }

    /// What was written down about a call, either way.
    fn read(&self, call: &ToolCallId) -> Option<Result<Rated, String>> {
        self.readings
            .lock()
            .iter()
            .find(|(known, _)| known == call)
            .map(|(_, reading)| reading.clone())
    }

    /// Asks where a command lands, and writes down the answer - or why there is none - for the
    /// question to draw.
    ///
    /// note: it returns nothing. There is no branch from here to a verdict, so the worst an outage
    /// can do is take the colour off the panel and put the reason there instead.
    async fn rate(&self, request: &PermissionRequest) {
        let reading = self.placed(request).await;

        let mut remembered = self.readings.lock();
        match remembered
            .iter_mut()
            .find(|(known, _)| known == &request.call)
        {
            Some(known) => known.1 = reading,
            None => {
                if remembered.len() == REMEMBERED {
                    remembered.pop_front();
                }
                remembered.push_back((request.call.clone(), reading));
            }
        }
    }

    /// Where a command lands, or why the advisor could not say.
    async fn placed(&self, request: &PermissionRequest) -> Result<Rated, String> {
        // note: through `inner`, because some models put every argument inside a wrapper object
        // and the panel unwraps one before drawing it. Reading the command from the other place
        // than the screen does would take spans into a string nobody is looking at, and the
        // stage underlined would be the wrong run of the right command
        let args = crate::tools::ops::inner(&request.args)
            .unwrap_or(std::borrow::Cow::Borrowed(&request.args));
        let cmd = args.get("cmd").and_then(Value::as_str).unwrap_or_default();
        let stages = stages(cmd);

        // note: all of them in one request, which is what makes taking a command apart affordable.
        // Each is evaluated on its own against the same state, so no stage's answer can be moved
        // by another's, and a command of `STAGES` stages costs the round trip a one-stage command
        // costs
        let mut questions = vec![
            (RATING.to_owned(), Question::score(PLACE, LEVELS)),
            (
                DANGER.to_owned(),
                Question::noul(RUIN).between(RUINED, INTACT),
            ),
        ];
        for (n, (from, to)) in stages.iter().enumerate() {
            questions.push((format!("{STAGE}-{n}"), placing(&cmd[*from..*to])));
            questions.push((format!("{DANGER}-{n}"), claiming(&cmd[*from..*to])));
        }

        let mut stopped = self.stopped.subscribe();
        let answers = tokio::select! {
            asked = self.engine.ask(state(request), questions) => {
                asked.map_err(|e| cut(&e.to_string()))?
            }
            _ = stopped.wait_for(|stopped| *stopped) => return Err(NOT_WAITED.to_owned()),
        };
        if let Some(usage) = &answers.usage {
            let tokens = usage
                .input_tokens
                .unwrap_or(0)
                .saturating_add(usage.output_tokens.unwrap_or(0));
            self.spent.fetch_add(tokens, Ordering::Relaxed);
        }

        // note: the score and the confidence together or not at all. A score with no confidence
        // beside it cannot be drawn by `Rated::shown`'s rule, and the safe reading of half an
        // answer is that nothing was said
        let read = |name: &str| Some(Rated::of(answers.score(name)?, answers.confidence(name)?));

        // note: a `noul`'s number is its own confidence, so there is no second field to pair it
        // with and no half-answer to guard against - see `Rated::claimed`
        let claim = |name: &str| Some(Rated::claimed(answers.noul(name)?));

        // note: the whole command is what decides whether anything was said at all, and it has
        // two ways of saying it. Either will do; neither is half an answer, because each is a
        // reading of the whole command on its own. Nothing at all is still nothing, and the safe
        // reading of that is the same here as it is above
        let whole: Vec<Rated> = [read(RATING), claim(DANGER)]
            .into_iter()
            .flatten()
            .collect();
        if whole.is_empty() {
            return Err(UNREAD.to_owned());
        }

        let placed = stages.iter().enumerate().flat_map(|(n, span)| {
            [
                read(&format!("{STAGE}-{n}")),
                claim(&format!("{DANGER}-{n}")),
            ]
            .into_iter()
            .flatten()
            .map(|part| Rated {
                worst: Some(*span),
                ..part
            })
            .collect::<Vec<_>>()
        });

        Rated::worst_of(whole.into_iter().chain(placed)).ok_or_else(|| UNREAD.to_owned())
    }
}

/// What the rubric is put to the model as, for a command taken as one thing.
pub(crate) const PLACE: &str = "A command is about to be run on the user's machine, and a person is about to \
                     be asked whether to let it. Place the command in `cmd` on this rubric by \
                     what it would do.";

/// And for one stage of one, which is placed with the whole command in the state beside it.
///
/// note: it says to place the stage and not the command, because the state carries the whole
/// command and has to: `cd /tmp && rm -rf *` is two stages whose second means nothing read on its
/// own, and a rubric answered about a fragment with no context is answered about a different
/// command. What the question asks for is a reading of the part, taken in view of the whole.
pub(crate) const PLACE_STAGE: &str = "A command is about to be run on the user's machine, and a person is \
                           about to be asked whether to let it. The whole command is in `cmd`. \
                           Place only the stage in `stage` on this rubric, by what that stage \
                           would do as part of that command.";

/// [`LEVELS`]' top band as a claim, for the whole command.
pub(crate) const RUIN: &str = "Would running the command in `cmd` change anything outside the working directory, destroy something that cannot be got back, or send something off this machine?";

/// And for one stage of it, read in view of the whole the way [`PLACE_STAGE`] is.
pub(crate) const RUIN_STAGE: &str = "A command is about to be run on the user's machine, and a person is about to be asked whether to let it. The whole command is in `cmd`. Would the stage in `stage` change anything outside the working directory, destroy something that cannot be got back, or send something off this machine?";

/// What each side of that claim means, in the words the rubric's own bands use.
pub(crate) const RUINED: &str = "it changes something outside the working directory, destroys something that cannot be got back, or sends something off this machine";

/// note: it names both of the lower bands, because a claim has one false side and the rubric has
/// two - and a `false` described as only the bottom one would be read as denying that a command
/// changes anything, which is not what is being asked.
pub(crate) const INTACT: &str = "it only looks, moves about, or changes files inside the working directory in a way git or a rebuild could undo";

/// The claim put to one stage, carrying the stage the way [`placing`] does.
fn claiming(stage: &str) -> Question {
    Question::noul(RUIN_STAGE)
        .between(RUINED, INTACT)
        .structured(json!({ "asked": RUIN_STAGE, "stage": stage }))
}

/// One stage of a command line, put on [`LEVELS`].
///
/// note: the stage travels as a *value* in an instructions object rather than interpolated into a
/// sentence, for the reason [`state`] is an object rather than a sentence built out of one - a
/// fragment carrying a newline or a quote cannot rearrange the question it is inside of, and a
/// stage of a command line is arbitrary text written by the model. `Question::structured`
/// replaces the sentence `score` was handed, so the sentence goes into the object with it.
fn placing(stage: &str) -> Question {
    Question::score(PLACE_STAGE, LEVELS).structured(json!({ "asked": PLACE_STAGE, "stage": stage }))
}

/// The byte ranges of a command line's own stages, or nothing where this is not a call it can
/// take apart.
///
/// note: four ways of answering nothing, and each of them leaves the whole-command rating to stand
/// on its own. A call whose `cmd` is not a string is not one this program knows to hold a command
/// line - the rating is asked for on [`Capability::exec`], which somebody else's tool may declare
/// while taking its command under another name, and guessing which field that is would be placing
/// a rubric on an argument nobody said was a command. A command with no joints in it is one
/// stage, and one stage folded with the whole is the whole. Past [`STAGES`] there are too many to
/// report on honestly - see the note there. And past [`ROOM`] the state the advisor is shown is a
/// *cut* of this command, so a stage taken from beyond the cut would be placed against a command
/// the model was never shown the end of.
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
///
/// note: the arguments inside the `call` wrapper, read through `inner` as `Advised::placed`
/// reads the command. Every question here says the command is in `cmd`, and what the rubric was
/// measured against put it at `arguments.cmd`; sent wrapped, it was at `arguments.call.cmd`, a
/// different question from the one measured.
pub(crate) fn state(request: &PermissionRequest) -> Value {
    let args = crate::tools::ops::inner(&request.args)
        .unwrap_or(std::borrow::Cow::Borrowed(&request.args));

    json!({
        "tool": request.tool,
        "capabilities": request
            .capabilities
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "arguments": capped(&args),
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

#[async_trait]
impl PermissionPolicy for Advised {
    fn why(&self, request: &PermissionRequest) -> Option<String> {
        self.careful.why(&request.call)
    }

    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let standing = self.careful.evaluate(request).await;

        // note: `Ask` only. An `Allow` is somebody's decision and runs without a question to draw
        // a rating in, and a `Deny` has none either - so rating either would send the arguments
        // of a call to a third party and put nothing on any screen in exchange
        //
        // note: and `exec:run` only, which is the capability rather than the tool's name. The
        // rubric is written about a command, and `shell` is the tool that takes one; a tool that
        // declares the capability is asking for the same thing whatever it calls itself, and one
        // that does not is not what `--advise` was turned on for
        if standing == Verdict::Ask && request.capabilities.contains(&Capability::exec("run")) {
            self.rate(request).await;
        }

        standing
    }
}

#[cfg(test)]
mod tests {
    use nachalnik::PermissionId;

    use super::*;
    use crate::tools::Subject;

    /// An address nothing is listening on, so the advisor fails the way an outage makes it fail.
    ///
    /// note: port 1, which is refused rather than filtered - the same distinction
    /// `waiting::tests::nobody_home` is careful about. A refused connection is not a timeout, so
    /// this also pins that the failure is not retried four times on the way to being ignored.
    fn unreachable() -> Arc<nachalnik_providers::system1::Client> {
        Arc::new(nachalnik_providers::system1::Client::new(
            "vendor/decider",
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

    /// One shell call, named, with the command the questions are put to.
    fn running(id: &str, cmd: &str) -> PermissionRequest {
        let mut request = asking("shell", Capability::exec("run"));
        request.call = ToolCallId::from(id);
        request.args = Arc::new(json!({ "cmd": cmd }));
        request
    }

    /// An engine that answers off the command it was shown, so two calls get two answers.
    ///
    /// note: two bands rather than one, because a test that cannot tell them apart cannot say
    /// whose reading it is holding.
    struct ByCommand;

    #[async_trait]
    impl SystemOne for ByCommand {
        async fn ask(
            &self,
            state: Value,
            _questions: Vec<(String, Question)>,
        ) -> Result<nachalnik_providers::system1::Answers, nachalnik::BoxError> {
            let score = match state["arguments"]["cmd"].as_str().unwrap_or_default() {
                it if it.contains("rm -rf") => 2.0,
                _ => 0.0,
            };
            Ok(nachalnik_providers::system1::Answers::read(json!({
                "model": "by-command",
                "answers": {
                    RATING: { "type": "score", "score": score, "confidence": 0.99 }
                },
            })))
        }

        fn named(&self) -> String {
            "by-command".to_owned()
        }
    }

    /// A question about something other than a command is not rated, and nothing about it goes.
    #[tokio::test]
    async fn a_call_already_going_to_be_asked_about_is_not_sent_anywhere() {
        // nothing set, so `fs:read` is `ask` - and the rubric is written about a command
        let engine = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), engine.clone());
        let request = asking("read", Capability::fs("read"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(engine.attempts(), 0, "nothing should have left the machine");
    }

    /// An allowed command runs, and nothing about it leaves the machine.
    ///
    /// note: the property the rest of this file is arranged around. An `allow` is somebody's
    /// decision: there is no question for a rating to be drawn in, and nothing the advisor could
    /// say is allowed to reopen it - so it is not asked.
    #[tokio::test]
    async fn an_allowed_command_is_not_sent_anywhere() {
        let careful = Arc::new(Careful::new());
        careful.set(
            &Subject::Capability(Capability::exec("run")),
            Verdict::Allow,
        );

        let engine = unreachable();
        let advised = Advised::new(careful, engine.clone());
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Allow);
        assert_eq!(engine.attempts(), 0, "nothing should have left the machine");
        assert!(advised.rating(&request.call).is_none());
    }

    /// A reason too long for the question's header is cut and says it was, and a short one is
    /// left whole.
    #[test]
    fn a_long_reason_is_cut_and_says_so() {
        assert_eq!(cut("the endpoint refused it"), "the endpoint refused it");

        let long = cut(&"a firewall's page about it ".repeat(20));
        assert!(long.ends_with('…'), "{long}");
        assert!(long.chars().count() <= SAID + 1, "{long}");
    }

    /// The rubric is read by the nearest level, not by the one it has passed.
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

    /// A claim lands on the top band when it holds, the bottom when it is confidently false, and
    /// the middle the rest of the time.
    ///
    /// note: the pair with `a_score_lands_on_the_band_it_is_nearest`, and the case worth having it
    /// for is the middle. A `noul` at 0.51 is the advisor saying it could not tell, and reading it
    /// by which side of a half it fell on would draw a coin toss in red - the same mistake
    /// `Rated::shown` exists to stop one primitive along, made here instead where `shown` could
    /// not undo it, because that rule only ever raises a band.
    #[test]
    fn a_claim_lands_on_the_top_band_only_when_it_holds() {
        let band = |danger| Rated::claimed(danger).shown();

        // it holds, and the advisor is sure of it
        assert_eq!(band(1.0), Rating::Grave);
        assert_eq!(band(SURE), Rating::Grave);
        // it does not hold, and the advisor is equally sure of that
        assert_eq!(band(0.0), Rating::Reads);
        assert_eq!(band(1.0 - SURE), Rating::Reads);

        // and in between there is no answer, whichever side of a half it happens to fall
        for unsure in [0.31, 0.49, 0.5, 0.51, 0.69] {
            assert_eq!(band(unsure), Rating::Changes, "{unsure}");
        }

        // the confidence is the claim's own distance from a half, which is what makes the band
        // above and `shown`'s rule agree rather than fight
        assert_eq!(Rated::claimed(0.9).confidence, 0.9);
        assert_eq!(Rated::claimed(0.1).confidence, 0.9);
        assert!(Rated::claimed(0.5).confidence < SURE);
    }

    /// The two readings of one command fold, and either alone is enough to draw a line.
    ///
    /// note: the property the second question is there for. One engine reads the rubric and misses
    /// destructive commands when it is taken away; another finds more of them by the claim than by
    /// the rubric. Folding them means a command either is right about carries it, and the fold is
    /// over `shown` for the reason `worst_of` documents.
    #[test]
    fn a_command_is_drawn_by_whichever_reading_of_it_is_worse() {
        // the rubric saw nothing and the claim did
        let folded = Rated::worst_of([Rated::of(0.1, 0.99), Rated::claimed(0.95)]);
        assert_eq!(folded.expect("it folds").shown(), Rating::Grave);

        // and the other way round, which is the case for an engine that reads the rubric well
        let folded = Rated::worst_of([Rated::of(2.0, 0.99), Rated::claimed(0.05)]);
        assert_eq!(folded.expect("it folds").shown(), Rating::Grave);

        // neither of them sure of anything is still a question rather than a clean bill
        let folded = Rated::worst_of([Rated::of(0.0, 0.1), Rated::claimed(0.5)]);
        assert_eq!(folded.expect("it folds").shown(), Rating::Changes);
    }

    /// A rating nobody is sure of is never drawn green, and is never drawn safer than it scored.
    ///
    /// note: the property this feature stands on. A spread distribution over a safety rubric is
    /// not evidence that a command is safe - it is the advisor saying it could not tell - and
    /// green is the one colour that would report it as the former.
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
    #[tokio::test]
    async fn a_command_the_rules_already_refuse_is_not_rated() {
        let careful = Arc::new(Careful::new());
        careful.set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);

        let engine = unreachable();
        let advised = Advised::new(careful, engine.clone());
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Deny);
        assert_eq!(engine.attempts(), 0, "nothing should have left the machine");
        assert!(advised.rating(&request.call).is_none());

        // and the reason the rules wrote down is the one handed out, since a wrapper that decides
        // nothing must not lose the sentence the model is told a refusal by
        let why = advised
            .why(&request)
            .expect("the rule that refused it is still accounted for");
        assert!(why.contains("`exec:run`"), "{why}");
    }

    /// And one going to be asked about is rated, which is the only call that is.
    ///
    /// note: the endpoint is not there, so what this pins is that the request was *attempted* -
    /// the rating itself needs a live model and lives in `tests/advise.rs`. It pairs with
    /// `a_call_already_going_to_be_asked_about_is_not_sent_anywhere`, which passes because it asks
    /// about `fs:read`; this one is what `exec:run` changes.
    #[tokio::test]
    async fn a_command_somebody_is_about_to_be_asked_about_is_rated() {
        let engine = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), engine.clone());
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(engine.attempts(), 1, "the rating was asked for");
        // and an advisor that could not be reached leaves no rating rather than a reassuring one,
        // and a reason where the rating would have been
        assert!(
            advised.rating(&request.call).is_none(),
            "an outage draws no band, rather than a green one"
        );
        assert!(
            advised.why_unrated(&request.call).is_some(),
            "and says it could not rate the command"
        );
    }

    /// A score that came with no confidence is half an answer, and half an answer is none.
    ///
    /// note: it was read with a confidence of `0.0`, which is a rating "0% sure" drawn in the
    /// panel - or, while the engine handed out `NaN` for the missing figure, "NaN% sure" and a
    /// message a remote client could not read. Refused, it is a question with a reason beside it
    /// rather than a rating.
    #[tokio::test]
    async fn a_score_with_no_confidence_is_not_a_rating() {
        struct Half;

        #[async_trait]
        impl SystemOne for Half {
            async fn ask(
                &self,
                _state: Value,
                _questions: Vec<(String, Question)>,
            ) -> Result<nachalnik_providers::system1::Answers, nachalnik::BoxError> {
                Ok(nachalnik_providers::system1::Answers::read(json!({
                    "model": "half",
                    "answers": { RATING: { "type": "score", "score": 0.1 } },
                })))
            }

            fn named(&self) -> String {
                "half".to_owned()
            }
        }

        let advised = Advised::new(Arc::new(Careful::new()), Arc::new(Half));
        let request = asking("shell", Capability::exec("run"));

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert!(advised.rating(&request.call).is_none());
        assert!(advised.why_unrated(&request.call).is_some());
    }

    /// What an answer reports it cost is added up, for the ceiling to charge.
    ///
    /// note: an answer that could not be read as a rating still cost what it cost, so the half
    /// answer is the case here: the figure is counted whether or not a colour came of it.
    #[tokio::test]
    async fn what_an_answer_cost_is_added_up() {
        struct Billed;

        #[async_trait]
        impl SystemOne for Billed {
            async fn ask(
                &self,
                _state: Value,
                _questions: Vec<(String, Question)>,
            ) -> Result<nachalnik_providers::system1::Answers, nachalnik::BoxError> {
                Ok(nachalnik_providers::system1::Answers::read(json!({
                    "model": "billed",
                    "answers": { RATING: { "type": "score", "score": 0.1 } },
                    "usage": { "input_tokens": 300, "output_tokens": 20 },
                })))
            }

            fn named(&self) -> String {
                "billed".to_owned()
            }
        }

        let advised = Advised::new(Arc::new(Careful::new()), Arc::new(Billed));
        assert_eq!(advised.spent(), 0);
        let request = asking("shell", Capability::exec("run"));
        advised.evaluate(&request).await;
        advised.evaluate(&request).await;
        assert_eq!(advised.spent(), 640, "both answers, input and output");

        // and an engine that could not be reached cost nothing
        let unreached = Advised::new(Arc::new(Careful::new()), unreachable());
        unreached.evaluate(&request).await;
        assert_eq!(unreached.spent(), 0);
    }

    /// A stop gives up on a rating at once, and the question says the advice was not waited for.
    ///
    /// note: an engine that never answers, which is a busy service's four tries and their waits at
    /// their longest. Before, a `ctrl+c` here did nothing for as long as the ask took; the verdict
    /// is the standing rules' either way, so giving up on the rating changes nothing but the wait.
    #[tokio::test]
    async fn a_stop_does_not_wait_for_the_advisor() {
        struct Silent;

        #[async_trait]
        impl SystemOne for Silent {
            async fn ask(
                &self,
                _state: Value,
                _questions: Vec<(String, Question)>,
            ) -> Result<nachalnik_providers::system1::Answers, nachalnik::BoxError> {
                std::future::pending().await
            }

            fn named(&self) -> String {
                "silent".to_owned()
            }
        }

        let advised = Arc::new(Advised::new(Arc::new(Careful::new()), Arc::new(Silent)));
        let request = asking("shell", Capability::exec("run"));

        // stopped while it is being asked
        let asking_it = tokio::spawn({
            let (advised, request) = (advised.clone(), request.clone());
            async move { advised.evaluate(&request).await }
        });
        tokio::task::yield_now().await;
        advised.stop();
        let verdict = tokio::time::timeout(std::time::Duration::from_secs(5), asking_it)
            .await
            .expect("the stop was waited out rather than taken")
            .expect("not a panic");
        assert_eq!(verdict, Verdict::Ask, "the verdict is the standing rules'");
        assert_eq!(
            advised.why_unrated(&request.call).as_deref(),
            Some(NOT_WAITED)
        );

        // and stopped before it was asked, which is the order the kernel can put the two in
        let verdict = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            advised.evaluate(&request),
        )
        .await
        .expect("a stop that came first was missed");
        assert_eq!(verdict, Verdict::Ask);

        // and the next turn is rated again, rather than stopped for good
        advised.resume();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                advised.evaluate(&request)
            )
            .await
            .is_err(),
            "a resumed advisor was not asked"
        );
    }

    /// A call is rated once, however many times the question comes round.
    ///
    /// note: the memory is keyed by the call, and what that key is for is that the second
    /// question about a call must read the second reading rather than the first. A lookup that
    /// answered "any call but this one" leaves the first answer where it is and puts a second
    /// entry under a call nobody asked about.
    #[tokio::test]
    async fn a_call_rated_again_is_the_one_that_is_read_back() {
        let advised = Advised::new(Arc::new(Careful::new()), Arc::new(ByCommand));

        let first = running("call-1", "ls");
        let second = running("call-2", "ls");
        assert_eq!(advised.evaluate(&first).await, Verdict::Ask);
        assert_eq!(
            advised.rating(&first.call).map(|it| it.shown()),
            Some(Rating::Reads),
            "the first reading of the first call"
        );

        // the same call asked about again, this time as something that would not be green
        let again = running("call-1", "rm -rf /");
        assert_eq!(advised.evaluate(&again).await, Verdict::Ask);
        assert_eq!(
            advised.rating(&again.call).map(|it| it.shown()),
            Some(Rating::Grave),
            "the reading written down for `call-1` is the one this call got"
        );

        // and the call nobody asked about twice still holds what it was given
        assert_eq!(advised.evaluate(&second).await, Verdict::Ask);
        assert_eq!(
            advised.rating(&second.call).map(|it| it.shown()),
            Some(Rating::Reads),
            "`call-2` is not left holding another call's reading"
        );
        assert_eq!(advised.readings.lock().len(), 2, "one entry per call");
    }

    /// A call rated before the latest one is still rated.
    ///
    /// note: the memory is bounded, so that a session that never reads the panel grows no queue
    /// behind it, and it lets go of the oldest - not of everything but the newest, which is a
    /// rating gone before the person it was for is shown it.
    #[tokio::test]
    async fn a_reading_before_the_latest_is_still_there() {
        let advised = Advised::new(Arc::new(Careful::new()), Arc::new(ByCommand));

        let (earlier, latest) = (running("call-1", "ls"), running("call-2", "ls"));
        for request in [&earlier, &latest] {
            assert_eq!(advised.evaluate(request).await, Verdict::Ask);
        }
        assert!(advised.rating(&earlier.call).is_some());
        assert!(advised.rating(&latest.call).is_some());
    }

    /// A command of eight stages costs the round trip a command of one costs.
    ///
    /// note: the property that makes taking a command apart worth doing here at all, and the
    /// reason this model rather than a chat one. Every stage is a question in the *same* request,
    /// evaluated on its own against the same state, so none of them can be moved by another's
    /// answer and the whole fold is paid for once. A question per request would be eight round
    /// trips in front of somebody waiting to press `y`, and nobody would keep the feature.
    #[tokio::test]
    async fn every_stage_of_a_command_is_asked_about_in_one_request() {
        let engine = unreachable();
        let advised = Advised::new(Arc::new(Careful::new()), engine.clone());

        let mut request = asking("shell", Capability::exec("run"));
        let cmd = ["true"; STAGES].join(" && ");
        request.args = Arc::new(json!({ "cmd": cmd }));
        assert_eq!(stages(request.args["cmd"].as_str().unwrap()).len(), STAGES);

        assert_eq!(advised.evaluate(&request).await, Verdict::Ask);
        assert_eq!(engine.attempts(), 1, "one request, however many stages");
    }

    /// The fold is over what each stage is *shown* as, and taking the scores first loses the
    /// property the whole rubric rests on.
    ///
    /// note: the case is not a corner. A stage scored `0.4` at 95% is a confident `Reads`; one
    /// scored `0.1` at 30% is a reading nobody could make, which `Rated::shown` lifts to `Changes`
    /// because green is the one colour that must never come out of a coin toss. Fold the *scores*
    /// and `0.4` wins, and the command is drawn green on the strength of the other stage's
    /// uncertainty - a higher number standing for a safer command, which is exactly backwards.
    /// Fold what each is shown as and the pair is `Changes`.
    ///
    /// note: the pair with `an_unsure_rating_is_never_drawn_safer_than_it_scored`, which holds
    /// one stage to this. Passing that says nothing about a fold of several, which is why this is
    /// separate rather than another case in it.
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
    /// The whole command is one of the parts folded, always, so a missing stage costs a raised band
    /// that might have happened and cannot produce one that should not have.
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

        // but a stage as bad as the whole is what made it that bad, and is pointed at - which is
        // the ordinary case, since a destructive link makes the whole read as destructive too
        let folded = Rated::worst_of([Rated::of(2.0, 0.99), at((0, 4), 0.0), at((7, 11), 2.0)])
            .expect("it folds");
        assert_eq!(folded.worst, Some((7, 11)));
        assert_eq!(folded.shown(), Rating::Grave);
    }

    /// And nothing is pointed at where no stage is worse than another, whatever the band.
    #[test]
    fn stages_that_all_reach_the_band_point_at_none_of_them() {
        let at = |span, score| Rated {
            worst: Some(span),
            ..Rated::of(score, 0.99)
        };

        for score in [0.0, 1.0, 2.0] {
            let folded = Rated::worst_of([
                Rated::of(score, 0.99),
                at((0, 9), score),
                at((13, 23), score),
            ])
            .expect("it folds");
            assert_eq!(folded.worst, None, "every stage scored {score}");
        }

        // a stage's two readings are one stage: the claim below the band does not single it out
        // while its score is at it
        let folded = Rated::worst_of([
            Rated::of(1.0, 0.99),
            at((0, 9), 1.0),
            Rated {
                worst: Some((0, 9)),
                ..Rated::claimed(0.0)
            },
            at((13, 23), 1.0),
        ])
        .expect("it folds");
        assert_eq!(folded.worst, None);

        // and one stage kept below it is what lets the others be pointed at
        let folded = Rated::worst_of([Rated::of(1.0, 0.99), at((0, 9), 1.0), at((13, 23), 0.0)])
            .expect("it folds");
        assert_eq!(folded.worst, Some((0, 9)));
    }

    /// `shown` run over its own answer answers the same thing, which is what `worst_of` leans on.
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

    /// A shell call as the tool really receives it puts the command where the questions say it is.
    ///
    /// note: every other test here builds its arguments bare, and every shell call arrives
    /// wrapped - `{"call": {"action": "run", "cmd": ...}}` - so the state they checked was never
    /// the state sent.
    #[test]
    fn a_wrapped_call_is_shown_with_the_command_in_cmd() {
        let request = PermissionRequest {
            id: PermissionId(1),
            call: ToolCallId::from("call-1"),
            tool: "shell".to_owned(),
            capabilities: vec![Capability::exec("run")],
            args: Arc::new(json!({ "call": { "action": "run", "cmd": "rm -rf target" } })),
        };

        assert_eq!(state(&request)["arguments"]["cmd"], "rm -rf target");
    }

    /// A string cut to the cap ends on the last whole character that fits, where one straddles it.
    #[test]
    fn a_string_is_cut_at_the_last_character_that_fits() {
        // `é` is two bytes and starts on every odd one here, so the `a` in front puts the cap in
        // the middle of one
        let straddling = format!("a{}", "é".repeat(ROOM));
        assert_eq!(
            capped(&Value::String(straddling.clone())),
            Value::String(format!(
                "{}… (cut; {} bytes in all)",
                &straddling[..ROOM - 1],
                straddling.len()
            ))
        );
    }

    /// A joint at the end of a command leaves nothing behind it, and nothing is not a stage.
    ///
    /// note: the range of an empty stage points at the same byte twice, which a client asked to
    /// underline it would draw as a caret at the end of the command - on a question the model was
    /// never shown a stage of. The stage is dropped rather than clamped back to the joint beside
    /// it, which would point at a stage the model did read and call it a different one.
    #[test]
    fn a_stage_with_nothing_in_it_is_not_a_stage() {
        // a trailing joint leaves a trailing stage of nothing
        let cmd = "ls | grep x |";
        assert_eq!(stages(cmd), [(0, 2), (5, 11)]);

        // and one between two joints leaves the same, in the middle this time
        let cmd = "ls | | wc -l";
        assert_eq!(stages(cmd), [(0, 2), (7, 12)]);
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

            // and the path survives it, which is why the cap is per value. Cutting the rendered
            // arguments would take this away: `contents` sorts before `path`, so the one field the
            // decision turns on would be the first thing to go
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
