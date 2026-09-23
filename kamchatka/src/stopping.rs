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
pub struct Stopping(Inner);

#[cfg(unix)]
type Inner = tokio::signal::unix::Signal;
#[cfg(windows)]
type Inner = tokio::signal::windows::CtrlC;
/// Anywhere else, a fresh subscription per press, and the window with it.
///
/// note: neither of the two above exists off its own platform. What is left is a target nobody
/// ships this to, and refusing to compile there would be a worse answer than this one.
#[cfg(not(any(unix, windows)))]
type Inner = ();

impl Stopping {
    /// Subscribes.
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        let inner = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        #[cfg(windows)]
        let inner = tokio::signal::windows::ctrl_c()?;
        #[cfg(not(any(unix, windows)))]
        let inner = ();

        Ok(Self(inner))
    }

    /// Waits for the next one.
    ///
    /// note: cancel-safe, which is why this is a value rather than a call: the subscription is in
    /// `self` and survives a `select!` choosing another branch.
    pub async fn pressed(&mut self) {
        #[cfg(any(unix, windows))]
        {
            self.0.recv().await;
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Every request from outside to end this process that is not <kbd>ctrl+c</kbd>: `SIGTERM` and
/// `SIGHUP` on unix, and closing the console, logging off or shutting down on Windows.
///
/// note: what a closed terminal window, a dropped ssh connection, `timeout`, `systemctl stop` and
/// `docker stop` send. Left to its default each ends the process where it stands, with no
/// `session.finished` and no record written - and a `shell` command, in a process group of its
/// own, is not sent the terminal's hangup and keeps running after the agent that started it. So
/// each loop takes one of these as `/quit`: the running turn is stopped and waited for, and the
/// session ends the way a session ends.
///
/// note: one arrival is enough, unlike `ctrl+c`, because none of these is somebody at a keyboard
/// who might mean less than "leave". Windows gives a handler a few seconds before it ends the
/// process regardless, which is shorter than [`crate::app::LEAVING`] and is the platform's call.
pub struct Terminated(TerminatedInner);

#[cfg(unix)]
type TerminatedInner = (tokio::signal::unix::Signal, tokio::signal::unix::Signal);
#[cfg(windows)]
type TerminatedInner = (
    tokio::signal::windows::CtrlClose,
    tokio::signal::windows::CtrlLogoff,
    tokio::signal::windows::CtrlShutdown,
);
#[cfg(not(any(unix, windows)))]
type TerminatedInner = ();

impl Terminated {
    /// Subscribes.
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        let inner = {
            use tokio::signal::unix::{SignalKind, signal};
            (
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
            )
        };
        #[cfg(windows)]
        let inner = (
            tokio::signal::windows::ctrl_close()?,
            tokio::signal::windows::ctrl_logoff()?,
            tokio::signal::windows::ctrl_shutdown()?,
        );
        #[cfg(not(any(unix, windows)))]
        let inner = ();

        Ok(Self(inner))
    }

    /// Waits for the next one; cancel-safe, as [`Stopping::pressed`] is.
    pub async fn arrived(&mut self) {
        #[cfg(unix)]
        {
            let (terminate, hangup) = &mut self.0;
            tokio::select! {
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
        }
        #[cfg(windows)]
        {
            let (close, logoff, shutdown) = &mut self.0;
            tokio::select! {
                _ = close.recv() => {}
                _ = logoff.recv() => {}
                _ = shutdown.recv() => {}
            }
        }
        #[cfg(not(any(unix, windows)))]
        std::future::pending::<()>().await;
    }
}
