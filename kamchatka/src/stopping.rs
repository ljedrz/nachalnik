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
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}
