//! Waiting on a server that is slow, silent or busy - and knowing which of the three it is.
//!
//! note: shared by both dialects rather than written twice, which is why it is `pub(crate)` and
//! why it names nothing dialect-specific. A stream that has gone quiet, a request that was refused
//! with a `Retry-After`, and one somebody pressed escape on are three different answers to "send
//! it again?", and getting them wrong costs either a turn or somebody's money.

use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use nachalnik::{BoxError, DeltaSink, ModelResponse, StopReason};
use parking_lot::Mutex;
use serde_json::Value;

use crate::{
    out_of_quota,
    reading::{complaint, failure},
    refused,
};

/// How many times a request is sent when the server keeps saying it is busy, the first included.
///
/// note: counted per request, not on the provider. A counter every request draws from and any of
/// them resets gives concurrent requests against a busy endpoint one allowance between them, so a
/// request can fail its first attempt and give up without being retried at all.
pub(crate) const RETRIES: usize = 4;

/// The longest a server may ask to be left alone before this stops waiting and says so.
///
/// note: for the difference between a busy server and one that has said no until tomorrow. A
/// per-minute limit answers `Retry-After: 5`; a spent daily quota answers with the seconds until
/// midnight, and sitting through four doublings to discover that wastes the turn and the wait.
pub(crate) const LINGER: Duration = Duration::from_secs(60);

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
pub(crate) const HEARTBEAT: Duration = Duration::from_millis(120);

/// How long a stream may say nothing before the person watching is told about it.
pub(crate) const QUIET: Duration = Duration::from_secs(10);

/// How much longer it has to keep saying nothing before being mentioned again.
pub(crate) const AGAIN: Duration = Duration::from_secs(30);

/// How long a *stream* may say nothing before the request is given up on.
pub(crate) const PATIENCE: Duration = Duration::from_secs(150);

/// The same, for an answer that is not streamed at all.
///
/// note: ten minutes, and it has to be minutes rather than the two and a half above, because
/// nothing arrives until the whole answer does: a model asked for a long answer says nothing for
/// as long as it takes to write one, and there is no fragment to reset the watch. A stream is
/// different, and 150s of silence in the middle of one really is a stall.
pub(crate) const WHOLE_ANSWER: Duration = Duration::from_secs(600);

/// What a stream's silence has come to mean.
pub(crate) enum Silence {
    /// Long enough to be worth saying out loud, this many whole seconds in.
    Worth(u64),
    /// Long enough to stop waiting.
    Enough,
    /// Not long enough to be either.
    Ordinary,
}

/// Watches the gap since the last byte of a stream.
///
/// note: [`HEARTBEAT`] only makes a stalled request *interruptible*. It wakes up, checks whether
/// escape was pressed, and goes back to waiting - so a server that answers the connection and
/// then goes quiet, which is exactly what an overloaded one does, would leave the status line
/// reading `asking` for ever with nothing to tell it apart from a model that is thinking hard.
/// This is the part that says so, and eventually the part that stops.
pub(crate) struct Vigil {
    /// When something last arrived.
    last: Instant,
    /// The silence already mentioned, in whole seconds; zero when there is nothing to mention.
    said: u64,
    /// How long a silence may run before this gives up on it.
    patience: Duration,
}

impl Vigil {
    /// Starts watching, now, with the patience a stream gets.
    pub(crate) fn new() -> Self {
        Self::waiting(PATIENCE)
    }

    /// The same, for a wait of a different shape; see [`WHOLE_ANSWER`].
    pub(crate) fn waiting(patience: Duration) -> Self {
        Self {
            last: Instant::now(),
            said: 0,
            patience,
        }
    }

    /// Something arrived; returns whether the quiet before it had been mentioned.
    pub(crate) fn heard(&mut self) -> bool {
        let mentioned = self.said > 0;
        self.last = Instant::now();
        self.said = 0;

        mentioned
    }

    /// Nothing has arrived; what that has come to mean.
    pub(crate) fn waited(&mut self) -> Silence {
        self.judge(self.last.elapsed())
    }

    /// The same, for a silence of a given length.
    ///
    /// note: split out so that the rule can be tested without a socket and a wall clock. What is
    /// left in `waited` is the clock reading, which has nothing in it to get wrong.
    fn judge(&mut self, silent: Duration) -> Silence {
        if silent >= self.patience {
            return Silence::Enough;
        }

        // once, and then at intervals: a line a second for two and a half minutes would bury the
        // conversation it was reporting on
        let due = match self.said {
            0 => QUIET.as_secs(),
            said => said + AGAIN.as_secs(),
        };
        let seconds = silent.as_secs();
        if seconds >= due {
            self.said = seconds;
            return Silence::Worth(seconds);
        }

        Silence::Ordinary
    }
}

/// Why a request never became a response.
pub(crate) enum Unsent {
    /// The transport gave up on it.
    Transport(reqwest::Error),
    /// Nothing came back at all, for this long - [`PATIENCE`], or [`WHOLE_ANSWER`] where the
    /// answer was never going to arrive in pieces.
    Silent(Duration),
    /// Somebody pressed escape before the answer had started.
    Interrupted,
}

impl Unsent {
    /// Whether sending it again is worth anything.
    pub(crate) fn worth_waiting_out(&self) -> bool {
        match self {
            Self::Transport(e) => worth_waiting_out(e),
            // the same thing the transport's own timeout means, arrived at by counting rather
            // than by being told: a server that took the connection and went quiet is busy
            Self::Silent(_) => true,
            Self::Interrupted => false,
        }
    }

    /// What happened, as the middle of a sentence whose subject is the model.
    pub(crate) fn what_happened(&self) -> &'static str {
        match self {
            // worth telling apart: one of them hung up, the other never spoke
            Self::Transport(_) => "did not answer in time",
            Self::Silent(_) => "has not answered at all",
            Self::Interrupted => "was interrupted",
        }
    }

    /// The error to end the turn with, once there is no patience left to spend.
    pub(crate) fn giving_up(self, model: &str) -> BoxError {
        match self {
            Self::Transport(e) => e.into(),
            Self::Silent(waited) => format!(
                "{model} never answered; giving up after {}s",
                waited.as_secs()
            )
            .into(),
            Self::Interrupted => "interrupted".into(),
        }
    }
}

/// The answer to a request that was stopped before it had one.
///
/// note: not an error. Somebody asked for this, and a red line on the screen for doing as asked
/// reads as a bug in the program rather than as an answer to the key that was pressed.
pub(crate) fn interrupted() -> ModelResponse {
    ModelResponse {
        content: None,
        reasoning: None,
        tool_calls: Vec::new(),
        stop: StopReason::Other("interrupted".to_owned()),
        usage: None,
        raw: None,
    }
}

/// What every notice about a silence ends with.
///
/// note: a constant because it is the part that must not drift, and because it is the part this
/// crate is entitled to say. What it is handed is a [`DeltaSink`], and what it asks is whether
/// that has been interrupted; how a caller decides to set it is none of its business. So it names
/// no key: a key is true of one client and advice nobody can take in another - a headless run
/// prints this down a pipe, where it would tell whoever reads it to press a key at a program that
/// has no keyboard.
const GIVES_UP: &str = "an interrupt gives up on it";

/// A model that has not said anything at all yet.
pub(crate) fn not_answered(model: &str, seconds: u64) -> String {
    format!("{model} has not answered for {seconds}s; {GIVES_UP}")
}

/// A model that started answering and then stopped.
///
/// note: told apart from [`not_answered`] because the two are different news. Nothing at all yet
/// is a server that may never have got the request; a stream that stops halfway is one that took
/// it and is in trouble - and a turn that asks for a tool makes two requests, so the second is
/// the shape "it hangs whenever it uses a tool" really has.
pub(crate) fn gone_quiet(model: &str, seconds: u64) -> String {
    format!("{model} has said nothing for {seconds}s; {GIVES_UP}")
}

/// Whose answer is being waited for, and where to say how the wait is going.
///
/// note: the model is read once per request and used for every line said about it: a name that
/// changed halfway through would make one wait look like two.
pub(crate) struct Asking<'a> {
    /// The model being asked, as every notice about this request names it.
    pub(crate) model: &'a str,
    /// Where the answer goes as it arrives, and where an interrupt is asked about.
    pub(crate) deltas: &'a DeltaSink,
    /// Where a notice goes, for [`Endpoint::take_notice`](crate::Endpoint::take_notice).
    pub(crate) notice: &'a Mutex<Option<String>>,
}

impl Asking<'_> {
    /// Puts a notice up, in place of any nobody has taken yet.
    pub(crate) fn say(&self, said: String) {
        *self.notice.lock() = Some(said);
    }
}

/// What a request came to, once the server stopped being busy.
pub(crate) enum Sent {
    /// Somebody asked to stop before there was an answer to read.
    Interrupted,
    /// A whole answer, read.
    Whole(Value),
    /// A response whose body is still arriving.
    Streaming(reqwest::Response),
}

/// Why an attempt is worth making again, if the retry rules agree.
enum Busy {
    /// Nothing came back, in a way that a busy server produces.
    Unsent(Unsent),
    /// The server answered, and the answer was a refusal.
    Refused {
        /// The status, or the code inside an error object that came with a good one.
        code: u64,
        /// What to say about it if it is not waited out.
        said: String,
        /// Whether it is the kind of refusal that goes away.
        transient: bool,
        /// How long the server asked to be left, where it said.
        asked: Option<Duration>,
    },
}

/// Sends a request, waiting out a server that is merely busy, and hands back what came of it.
///
/// note: waiting and trying again is the *provider's* business. A free tier answers "busy" often
/// enough that not retrying makes the whole thing look broken when it is not - and the kernel
/// must not silently send a request twice behind a caller's back. The count is this request's
/// and nobody else's; see [`RETRIES`].
///
/// note: `request` builds the request afresh for each attempt, because a sent one is spent. It is
/// also everything the dialects do differently here: a path, a header, a body.
///
/// note: a whole answer is read *here* rather than by the caller, because the OpenAI dialect's
/// other way of saying 429 is an `error` object inside a perfectly good 200 - and a limit
/// reported that way is exactly as worth waiting out as one reported as a status.
pub(crate) async fn sent(
    asking: &Asking<'_>,
    attempts: &AtomicUsize,
    limit: Option<usize>,
    streaming: bool,
    request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<Sent, BoxError> {
    // nothing arrives until the whole answer does, when it is not a stream, so the silence that
    // means "this has stalled" is a much longer one
    let patience = match streaming {
        true => PATIENCE,
        false => WHOLE_ANSWER,
    };
    let mut tried = 0;

    loop {
        attempts.fetch_add(1, Ordering::SeqCst);
        tried += 1;

        let busy = match watched(request().send(), asking, patience).await {
            // nobody is owed an error for being obeyed
            Err(Unsent::Interrupted) => return Ok(Sent::Interrupted),
            // a connection that timed out is a busy server wearing different clothes. A refused
            // connection is *not* this - it is a definite answer, usually an address with nothing
            // behind it, and making a typo take four doublings to report helps nobody
            Err(reason) if reason.worth_waiting_out() => Busy::Unsent(reason),
            Err(reason) => return Err(reason.giving_up(asking.model)),
            Ok(response) if streaming && response.status().is_success() => {
                return Ok(Sent::Streaming(response));
            }
            Ok(response) => {
                let status = response.status();
                // the server's own answer to "when?", where it gives one. Guessing at a doubling
                // is for a server that did not say
                let asked = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let body = match body(response, asking, patience).await {
                    Ok(Some(body)) => body,
                    Ok(None) => return Ok(Sent::Interrupted),
                    Err(e) if status.is_success() => return Err(e),
                    // a refusal whose body never arrived is still a refusal, and its status says
                    // which kind
                    Err(_) => String::new(),
                };

                match status.is_success() {
                    true => {
                        let payload: Value = serde_json::from_str(&body).map_err(|e| {
                            let short: String = body.chars().take(300).collect();
                            format!("the answer was not JSON ({e}): {short}")
                        })?;
                        let Some(error) = payload.get("error").filter(|error| !error.is_null())
                        else {
                            return Ok(Sent::Whole(payload));
                        };
                        let code = error["code"].as_u64().unwrap_or_default();
                        Busy::Refused {
                            code,
                            said: failure(error),
                            transient: passing(code) && !out_of_quota(&body),
                            asked,
                        }
                    }
                    // read before deciding, because a spent daily quota is a 429 that will still be
                    // one in a minute
                    false => Busy::Refused {
                        code: status.as_u16().into(),
                        said: complaint(status, &body),
                        transient: passing(status.as_u16().into()) && !out_of_quota(&body),
                        asked,
                    },
                }
            }
        };

        let doubling = Duration::from_secs(1 << tried);
        let (wait, what) = match busy {
            Busy::Unsent(reason) if tried >= RETRIES => {
                return Err(reason.giving_up(asking.model));
            }
            Busy::Unsent(reason) => (doubling, reason.what_happened().to_owned()),
            Busy::Refused {
                code,
                said,
                transient,
                asked,
            } => {
                let wait = asked.unwrap_or(doubling);
                if !transient || tried >= RETRIES || wait > LINGER {
                    let mut said = said;
                    if transient && wait > LINGER {
                        said.push_str(&format!(
                            " - it asked to be left for {}s, which is longer than this waits",
                            wait.as_secs()
                        ));
                    }
                    return Err(refused(said, limit));
                }
                (wait, format!("answered {code}"))
            }
        };

        asking.say(format!(
            "{} {what}; trying again in {}s",
            asking.model,
            wait.as_secs()
        ));
        if !backed_off(wait, asking.deltas).await {
            return Ok(Sent::Interrupted);
        }
    }
}

/// Whether a status - or the code in an error object - is one that goes away by itself.
fn passing(code: u64) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// Reads a whole body, watching the wait the way a stream is watched; `None` if somebody asked to
/// stop first.
///
/// note: the headers arriving is not the end of the wait. An endpoint may send them at once and
/// the answer when it is written, which for a whole answer is minutes - and one that sends them
/// and then nothing is the stall [`WHOLE_ANSWER`] is for. Read in one `text()`, neither can be
/// interrupted, and the second holds the turn until the transport's own timeout, which the default
/// client does not have.
async fn body(
    mut response: reqwest::Response,
    asking: &Asking<'_>,
    patience: Duration,
) -> Result<Option<String>, BoxError> {
    let mut body = Vec::new();
    let mut vigil = Vigil::waiting(patience);

    loop {
        match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                vigil.heard();
                body.extend_from_slice(&bytes);
            }
            Ok(Ok(None)) => return Ok(Some(String::from_utf8_lossy(&body).into_owned())),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                if asking.deltas.is_interrupted() {
                    return Ok(None);
                }
                match vigil.waited() {
                    Silence::Enough => return Err(stalled(asking.model, patience)),
                    Silence::Worth(seconds) => asking.say(not_answered(asking.model, seconds)),
                    Silence::Ordinary => {}
                }
            }
        }
    }
}

/// The error for a response that started and then said nothing for as long as it was given.
///
/// note: not retried, unlike a request that never answered at all. The model may well have been
/// generating, and every attempt is billed.
pub(crate) fn stalled(model: &str, waited: Duration) -> BoxError {
    format!(
        "{model} answered and then said nothing for {}s; giving up",
        waited.as_secs()
    )
    .into()
}

/// Waits for a request to be answered, watching the wait the way the stream itself is watched.
///
/// note: `&mut sending` rather than `sending`. Handing `timeout` the future itself would drop it
/// 120ms later and cancel the request that had just been made; borrowing it stops polling for
/// that round and leaves the connection standing. The loop is the one the stream runs, so that the
/// wait before the first byte gets the same three things: an interrupt is heard, the silence is
/// said out loud, and it ends.
async fn watched(
    sending: impl Future<Output = reqwest::Result<reqwest::Response>>,
    asking: &Asking<'_>,
    patience: Duration,
) -> Result<reqwest::Response, Unsent> {
    let mut sending = std::pin::pin!(sending);
    let mut vigil = Vigil::waiting(patience);

    loop {
        if let Ok(sent) = tokio::time::timeout(HEARTBEAT, &mut sending).await {
            return sent.map_err(Unsent::Transport);
        }

        if asking.deltas.is_interrupted() {
            return Err(Unsent::Interrupted);
        }

        match vigil.waited() {
            Silence::Enough => return Err(Unsent::Silent(patience)),
            Silence::Worth(seconds) => asking.say(not_answered(asking.model, seconds)),
            Silence::Ordinary => {}
        }
    }
}

/// Waits out a backoff, and says whether it was let run to the end.
///
/// note: in heartbeats rather than one sleep, because a request somebody has asked to stop is not
/// one to send again. A single sleep keeps a stopped turn waiting for as long as the server
/// asked - a minute, for a `Retry-After: 60` - and then sends the request anyway, to be answered
/// and billed.
async fn backed_off(wait: Duration, deltas: &DeltaSink) -> bool {
    let until = Instant::now() + wait;
    loop {
        if deltas.is_interrupted() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        tokio::time::sleep(left.min(HEARTBEAT)).await;
    }
}

/// Whether a request that never got an answer is worth sending again.
///
/// note: a timeout only. Everything else a transport can fail with is either a decision - nothing
/// listening on that address, a name that does not resolve - or a bug in what was built, and
/// neither improves by being repeated. `is_timeout` walks the source chain, so a stall reported
/// as hyper's `Io(TimedOut)` several layers down still counts.
pub(crate) fn worth_waiting_out(e: &reqwest::Error) -> bool {
    e.is_timeout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_crypto;

    /// What the rule says about a silence of a given length, as a word.
    fn judged(vigil: &mut Vigil, seconds: u64) -> &'static str {
        match vigil.judge(Duration::from_secs(seconds)) {
            Silence::Worth(_) => "said",
            Silence::Enough => "gave up",
            Silence::Ordinary => "waited",
        }
    }

    #[test]
    fn a_stream_that_goes_quiet_is_mentioned_once_and_then_occasionally() {
        let mut vigil = Vigil::new();

        // a gap short enough to be a model thinking is not news
        assert_eq!(judged(&mut vigil, 0), "waited");
        assert_eq!(judged(&mut vigil, QUIET.as_secs() - 1), "waited");

        // the first one that is
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");
        // and not again a second later, or the report would bury the conversation it is about
        assert_eq!(judged(&mut vigil, QUIET.as_secs() + 1), "waited");
        assert_eq!(
            judged(&mut vigil, QUIET.as_secs() + AGAIN.as_secs() - 1),
            "waited"
        );
        assert_eq!(
            judged(&mut vigil, QUIET.as_secs() + AGAIN.as_secs()),
            "said"
        );

        // and eventually it stops waiting: `asking` for ever is indistinguishable from a model
        // that is still coming
        assert_eq!(judged(&mut vigil, PATIENCE.as_secs()), "gave up");
        assert_eq!(judged(&mut vigil, PATIENCE.as_secs() + 60), "gave up");
    }

    #[test]
    fn a_stream_that_starts_talking_again_starts_the_count_over() {
        let mut vigil = Vigil::new();
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");

        // it came back, and the quiet before it had been mentioned - so the next one is news
        assert!(vigil.heard(), "the silence was reported, so its end is too");
        assert!(!vigil.heard(), "an ordinary byte is not");
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");
    }

    /// A socket that accepts and then says nothing, so a request to it stalls the way a loaded
    /// upstream does rather than being refused.
    async fn black_hole() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        tokio::spawn(async move {
            // held open, never answered: accepting and dropping would be a reset, which is a
            // different thing entirely
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });

        at
    }

    /// An address on which nothing is listening, and which says so.
    ///
    /// note: bound and then dropped rather than a well-known port assumed to be free. A firewall
    /// that *drops* packets to a reserved port such as discard, `127.0.0.1:9`, instead of refusing
    /// them turns "nothing is listening" into a timeout, which is the one answer this half of the
    /// test needs it not to give. A port the OS has just handed out and taken back is refused
    /// rather than filtered.
    async fn nobody_home() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        drop(listener);

        at
    }

    #[tokio::test]
    async fn a_stalled_request_is_waited_out_and_a_refused_one_is_not() {
        // a 429 gets four tries and a doubling, and a connection that stalls has to get them too,
        // or a loaded upstream ends the session while the same model is answering other requests
        install_crypto();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .expect("a client");

        let stalled = client
            .get(format!("http://{}/", black_hole().await))
            .send()
            .await
            .expect_err("nothing ever answers there");
        assert!(
            worth_waiting_out(&stalled),
            "a stall is a busy server, and this is the error a busy one produces: {stalled:?}"
        );

        // note: its own client, with room to spare. A refusal comes back in microseconds, so the
        // only thing a long timeout changes is whether a loaded machine can turn one into a
        // timeout before it arrives - and a timeout is precisely the answer that would make this
        // assertion mean the opposite of what it says
        let patient = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("a client");
        let refused = patient
            .get(format!("http://{}/", nobody_home().await))
            .send()
            .await
            .expect_err("nothing is listening");
        assert!(
            !refused.is_timeout(),
            "this host neither answered nor refused, so there is nothing here to tell apart; \
             a firewall that drops rather than refuses will do this: {refused:?}"
        );
        assert!(
            !worth_waiting_out(&refused),
            "an address with nothing behind it is an answer, not a delay: {refused:?}"
        );

        // counting the silence ourselves has to mean what the transport's own timeout means,
        // because it is usually the thing that notices first
        assert!(
            Unsent::Silent(PATIENCE).worth_waiting_out(),
            "a server that took the request and said nothing is a busy one"
        );

        // and the one failure that must never be retried: resending a request somebody cancelled
        // spends their money on an answer they asked not to have
        assert!(
            !Unsent::Interrupted.worth_waiting_out(),
            "an interrupt is a decision, not a delay"
        );
    }

    /// note: the sentences a person actually reads when a model goes quiet. What it pins is the
    /// half that is a claim about *this* crate: that it names the mechanism it really watches
    /// rather than a key, which is a fact about the caller.
    #[test]
    fn a_silence_names_no_key() {
        for said in [not_answered("a-model", 40), gone_quiet("a-model", 40)] {
            assert!(said.contains("a-model"), "{said}");
            assert!(said.contains("40s"), "{said}");
            assert!(said.ends_with("an interrupt gives up on it"), "{said}");
            assert!(
                !said.contains("esc") && !said.contains("key"),
                "a library does not know what its caller's keyboard does: {said}"
            );
        }
        // and they are different news: nothing yet, against a stream that stopped halfway
        assert_ne!(not_answered("m", 40), gone_quiet("m", 40));
    }
}
