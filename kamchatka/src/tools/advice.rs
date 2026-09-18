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
//! note: the invariant *nothing in a model's output reaches the policy* still holds, and is worth
//! being precise about. What reaches this is the tool name and the arguments, both as data, which
//! is what reached [`Careful`] before. The agent under judgement cannot address the judge: there
//! is no path for it to add a sentence to this request, and a tool call that *says* it has been
//! approved is a string in `args` like any other.

use std::{collections::VecDeque, sync::Arc};

use nachalnik::{PermissionPolicy, PermissionRequest, ToolCallId, Verdict, async_trait};
use nachalnik_providers::typesafe::{Jev, Question};
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

/// [`Careful`], with a model asked about whatever it was going to allow.
pub struct Advised {
    /// The standing rules, which decide first and decide alone whenever this cannot reach a
    /// model.
    careful: Arc<Careful>,
    /// The model asked for the second opinion.
    jev: Arc<Jev>,
    /// What it said about each call, for [`Advised::said`] and for the refusal the model reads.
    said: Mutex<VecDeque<(ToolCallId, String)>>,
}

impl Advised {
    /// Wraps a policy in a second opinion.
    pub fn new(careful: Arc<Careful>, jev: Arc<Jev>) -> Self {
        Self {
            careful,
            jev,
            said: Mutex::new(VecDeque::new()),
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

        // note: asked only about what would otherwise run. A call already heading for `Ask` or
        // `Deny` cannot be made stricter by anything the model says, so asking would spend a
        // round trip and somebody's money to learn nothing - and would put a network request in
        // front of the refusal a person is waiting to see
        if standing != Verdict::Allow {
            return standing;
        }

        let asked = self
            .jev
            .ask(
                state(request),
                [
                    (
                        VERDICT,
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
                        IRREVERSIBLE,
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

    /// An address nothing is listening on, so the advisor fails the way an outage makes it fail.
    ///
    /// note: port 1, which is refused rather than filtered - the same distinction
    /// `waiting::tests::nobody_home` is careful about. A refused connection is not a timeout, so
    /// this also pins that the failure is not retried four times on the way to being ignored.
    fn unreachable() -> Arc<Jev> {
        Arc::new(Jev::new("jev-latest", "http://127.0.0.1:1", "not-a-key"))
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

        let advised = Advised::new(careful, unreachable());
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
        let advised = Advised::new(careful, jev.clone());
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
        let advised = Advised::new(Arc::new(Careful::new()), jev.clone());
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
