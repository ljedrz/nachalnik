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
/// Anywhere else, the old behaviour: a fresh subscription per press, and the window with it.
///
/// note: neither of the two above exists off its own platform, and this crate is built for three
/// operating systems. What is left is a target nobody ships this to, and refusing to compile there
/// would be a worse answer than the one it has today.
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
    /// note: cancel-safe, which is the whole reason this is a value rather than a call: the
    /// subscription is in `self` and survives a `select!` choosing another branch.
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
