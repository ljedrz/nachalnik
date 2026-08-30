//! A terminal agent built on [`nachalnik`], and a demonstration of what that runtime is for.
//!
//! Four tabs, each of which gets the whole window, because each of them is a whole view.
//! **chat** is the conversation, and every other agent in the terminal has one. **context** is
//! the reason this exists: the context, item by item, with what each one costs, whether it is
//! going into the next request, and - for the ones that are not - why not, in the projector's own
//! words. Space takes an item out and puts it back; `p` pins it so that compaction cannot have
//! it; enter shows what it actually says. **trace** is every event the runtime emits, as it
//! happens, under the same names the session log is made of. **permissions** is every answer
//! somebody has given the policy, what each one covers, and a count of what is still a question -
//! changeable where it is read rather than one prompt at a time.
//!
//! Nothing is inferred and nothing is hidden: the context tab is a list of ordinary values the
//! runtime hands out, and `ctrl+p` prints the exact request they add up to.
//!
//! `--introspect`, or `/introspect` at any point, adds two more tools for reading and managing a
//! context from the inside. [`introspect`] lists what is being carried and what each item costs,
//! reports the budget against what the last request really cost, shows the request about to go
//! out, and answers on a throwaway fork so an answer can be read before it is given; `amend`
//! elides, excludes, pins and rewrites what is being carried, writes something down that
//! compaction cannot take, and walks its own changes back. Neither is allowed to touch what a
//! person pinned. Nothing in the runtime knows about any of this - it is what a tool can already
//! do with a context that is a list of public values and a request that can be built without
//! being sent.
//!
//! `--gemini` swaps the wire format for Google's own, in which an assistant turn is an ordered
//! list of parts rather than a content slot beside a list of calls. What that buys is the order
//! itself: thinking, a sentence, a tool call, more thinking - recorded as it happened, counted and
//! prunable like anything else, and sent back the same way. Both dialects answer one trait
//! ([`provider::Endpoint`]), so nothing above them knows which one it got.
//!
//! Everything in here is user code: the providers, the tools, the policy, the compactor and the
//! rendering. The kernel supplies the state machine, the context and the paper trail.
//!
//! It is a library only so that the screen can be tested - [`ui::draw`] against a
//! `TestBackend` is how the tests check that a pruned item really does leave the next request.
//! The program is `kamchatka`.

#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod app;
pub mod gemini;
pub mod introspect;
pub mod provider;
pub mod sandbox;
pub mod tools;
pub mod ui;
