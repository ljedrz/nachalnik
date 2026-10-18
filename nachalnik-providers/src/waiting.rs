//! Waiting on a slow, silent or busy server, and telling which it is.
//!
//! note: shared by both dialects, so it is `pub(crate)` and names nothing dialect-specific. A quiet
//! stream, a `Retry-After` refusal, and an escape are three different answers to "send it again?",
//! and getting them wrong costs a turn or somebody's money.

use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

// tokio's `Instant`, not `std`'s, so silences and waits use the same clock, which a test can pause
use tokio::time::Instant;

use nachalnik::{BoxError, DeltaSink, ModelResponse, StopReason};
use parking_lot::Mutex;
use serde_json::Value;

use crate::{
    out_of_quota,
    reading::{BrokeOff, Events, Read, complaint, compressed, failure, not_json, read},
    refused,
};

/// The longest a server may ask to be left alone before this stops waiting and says so.
///
/// note: for the difference between a busy server and one that has said no until tomorrow. A
/// per-minute limit answers `Retry-After: 5`; a spent daily quota answers with the seconds until
/// midnight, and sitting through three doublings to discover that wastes the turn and the wait.
pub(crate) const LINGER: Duration = Duration::from_secs(60);

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
pub(crate) const HEARTBEAT: Duration = Duration::from_millis(120);

/// How long a stream may say nothing before the person watching is told about it.
pub(crate) const QUIET: Duration = Duration::from_secs(10);

/// How much longer it has to keep saying nothing before being mentioned again.
pub(crate) const AGAIN: Duration = Duration::from_secs(30);

/// How long a *stream* may say nothing before the request is given up on, unless the caller says
/// otherwise; see [`Dialect::set_patience`](crate::Dialect::set_patience).
pub(crate) const PATIENCE: Duration = Duration::from_secs(150);

/// The same, for an answer that is not streamed at all.
///
/// note: ten minutes rather than the two and a half above, because nothing arrives until the whole
/// answer does and there is no fragment to reset the watch. A model may be silent for as long as it
/// takes to write a long answer; 150s of silence in a stream is a stall.
pub(crate) const WHOLE_ANSWER: Duration = Duration::from_secs(600);

/// The most of one response this reads before it stops: a line of a stream with no end to it, or a
/// whole body.
///
/// note: a bound because nothing else is one. The silence watch is reset by every byte, so a body
/// that trickles without ever ending a line is read for as long as it trickles, and all of it is
/// held. Far past anything a model answers with, and near nothing to a machine.
pub(crate) const LARGEST: usize = 64 << 20;

/// The error for a response past [`LARGEST`].
pub(crate) fn too_large(model: &str) -> BoxError {
    format!(
        "{model} sent more than {} MiB without finishing what it was sending; this reads no further",
        LARGEST >> 20
    )
    .into()
}

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
/// note: [`HEARTBEAT`] only makes a stalled request interruptible: it wakes, checks for escape, and
/// waits again. A server that answers and then goes quiet - exactly what an overloaded one does -
/// would leave the status line reading `asking` for ever, indistinguishable from a model thinking
/// hard. This part says so, and eventually stops.
pub(crate) struct Vigil {
    /// When something last arrived.
    last: Instant,
    /// The silence already mentioned, in whole seconds; zero when there is nothing to mention.
    said: u64,
    /// How long a silence may run before this gives up on it.
    patience: Duration,
}

impl Vigil {
    /// Starts watching, now, giving up on a silence of `patience`: the caller's for a stream, and
    /// [`WHOLE_ANSWER`] for an answer that is not streamed.
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

    /// Judges a silence of a given length.
    ///
    /// note: split out so the rule can be tested without a socket and a wall clock. `waited` holds
    /// the clock reading, which cannot go wrong.
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
    /// Nothing came back at all: [`PATIENCE`] for a stream, [`WHOLE_ANSWER`] when the answer was
    /// never going to arrive in pieces.
    Silent(Duration),
    /// Somebody pressed escape before the answer had started.
    Interrupted,
}

impl Unsent {
    /// Whether sending it again is worth anything.
    pub(crate) fn worth_waiting_out(&self) -> bool {
        match self {
            Self::Transport(e) => worth_waiting_out(e),
            // It means the same as the transport's own timeout, counted rather than reported. A
            // server that took the connection and went quiet is busy, and may be busy with this
            // request. `may_have_been_heard` reports it
            Self::Silent(_) => true,
            Self::Interrupted => false,
        }
    }

    /// Whether the server may have started on it, so resending it could pay for it twice.
    ///
    /// note: only a connection that was never made is certain not to have reached anybody. Silence
    /// or a timeout after connecting means the server may be working on the request.
    fn may_have_been_heard(&self) -> bool {
        match self {
            Self::Transport(e) => !e.is_connect(),
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

    /// The error to end the turn with, once there is no patience left to spend, `tried` times.
    ///
    /// note: the count is said because the wait is each try's. Four tries of a stream that never
    /// started are ten minutes, and a sentence naming one try's two and a half reads as a clock
    /// that is wrong.
    pub(crate) fn giving_up(self, model: &str, tried: usize) -> BoxError {
        match self {
            Self::Transport(e) => crate::with_causes(&e).into(),
            Self::Silent(waited) if tried > 1 => format!(
                "{model} never answered, asked {tried} times and given {}s each; giving up",
                waited.as_secs()
            )
            .into(),
            Self::Silent(waited) => format!(
                "{model} never answered; giving up after {}s",
                waited.as_secs()
            )
            .into(),
            Self::Interrupted => "interrupted".into(),
        }
    }
}

/// The answer to a request stopped before it had an answer.
///
/// note: not an error. The user asked for this, so a red line reads as a bug rather than an answer.
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

/// What every silence notice ends with.
///
/// note: a constant because it must not drift and is the crate's to say. It takes a [`DeltaSink`]
/// and asks whether it has been interrupted; how a caller sets it is not its business. It names no
/// key: a key suits one client only, and headless output has no keyboard to press.
const GIVES_UP: &str = "an interrupt gives up on it";

/// A model that has not said anything at all yet.
pub(crate) fn not_answered(model: &str, seconds: u64) -> String {
    format!("{model} has not answered for {seconds}s; {GIVES_UP}")
}

/// A model that started answering and then stopped.
///
/// note: distinct from [`not_answered`] because they are different news. Nothing yet means the
/// server may never have received the request; a stream that stops halfway took it and is in
/// trouble. A turn that uses a tool makes two requests, so the second is what "it hangs whenever it
/// uses a tool" really looks like.
pub(crate) fn gone_quiet(model: &str, seconds: u64) -> String {
    format!("{model} has said nothing for {seconds}s; {GIVES_UP}")
}

/// The model being waited on, and where to report the wait's progress.
///
/// note: the model is read once per request and used for every line about it; a name that changed
/// halfway would make one wait look like two.
pub(crate) struct Asking<'a> {
    /// The model being asked, as every notice about this request names it.
    pub(crate) model: &'a str,
    /// Where the answer goes as it arrives, and where an interrupt is asked about.
    pub(crate) deltas: &'a DeltaSink,
    /// Where a notice goes, for [`Endpoint::take_notice`](crate::Endpoint::take_notice).
    pub(crate) notice: &'a Mutex<Option<String>>,
    /// How many times the request may be sent while the server is busy, the first included; see
    /// [`Dialect::set_tries`](crate::Dialect::set_tries).
    pub(crate) tries: usize,
    /// How long a stream may say nothing before it is given up on; see
    /// [`Dialect::set_patience`](crate::Dialect::set_patience).
    pub(crate) patience: Duration,
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
    /// A stream, read to its end - never [`Read::Refused`], which is waited out or returned as
    /// the error it is.
    Streamed(Read),
}

/// Why an attempt is worth making again, if the retry rules agree.
enum Busy {
    /// Nothing came back, in a way that a busy server produces.
    Unsent(Unsent),
    /// The answer began and broke before a byte of it arrived.
    Broke(BoxError),
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

/// Sends a request, retrying while the server is busy, and returns what came of it.
///
/// note: retrying is the provider's business. A free tier often answers busy, so not retrying looks
/// broken when it is not. The kernel must not silently send a request twice behind a caller's back.
/// The count is per request; its limit is [`Asking::tries`]; see [`RETRIES`](crate::RETRIES).
///
/// note: `request` builds a fresh request for each attempt because a sent one is spent. It covers
/// what dialects do differently here: a path, a header, a body.
///
/// note: a whole answer is read here, not by the caller: the OpenAI dialect's other way of saying
/// 429 is an `error` object inside a 200, and a limit reported that way is worth waiting out like a
/// status. A stream is read here for the same reason: its error is the first event, and `events` is
/// untouched by an attempt that ended that way.
pub(crate) async fn sent(
    asking: &Asking<'_>,
    attempts: &AtomicUsize,
    limit: Option<usize>,
    streaming: bool,
    request: impl Fn() -> reqwest::RequestBuilder,
    events: &mut impl Events,
) -> Result<Sent, BoxError> {
    // nothing arrives until the whole answer does, when it is not a stream, so the silence that
    // means "this has stalled" is a much longer one
    let patience = match streaming {
        true => asking.patience,
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
            // behind it, and making a typo take three doublings to report helps nobody
            //
            // note: and an answer that has not started is not this either, unless it never
            // reached the server. A whole answer's headers come with its last token, so none yet
            // means the model is still writing it; and a stream's may come only with its first,
            // as ollama's do while it loads the model. Sending either again would pay for it
            // again, four times over, and fail anyway once the last attempt ran as long as the
            // first - so what is sent again is a request that never made its connection
            Err(reason) if reason.worth_waiting_out() && !reason.may_have_been_heard() => {
                Busy::Unsent(reason)
            }
            Err(reason) => return Err(reason.giving_up(asking.model, tried)),
            Ok(mut response) if streaming && response.status().is_success() => {
                match read(&mut response, asking, events).await {
                    Ok(Read::Refused { code, said }) => Busy::Refused {
                        code,
                        transient: passing(code) && !out_of_quota(&said),
                        said,
                        asked: None,
                    },
                    Ok(other) => return Ok(Sent::Streamed(other)),
                    Err(broke) if broke.is::<BrokeOff>() => Busy::Broke(broke),
                    Err(e) => return Err(e),
                }
            }
            Ok(response) => {
                let status = response.status();
                // read before the body takes the response, for the one refusal that is about the
                // address rather than the request: see below
                let url = response.url().clone();
                // the server's own answer to "when?", where it gives one. Guessing at a doubling
                // is for a server that did not say
                let asked = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| retry_after(value, std::time::SystemTime::now()));
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
                        // note: through `not_json`, which is the reading the streaming path gives
                        // a body that is not JSON - see there for why the words and not the
                        // markup
                        let payload: Value =
                            serde_json::from_str(&body).map_err(|e| not_json(&e, &body))?;
                        let Some(error) = payload.get("error").filter(|error| !error.is_null())
                        else {
                            return Ok(Sent::Whole(payload));
                        };
                        // read as a stream's would be: a number, a number in a string, or a name
                        let code = crate::reading::code_of(error);
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
                        // note: a 404 names where it was asked because the server's own "Not Found"
                        // does not say which address - for example a base URL missing `/v1`, or one
                        // that is a web site
                        said: match status == reqwest::StatusCode::NOT_FOUND {
                            true => format!("{} - asked at {url}", complaint(status, &body)),
                            false => complaint(status, &body),
                        },
                        transient: passing(status.as_u16().into()) && !out_of_quota(&body),
                        asked,
                    },
                }
            }
        };

        // note: `doubling` is held to `LINGER`, which only a caller asking for more than `RETRIES`
        // tries reaches. A longer doubling would be read as the server asking to wait longer than
        // this waits, ending the tries at the sixth
        let doubling = Duration::from_secs(1 << tried.min(6)).min(LINGER);
        let (wait, what) = match busy {
            Busy::Unsent(reason) if tried >= asking.tries => {
                return Err(reason.giving_up(asking.model, tried));
            }
            Busy::Unsent(reason) => (doubling, reason.what_happened().to_owned()),
            Busy::Broke(broke) if tried >= asking.tries => return Err(broke),
            Busy::Broke(broke) => (
                doubling,
                format!("broke off before any of its answer arrived ({broke})"),
            ),
            Busy::Refused {
                code,
                said,
                transient,
                asked,
            } => {
                let wait = asked.unwrap_or(doubling);
                if !transient || tried >= asking.tries || wait > LINGER {
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

/// Returns how long a `Retry-After` value asks to be left: a number of seconds, or a date to wait
/// until.
///
/// note: dates are parsed in the one shape HTTP sends, `Sun, 06 Nov 1994 08:49:37 GMT`, not the two
/// obsolete shapes a recipient is also asked to read. Parsing seconds alone made a date fall
/// through to the doubling used when a server says nothing, so the request was sent again before
/// the named time. A past date means no wait. The caller caps a date too far off at [`LINGER`], and
/// caps a seconds value the same way.
///
/// note: `now` is a parameter so dates can be checked against a fixed clock.
fn retry_after(value: &str, now: std::time::SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    // `Sun, 06 Nov 1994 08:49:37 GMT`: a weekday nobody needs, then the date and the time
    let (_, date) = value.split_once(", ")?;
    let mut parts = date.split(' ');
    let (day, month, year, clock, zone) = (
        parts.next()?.parse::<u32>().ok()?,
        parts.next()?,
        parts.next()?.parse::<i64>().ok()?,
        parts.next()?,
        parts.next()?,
    );
    if zone != "GMT" || parts.next().is_some() {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| *name == month)? as u32
        + 1;
    let mut clock = clock.split(':').map(|part| part.parse::<u32>().ok());
    let (hours, minutes, seconds) = (clock.next()??, clock.next()??, clock.next()??);
    // note: the year bounded with the rest, as the four digits an HTTP date has. Unbounded, the
    // arithmetic below overflowed on one: a panic in a debug build, and in a release build a wait
    // of nothing - asked again at once, of a server that had asked to be left alone
    if clock.next().is_some()
        || !(1..=9999).contains(&year)
        || !(1..=31).contains(&day)
        || hours > 23
        || minutes > 59
        || seconds > 60
    {
        return None;
    }

    // days since 1970-01-01 for a proleptic Gregorian date, using Howard Hinnant's
    // `days_from_civil`. The year starts in March, so the leap day is last
    let (y, m) = match month <= 2 {
        true => (year - 1, month + 9),
        false => (year, month - 3),
    };
    let era = y.div_euclid(400);
    let of_era = y.rem_euclid(400);
    let of_year = (153 * i64::from(m) + 2) / 5 + i64::from(day) - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;

    let at = days * 86_400 + i64::from(hours * 3600 + minutes * 60 + seconds);
    let now = now.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let now = i64::try_from(now).ok()?;

    Some(Duration::from_secs(u64::try_from(at - now).unwrap_or(0)))
}

/// Returns whether a status code, or the code in an error object, goes away by itself.
///
/// note: not every 500 code qualifies. `501 Not Implemented` and `505 HTTP Version Not Supported`
/// mean the server does not support the request; a plain file server answers a `POST` with `501`.
/// Retrying them only spends the doublings before the same answer.
fn passing(code: u64) -> bool {
    code == 429 || ((500..600).contains(&code) && !matches!(code, 501 | 505))
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

    // note: the same refusal the streaming path makes, and here it covers the two bodies nothing
    // else does: a whole answer and the body of a refusal. Both are read as text below, and the
    // bytes of a compressed one are not text
    if let Some(encoded) = compressed(asking.model, response.headers()) {
        return Err(encoded);
    }

    loop {
        match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                vigil.heard();
                if body.len() + bytes.len() > LARGEST {
                    return Err(too_large(asking.model));
                }
                body.extend_from_slice(&bytes);
                // checked on every chunk as well as in the quiet: a body that never goes quiet
                // would otherwise never be asked
                if asking.deltas.is_interrupted() {
                    return Ok(None);
                }
            }
            Ok(Ok(None)) => return Ok(Some(String::from_utf8_lossy(&body).into_owned())),
            Ok(Err(e)) => return Err(crate::with_causes(&e).into()),
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
/// note: take `&mut sending`, not `sending`. Handing the future itself to `timeout` would drop it
/// after 120ms and cancel the request that had just been made. Borrowing it stops polling for that
/// round and leaves the connection standing. The loop is the one the stream runs, so the wait
/// before the first byte gets the same three things: it hears an interrupt, reports silence, and
/// ends.
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

/// Waits out a backoff and returns whether it ran to the end.
///
/// note: wait in heartbeats rather than one sleep, because a request that has been asked to stop
/// must not be sent again. A single sleep keeps a stopped turn waiting for as long as the server
/// asked - a minute for `Retry-After: 60` - and then sends the request anyway, so it is answered
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

    /// A `Retry-After` is a number of seconds or a date, and a date is the wait until it.
    ///
    /// note: the dates' own instants were worked out apart from this parser, by Python's
    /// `email.utils`, and include a leap day and the turn of a century that is not a leap year -
    /// the two places a calendar written by hand goes wrong.
    #[test]
    fn a_retry_after_is_read_as_seconds_or_as_a_date() {
        let at = |seconds: u64| std::time::UNIX_EPOCH + Duration::from_secs(seconds);

        assert_eq!(retry_after("120", at(0)), Some(Duration::from_secs(120)));
        assert_eq!(retry_after(" 7 ", at(0)), Some(Duration::from_secs(7)));

        for (date, instant) in [
            ("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777),
            ("Thu, 29 Feb 2024 00:00:00 GMT", 1_709_164_800),
            ("Wed, 31 Dec 2025 23:59:59 GMT", 1_767_225_599),
            ("Mon, 01 Mar 2100 12:00:00 GMT", 4_107_585_600),
            // a leap second, which an HTTP date may name
            ("Wed, 31 Dec 2025 23:59:60 GMT", 1_767_225_600),
        ] {
            assert_eq!(
                retry_after(date, at(instant - 30)),
                Some(Duration::from_secs(30)),
                "{date}"
            );
            // and one that has already gone by asks for no wait at all
            assert_eq!(
                retry_after(date, at(instant + 30)),
                Some(Duration::ZERO),
                "{date}"
            );
        }

        // what is neither is no answer, and the doubling a server that said nothing gets
        for neither in [
            "",
            "soon",
            "-5",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nob 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 25:49:37 GMT",
            "Sun, 06 Nov 1994 08:60:37 GMT",
            "Sun, 06 Nov 1994 08:49:61 GMT",
            // a year no date has, which the arithmetic below overflowed on: a panic, or a wait of
            // nothing asked for again at once
            "Mon, 01 Jan -9223372036854775808 08:49:37 GMT",
            "Mon, 01 Jan 1000000000000 08:49:37 GMT",
        ] {
            assert_eq!(retry_after(neither, at(0)), None, "{neither:?}");
        }
    }

    #[test]
    fn a_stream_that_goes_quiet_is_mentioned_once_and_then_occasionally() {
        let mut vigil = Vigil::waiting(PATIENCE);

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
        let mut vigil = Vigil::waiting(PATIENCE);
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");

        // it came back, and the quiet before it had been mentioned - so the next one is news
        assert!(vigil.heard(), "the silence was reported, so its end is too");
        assert!(!vigil.heard(), "an ordinary byte is not");
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");
    }

    /// A socket that accepts and then says nothing, so a request to it stalls like a loaded
    /// upstream instead of being refused.
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

    /// An address where nothing is listening, which refuses connections.
    ///
    /// note: bound and then dropped instead of assuming a well-known port is free. A firewall that
    /// drops packets to a reserved port such as `127.0.0.1:9` instead of refusing them turns
    /// "nothing is listening" into a timeout, which this test must not get. A port the OS just
    /// handed out and took back is refused rather than filtered.
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

        // note: its own client, with time to spare. A refusal arrives in microseconds, so a long
        // timeout only changes whether a loaded machine turns it into a timeout first - and that
        // would invert this assertion
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

        // counting the silence ourselves must mean what the transport's own timeout means, because
        // the transport usually notices first
        assert!(
            Unsent::Silent(PATIENCE).worth_waiting_out(),
            "a server that took the request and said nothing is a busy one"
        );

        // Never retry a cancelled request: resending a request somebody cancelled spends their
        // money on an answer they asked not to have
        assert!(
            !Unsent::Interrupted.worth_waiting_out(),
            "an interrupt is a decision, not a delay"
        );

        // The notice before a retry says which it waited out: a transport that gave up, or a server
        // that took the request and never replied
        let hung_up = Unsent::Transport(stalled).what_happened();
        let never_spoke = Unsent::Silent(PATIENCE).what_happened();
        assert!(!hung_up.is_empty() && !never_spoke.is_empty());
        assert_ne!(hung_up, never_spoke);
    }

    /// The error for a request nobody answered says how many tries it was given, and only once
    /// there has been more than one.
    ///
    /// note: the count is in the sentence because the wait is each try's - four tries of a stream
    /// that never started are ten minutes, and a sentence naming one try's two and a half reads
    /// as a clock that is wrong. A first try has no count worth naming: there was one.
    #[test]
    fn giving_up_says_how_many_tries_there_were() {
        let once = Unsent::Silent(PATIENCE).giving_up("a-model", 1).to_string();
        assert!(
            once.contains("a-model") && once.contains(&format!("{}s", PATIENCE.as_secs())),
            "{once}"
        );
        assert!(
            !once.contains("asked") && !once.contains("times"),
            "one try is not a number of tries: {once}"
        );

        // and once there were several, the sentence says how many and what each of them waited
        for tried in 2..=crate::RETRIES {
            let said = Unsent::Silent(PATIENCE)
                .giving_up("a-model", tried)
                .to_string();
            assert!(
                said.contains(&format!("asked {tried} times")),
                "the count is what says the wait was more than one try's: {said}"
            );
            assert!(
                said.contains(&format!("given {}s each", PATIENCE.as_secs())),
                "each try waited the whole patience, so each try's wait is the number: {said}"
            );
            assert!(said.contains("a-model"), "{said}");
        }

        // an interrupt and a transport both say what they are rather than counting tries
        assert_eq!(
            Unsent::Interrupted.giving_up("a-model", 4).to_string(),
            "interrupted"
        );
    }

    /// A server that is busy or failing is asked again, and one saying what it does not do is not.
    #[test]
    fn a_server_that_does_not_do_this_is_not_asked_again() {
        for busy in [429, 500, 502, 503, 504, 529] {
            assert!(passing(busy), "{busy}");
        }
        for definite in [400, 401, 404, 501, 505] {
            assert!(!passing(definite), "{definite}");
        }
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
