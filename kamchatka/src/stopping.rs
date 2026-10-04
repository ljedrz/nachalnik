//! <kbd>ctrl+c</kbd>, subscribed once rather than once per turn round a loop.
//!
//! note: [`tokio::signal::ctrl_c`] is an `async fn`, so the subscription is made when the future
//! is first polled and dropped with the future. Its own documentation says as much - the future
//! completes on the first ctrl+c *after* the initial call to `poll` - and a `select!` that names
//! it directly builds a new one every time round the loop. Between one iteration finishing and the
//! next poll there is nothing subscribed, and a signal delivered in that window is not delivered
//! late, it is gone: the handler sets a flag, the driver broadcasts it to whoever is listening,
//! and a receiver made afterwards starts from the present.
//!
//! note: three loops in this program read a *second* press as "leave now", which is what makes the
//! window worth closing rather than noting. The press that lands in it is the second one, arriving
//! while the first is still being handled, and it is the one that means it.

use std::io;

/// Every <kbd>ctrl+c</kbd> this process is sent, from the moment this is made.
pub struct Stopping(tokio::signal::unix::Signal);

impl Stopping {
    /// Subscribes.
    pub fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self(signal(SignalKind::interrupt())?))
    }

    /// Waits for the next one.
    ///
    /// note: cancel-safe, which is why this is a value rather than a call: the subscription is in
    /// `self` and survives a `select!` choosing another branch.
    pub async fn pressed(&mut self) {
        self.0.recv().await;
    }
}

/// Every request from outside to end this process that is not <kbd>ctrl+c</kbd>: `SIGTERM` and
/// `SIGHUP`.
///
/// note: what a closed terminal window, a dropped ssh connection, `timeout`, `systemctl stop` and
/// `docker stop` send. Left to its default each ends the process where it stands, with no
/// `session.finished` and no record written - and a `shell` command, in a process group of its
/// own, is not sent the terminal's hangup and keeps running after the agent that started it. So
/// each loop takes one of these as `/quit`: the running turn is stopped and waited for, and the
/// session ends the way a session ends.
///
/// note: one arrival is enough, unlike `ctrl+c`, because neither is somebody at a keyboard who
/// might mean less than "leave".
pub struct Terminated {
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl Terminated {
    /// Subscribes.
    pub fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Waits for the next one; cancel-safe, as [`Stopping::pressed`] is.
    pub async fn arrived(&mut self) {
        self.which_arrived().await;
    }

    /// The same, saying which of the two it was.
    ///
    /// note: for the exit status, which is `128` plus the signal by convention - so a hangup and a
    /// `SIGTERM` leave with different ones, although both end the session the same way.
    pub async fn which_arrived(&mut self) -> Ending {
        tokio::select! {
            _ = self.terminate.recv() => Ending::Terminated,
            _ = self.hangup.recv() => Ending::HungUp,
        }
    }
}

/// Which request to end arrived; see [`Terminated::which_arrived`].
///
/// note: `#[non_exhaustive]`, which is what every public enum in this workspace carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ending {
    /// `SIGTERM`.
    Terminated,
    /// `SIGHUP`.
    HungUp,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// `arrived` waits for a signal, and is still waiting after one has been dropped.
    ///
    /// note: it is [`Terminated::which_arrived`] with the answer thrown away, and the one loop
    /// that waits on it decides whether a session is over - so a future that completed at once
    /// would end every drawn session the moment it drew. Both halves are here because either
    /// alone is half a claim: a future that never completed would pass a check that only watched
    /// it wait. The signal is sent to this process, because `Terminated::new` is what installs
    /// the handler and a subscription nothing sends anything to cannot be told from a wait.
    ///
    /// note: the first wait is dropped by its timeout, and the second still catches what was
    /// sent - which is what the cancel-safety the note on `pressed` claims, read rather than
    /// believed.
    #[tokio::test]
    async fn arrived_waits_for_a_signal_and_survives_the_wait_being_dropped() {
        for signal in ["TERM", "HUP"] {
            let mut terminations = Terminated::new().expect("the subscription");
            assert!(
                tokio::time::timeout(Duration::from_millis(200), terminations.arrived())
                    .await
                    .is_err(),
                "SIG{signal}: nothing was sent to this process and one arrived"
            );

            let sent = std::process::Command::new("kill")
                .args([&format!("-{signal}"), &std::process::id().to_string()])
                .status()
                .expect("`kill` is on the path");
            assert!(sent.success(), "SIG{signal} did not reach this process");

            tokio::time::timeout(Duration::from_secs(5), terminations.arrived())
                .await
                .unwrap_or_else(|_| panic!("SIG{signal} arrived and nothing said so"));
        }
    }
}
