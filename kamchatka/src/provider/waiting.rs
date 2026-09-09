//! Waiting on a server that is slow, silent or busy - and knowing which of the three it is.
//!
//! note: shared by both dialects rather than written twice, which is why it is `pub(crate)` and
//! why it names nothing OpenAI-specific. A stream that has gone quiet, a request that was refused
//! with a `Retry-After`, and one somebody pressed escape on are three different answers to "send
//! it again?", and getting them wrong costs either a turn or somebody's money.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use nachalnik::{BoxError, DeltaSink, ModelResponse, StopReason};
use parking_lot::Mutex;

/// How many times a request is retried when the server says it is busy.
pub(crate) const RETRIES: usize = 4;

/// The longest a server may ask to be left alone before this stops waiting and says so.
///
/// note: for the difference between a busy server and one that has said no until tomorrow. A
/// per-minute limit answers `Retry-After: 5`; a spent daily quota answers with the seconds until
/// midnight, and sitting through four doublings to discover that wastes the turn and the wait.
pub(super) const LINGER: Duration = Duration::from_secs(60);

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
pub(super) const HEARTBEAT: Duration = Duration::from_millis(120);

/// How long a stream may say nothing before the person watching is told about it.
pub(crate) const QUIET: Duration = Duration::from_secs(10);

/// How much longer it has to keep saying nothing before being mentioned again.
pub(crate) const AGAIN: Duration = Duration::from_secs(30);

/// How long it may say nothing before the request is given up on.
pub(crate) const PATIENCE: Duration = Duration::from_secs(150);

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
/// then goes quiet, which is exactly what an overloaded one does, left the status line reading
/// `asking` for ever with nothing to tell it apart from a model that was simply thinking hard.
/// This is the part that says so, and eventually the part that stops.
pub(crate) struct Vigil {
    /// When something last arrived.
    last: Instant,
    /// The silence already mentioned, in whole seconds; zero when there is nothing to mention.
    said: u64,
}

impl Vigil {
    /// Starts watching, now.
    pub(crate) fn new() -> Self {
        Self {
            last: Instant::now(),
            said: 0,
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
        if silent >= PATIENCE {
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
    /// Nothing came back at all, for [`PATIENCE`].
    Silent,
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
            Self::Silent => true,
            Self::Interrupted => false,
        }
    }

    /// What happened, as the middle of a sentence whose subject is the model.
    pub(crate) fn what_happened(&self) -> &'static str {
        match self {
            // worth telling apart: one of them hung up, the other never spoke
            Self::Transport(_) => "did not answer in time",
            Self::Silent => "has not answered at all",
            Self::Interrupted => "was interrupted",
        }
    }

    /// The error to end the turn with, once there is no patience left to spend.
    pub(crate) fn giving_up(self, model: &str) -> BoxError {
        match self {
            Self::Transport(e) => e.into(),
            Self::Silent => format!(
                "{model} never answered; giving up after {}s",
                PATIENCE.as_secs()
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

/// Waits for a request to be answered, watching the wait the way the stream itself is watched.
///
/// note: `&mut sending` rather than `sending`. Handing `timeout` the future itself would drop it
/// 120ms later and cancel the request that had just been made; borrowing it stops polling for
/// that round and leaves the connection standing. The loop is the one the stream runs, for the
/// same three reasons - `esc` is heard, the silence is said out loud, and it ends - and it is
/// here because everything before the first byte had none of them. A server that accepted the
/// connection and then went away held the terminal for eighteen minutes with `asking` on the
/// status line and no way to take it back.
///
/// note: free rather than a method, because [`Gemini`](crate::gemini::Gemini) sends its requests
/// down a different URL with a different header and needs exactly this in front of them.
pub(crate) async fn watched(
    sending: impl Future<Output = reqwest::Result<reqwest::Response>>,
    deltas: &DeltaSink,
    model: &str,
    notice: &Mutex<Option<String>>,
) -> Result<reqwest::Response, Unsent> {
    let mut sending = std::pin::pin!(sending);
    let mut vigil = Vigil::new();

    loop {
        if let Ok(sent) = tokio::time::timeout(HEARTBEAT, &mut sending).await {
            return sent.map_err(Unsent::Transport);
        }

        if deltas.is_interrupted() {
            return Err(Unsent::Interrupted);
        }

        match vigil.waited() {
            Silence::Enough => return Err(Unsent::Silent),
            Silence::Worth(seconds) => {
                *notice.lock() = Some(format!(
                    "{model} has not answered for {seconds}s; esc gives up on it"
                ));
            }
            Silence::Ordinary => {}
        }
    }
}

/// Whether a request that never got an answer is worth sending again.
///
/// note: a timeout only. Everything else a transport can fail with is either a decision - nothing
/// listening on that address, a name that does not resolve - or a bug in what was built, and
/// neither improves by being repeated. `is_timeout` walks the source chain, so a stall reported
/// as hyper's `Io(TimedOut)` several layers down still counts.
pub(super) fn worth_waiting_out(e: &reqwest::Error) -> bool {
    e.is_timeout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::install_crypto;

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

        // and eventually it stops waiting, which is the whole point: `asking` for ever was
        // indistinguishable from a model that was still coming
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
    /// note: bound and then dropped rather than a well-known port assumed to be free. This was
    /// `127.0.0.1:9` - discard - and it failed on a CI host whose firewall *drops* packets to a
    /// reserved port instead of refusing them, which turns "nothing is listening" into a timeout
    /// and so into the one answer this half of the test needs it not to give. A port the OS has
    /// just handed out and taken back is refused rather than filtered.
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
        // the failure this closes: a 429 got four tries and a doubling; a connection that stalled
        // got none, and took the session with it. Eleven of fourteen runs against one upstream
        // died this way while the same model answered a single request in six seconds
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
        // because it is now the thing that usually notices first
        assert!(
            Unsent::Silent.worth_waiting_out(),
            "a server that took the request and said nothing is a busy one"
        );

        // and the one failure that must never be retried: resending a request somebody cancelled
        // spends their money on an answer they asked not to have
        assert!(
            !Unsent::Interrupted.worth_waiting_out(),
            "esc is a decision, not a delay"
        );
    }
}
