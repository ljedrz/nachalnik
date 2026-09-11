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
//! `/attach PATH [TEXT]` puts a file in the context and asks about it in one go, and `-f` is the
//! same act at startup. Source and markdown go in as text; a PDF, an image or a recording goes in
//! as [`nachalnik::Content::Blob`], which nothing here can price - so the row, the chat line and
//! `/budget` all say a piece of the figure is missing rather than putting a `0` where a number
//! should be. It sends pictures and draws none: a terminal cell is not a pixel.
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
//! ([`nachalnik_providers::Endpoint`]), so nothing above them knows which one it got.
//!
//! Everything in here is user code: the tools, the policy, the compactor and the rendering, and
//! the providers next door in [`nachalnik_providers`]. The kernel supplies the state machine, the
//! context and the paper trail.
//!
//! It is a library because the screen has to be testable - `ui::draw` against a `TestBackend` is
//! how the tests check that a pruned item really does leave the next request - and because the
//! screen is not the program. [`app::App`] holds the session, and what the keys do to it is one
//! caller: `submit` takes the same line a person types, a command or a message, and `on_event`
//! takes what the kernel says back. The program is `kamchatka`.
//!
//! The `tui` feature, on by default, is the drawing and the keys: `ui`, the prompt, and the
//! bindings. Without it the library is the same program with nothing rendering it - and six
//! fewer dependencies - which is what a session driven by something other than a person at a
//! terminal needs.
//!
//! The way in is [`wiring::Setup`], which assembles a kernel, a policy, the tools, the sandbox
//! and an [`app::App`] around them in the order they have to go in, and hands back the two
//! receivers a loop needs. [`headless::Headless`] is one such loop and the program's own is the
//! other; a host with an event loop of its own wants neither, and [`app::App::submit`] is where
//! it hands in a line. [`config::Settings`] is the JSON the program's `--config-file` takes, for
//! a host that would rather read its defaults out of a file than hard-code them.
//!
//! What a session may spend is [`wiring::Setup::spend`], and it is enforced on the `App` rather
//! than in either loop - the accounting is in `on_event` and the refusal in `start_turn`, which
//! is how a host with a loop of its own gets the same ceiling as the program does.

#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod app;
pub mod attach;
pub mod config;
pub mod headless;
pub mod introspect;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod provider;
pub mod sandbox;
pub mod tools;
#[cfg(feature = "tui")]
pub mod ui;
pub mod wiring;

mod help;
