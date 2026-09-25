//! The questions a running command is waiting on: it tried to open an internet socket, the gate
//! held the call, and somebody has to say whether it goes on.
//!
//! note: a queue of its own rather than a question the kernel raises, because the kernel asks
//! every question it has in `Deciding`, before a call runs, and this one arrives while the call is
//! running. The runtime has no state for "a tool is waiting on a person", and it should not grow
//! one for a tool that happens to spawn processes; what it does have is `policy.ruled`, which is
//! where the answer is written down. See [`crate::gate`] for what holds the call.
//!
//! note: held by [`Careful`](super::Careful), because the two things that need it already share the
//! policy: the shell asks through it, and the [`App`](crate::app::App) every loop drives answers
//! through it.

use std::sync::atomic::{AtomicU64, Ordering};

use nachalnik::ToolCallId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

/// A running command that reached for the network, waiting on an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Reached {
    /// Which question this is, for answering it.
    pub id: u64,
    /// The call whose command it is.
    pub call: ToolCallId,
    /// The command, as the model wrote it.
    pub cmd: String,
}

/// Every question a running command is waiting on, in the order they were asked.
pub struct Reaching {
    waiting: Mutex<Vec<(Reached, oneshot::Sender<bool>)>>,
    /// The next question's identifier.
    ///
    /// note: never reused while the process runs, which is as long as a question can be waiting.
    /// A question is not kept in a snapshot - a command does not survive a restart - so nothing
    /// from before one could be answered by mistake.
    next: AtomicU64,
    /// Moved on whenever a question arrives or goes, for the loops that have to wake for one.
    changed: watch::Sender<u64>,
}

impl Default for Reaching {
    fn default() -> Self {
        Self::new()
    }
}

impl Reaching {
    /// Nobody waiting.
    pub fn new() -> Self {
        Self {
            waiting: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
            changed: watch::Sender::new(0),
        }
    }

    /// Asks whether this call's command may reach the network, and waits for the answer; `None`
    /// where the question went away unanswered.
    ///
    /// note: the question goes when this future does, answered or not. The shell drops it when the
    /// call is over - stopped, or finished with something it started still waiting - and a panel
    /// asking about a command that has already ended is a question whose answer reaches nothing.
    pub async fn ask(&self, call: ToolCallId, cmd: String) -> Option<bool> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.waiting
            .lock()
            .push((Reached { id, call, cmd }, answer));
        self.moved();

        let _asked = Asked { reaching: self, id };
        answered.await.ok()
    }

    /// The questions waiting, oldest first.
    pub fn waiting(&self) -> Vec<Reached> {
        self.waiting
            .lock()
            .iter()
            .map(|(reached, _)| reached.clone())
            .collect()
    }

    /// The oldest question waiting, which is the one a panel shows.
    pub fn first(&self) -> Option<Reached> {
        self.waiting
            .lock()
            .first()
            .map(|(reached, _)| reached.clone())
    }

    /// Answers one, handing back what it was about.
    pub fn answer(&self, id: u64, allow: bool) -> Result<Reached, String> {
        let taken = {
            let mut waiting = self.waiting.lock();
            waiting
                .iter()
                .position(|(reached, _)| reached.id == id)
                .map(|at| waiting.remove(at))
        };
        let Some((reached, answer)) = taken else {
            return Err(format!(
                "there is no command waiting on question {id} - it may have finished or been \
                 stopped"
            ));
        };
        // the command may have ended in the moment between the look and the answer, and then
        // there is nobody to tell
        let _ = answer.send(allow);
        self.moved();

        Ok(reached)
    }

    /// Something to wait on for the next time a question arrives or goes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn moved(&self) {
        self.changed.send_modify(|seen| *seen += 1);
    }

    fn withdraw(&self, id: u64) {
        let removed = {
            let mut waiting = self.waiting.lock();
            let before = waiting.len();
            waiting.retain(|(reached, _)| reached.id != id);
            before != waiting.len()
        };
        if removed {
            self.moved();
        }
    }
}

/// Takes a question away when the command asking it stops waiting, answered or not.
struct Asked<'a> {
    reaching: &'a Reaching,
    id: u64,
}

impl Drop for Asked<'_> {
    fn drop(&mut self) {
        self.reaching.withdraw(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A question answered reaches the command that asked, and one it stopped waiting on goes.
    #[tokio::test]
    async fn an_answer_reaches_the_asker_and_an_abandoned_question_goes() {
        let reaching = std::sync::Arc::new(Reaching::new());
        let mut woken = reaching.subscribe();

        let asking = tokio::spawn({
            let reaching = reaching.clone();
            async move { reaching.ask(ToolCallId::from("c1"), "curl x".into()).await }
        });
        woken.changed().await.expect("it said a question arrived");
        let asked = reaching.first().expect("it is waiting");
        assert_eq!(asked.cmd, "curl x");

        reaching
            .answer(asked.id, true)
            .expect("it is there to answer");
        assert_eq!(asking.await.expect("it ran"), Some(true));
        assert!(reaching.waiting().is_empty());
        assert!(reaching.answer(asked.id, true).is_err(), "answered once");

        // and one whose command stopped waiting is no longer on offer
        let abandoned = tokio::spawn({
            let reaching = reaching.clone();
            async move { reaching.ask(ToolCallId::from("c2"), "nc".into()).await }
        });
        while reaching.first().is_none() {
            tokio::task::yield_now().await;
        }
        abandoned.abort();
        let _ = abandoned.await;
        assert!(reaching.waiting().is_empty(), "{:?}", reaching.waiting());
    }
}
