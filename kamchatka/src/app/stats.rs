//! How long a response took and what it was made of, said after it lands when somebody asked for
//! it with `--response-stats`.
//!
//! note: measured here, by the clock of whatever is handed the events, rather than read off the
//! endpoint. Every endpoint answers with the same events, and only some say how fast they were,
//! each in a shape of its own. The cost is that the times include the network and the time the
//! event spent on its way here, which on a local server is noise and on a remote one is part of
//! what the person waited for anyway.

use std::time::{Duration, Instant};

use nachalnik::Usage;

use super::{App, Speaker, text::thousands};

/// When the request in flight went out, and when the first fragment of its answer came back.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    sent: Instant,
    first: Option<Instant>,
}

impl App {
    /// Starts timing a request that has just gone out.
    pub(super) fn time_request(&mut self) {
        self.timing = Some(Timing {
            sent: Instant::now(),
            first: None,
        });
    }

    /// Notes when the answer started arriving, the first time it does.
    pub(super) fn time_fragment(&mut self) {
        if let Some(timing) = &mut self.timing
            && timing.first.is_none()
        {
            timing.first = Some(Instant::now());
        }
    }

    /// Stops timing a request that will not be answered.
    pub(super) fn forget_timing(&mut self) {
        self.timing = None;
    }

    /// Says how the response that has just landed went, if that was asked for.
    ///
    /// note: timed whether or not it is said, so turning it on mid-session reports the next
    /// response rather than waiting for one that started after. A request whose start was never
    /// seen - its event lost to a lagging receiver - is not reported at all, since anything said
    /// about it would be measured from the wrong moment.
    pub(super) fn report_timing(&mut self, usage: Option<Usage>) {
        let Some(timing) = self.timing.take() else {
            return;
        };
        if !self.response_stats {
            return;
        }

        let now = Instant::now();
        let streamed = timing.first.map(|first| (first - timing.sent, now - first));
        self.say(Speaker::Note, stats(usage, now - timing.sent, streamed));
    }
}

/// One line about one response: how long it took, and the token counts the provider reported.
///
/// note: a speed only for a streamed answer, measured from its first fragment to its end. That is
/// the rate the model generated at. Over the whole response it would include the time spent
/// reading the request, which on a long context is most of it, and the figure would fall as the
/// context grew while nothing about the model changed. An answer that arrived in one piece has no
/// first fragment to measure from, so it gets no speed rather than a wrong one.
fn stats(usage: Option<Usage>, total: Duration, streamed: Option<(Duration, Duration)>) -> String {
    let mut said = format!("this response took {}", seconds(total));
    if let Some((first, _)) = streamed {
        said.push_str(&format!(", the first of it after {}", seconds(first)));
    }

    let Some(usage) = usage.filter(|it| it.input_tokens.is_some() || it.output_tokens.is_some())
    else {
        said.push_str("; the endpoint reported no token counts");
        return said;
    };
    if let Some(out) = usage.output_tokens {
        said.push_str(&format!("; {} out", thousands(out as usize)));
        if let Some(thinking) = usage.reasoning_tokens.filter(|it| *it > 0) {
            said.push_str(&format!(
                " ({} of it thinking)",
                thousands(thinking as usize)
            ));
        }
        // under a twentieth of a second, timer noise outweighs the measurement
        if let Some((_, generating)) = streamed.filter(|(_, it)| it.as_secs_f64() >= 0.05)
            && out > 0
        {
            said.push_str(&format!(
                " at {:.1} tokens a second",
                out as f64 / generating.as_secs_f64()
            ));
        }
    }
    if let Some(input) = usage.input_tokens {
        said.push_str(&format!("; {} in", thousands(input as usize)));
        if let Some(cached) = usage.cached_input_tokens.filter(|it| *it > 0) {
            said.push_str(&format!(
                ", {} of them from the cache",
                thousands(cached as usize)
            ));
        }
    }

    said
}

/// A duration to a tenth of a second, which is as finely as a person reads one.
fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        }
    }

    #[test]
    fn a_streamed_answer_gets_its_speed_from_the_first_fragment() {
        let said = stats(
            Some(Usage {
                cached_input_tokens: Some(10_890),
                reasoning_tokens: Some(3_900),
                ..usage(11_151, 4_289)
            }),
            Duration::from_millis(84_000),
            Some((Duration::from_millis(2_000), Duration::from_millis(82_000))),
        );

        assert_eq!(
            said,
            "this response took 84.0s, the first of it after 2.0s; 4,289 out (3,900 of it \
             thinking) at 52.3 tokens a second; 11,151 in, 10,890 of them from the cache"
        );
    }

    #[test]
    fn an_answer_in_one_piece_gets_no_speed() {
        let said = stats(Some(usage(500, 120)), Duration::from_millis(3_000), None);

        assert_eq!(said, "this response took 3.0s; 120 out; 500 in");
    }

    #[test]
    fn an_answer_with_no_counts_says_so() {
        let said = stats(
            Some(Usage::default()),
            Duration::from_millis(1_500),
            Some((Duration::from_millis(500), Duration::from_millis(1_000))),
        );

        assert_eq!(
            said,
            "this response took 1.5s, the first of it after 0.5s; the endpoint reported no \
             token counts"
        );
    }

    #[test]
    fn a_blink_of_generating_gets_no_speed() {
        let said = stats(
            Some(usage(500, 3)),
            Duration::from_millis(400),
            Some((Duration::from_millis(390), Duration::from_millis(10))),
        );

        assert!(!said.contains("a second"), "{said}");
    }
}
