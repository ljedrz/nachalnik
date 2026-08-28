//! A terminal agent built on [`nachalnik`], and a demonstration of what that runtime is for.
//!
//! Three tabs, each of which gets the whole window, because each of them is a whole view.
//! **chat** is the conversation, and every other agent in the terminal has one. **context** is
//! the reason this exists: the context, item by item, with what each one costs, whether it is
//! going into the next request, and - for the ones that are not - why not, in the projector's own
//! words. Space takes an item out and puts it back; `p` pins it so that compaction cannot have
//! it; enter shows what it actually says. **trace** is every event the runtime emits, as it
//! happens, under the same names the session log is made of.
//!
//! Nothing is inferred and nothing is hidden: the context tab is a list of ordinary values the
//! runtime hands out, and `ctrl+p` prints the exact request they add up to.
//!
//! Everything in here is user code: the provider, the tools, the policy, the compactor and the
//! rendering. The kernel supplies the state machine, the context and the paper trail.
//!
//! It is a library only so that the screen can be tested - [`ui::draw`] against a
//! `TestBackend` is how the tests check that a pruned item really does leave the next request.
//! The program is `kamchatka`.

#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod app;
pub mod provider;
pub mod tools;
pub mod ui;
