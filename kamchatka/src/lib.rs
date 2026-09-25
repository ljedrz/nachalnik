//! A terminal agent for Linux, built on [`nachalnik`], and a demonstration of what that runtime is
//! for.
//!
//! Four tabs, each of which gets the whole window, because each of them is a whole view.
//! **chat** is the conversation, and every other agent in the terminal has one. **context** is
//! the reason this exists: the context, item by item, with what each one costs, whether it is
//! going into the next request, and - for the ones that are not - why not, in the projector's own
//! words. Space moves an item from active to elided to excluded and back; `p` pins it so that
//! compaction cannot have it; enter shows what it actually says. **trace** is every event the
//! runtime emits, as it happens, under the same names the session log is made of. **permissions**
//! is every answer somebody has given the policy, what each one covers, and a count of what is
//! still a question - changeable where it is read rather than one prompt at a time.
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
//! Four of the tools are about the session itself, and they are offered like the rest of them.
//!
//! - [`introspect::Context`] is the context. It lists what is being carried and what each item
//!   costs, reports the budget against what the last request really cost, shows the request about
//!   to go out, and finds text anywhere in it - the archive included, which nothing else can read
//!   without paying to carry it again. It also elides, excludes, pins, restores and rewrites what
//!   is being carried, writes down something compaction cannot take, and walks its own changes
//!   back.
//! - [`introspect::Fork`] stands up a throwaway copy of the session and asks it something, so an
//!   answer can be read before it is given, and a piece of context can be taken away to see what
//!   it was doing.
//! - [`introspect::Log`] reads the record kept beside the context: what was added, replaced,
//!   elided or compacted, and what was asked permission for and answered. It leads with what there
//!   is and what taking it would cost, so a short answer is never mistaken for a quiet session.
//! - [`introspect::Setup`] reads what the session is running *with*: which model, which tools and
//!   what each declares, what the policy will refuse before it is asked, what the compactor and
//!   the projector will do unasked, and whether this conversation was resumed from somebody
//!   else's.
//!
//! None of them is allowed to touch what a person pinned. Nothing in the runtime knows about any
//! of this - it is what a tool can already do with a context that is a list of public values and a
//! request that can be built without being sent. `/tools toggle ID` stops offering one of them, or
//! any other tool, and offers it again; the `tools` key in a settings file says which of them a
//! session starts with.
//!
//! `--gemini` swaps the wire format for Google's own, in which an assistant turn is an ordered
//! list of parts rather than a content slot beside a list of calls. What that buys is the order
//! itself: thinking, a sentence, a tool call, more thinking - recorded as it happened, counted,
//! elided and excluded like any other turn, and sent back the same way. Both dialects answer one
//! trait ([`nachalnik_providers::Endpoint`]), so nothing above them knows which one it got.
//!
//! Everything in here is user code: the tools, the policy, the compactor and the rendering, and
//! the providers next door in [`nachalnik_providers`]. The kernel supplies the state machine, the
//! context and the paper trail.
//!
//! It is a library because the screen has to be testable, and because the screen is not the
//! program. `ui::draw` against a `TestBackend` is how the tests check that an excluded item really
//! does leave the next request. [`app::App`] holds the session, and the keys are one caller of it:
//! `submit` takes the same line a person types, a command or a message, and `on_event` takes what
//! the kernel says back. The program is the `kamchatka` binary.
//!
//! The `tui` feature, on by default, is the drawing and the keys: `ui`, the prompt, and the
//! bindings. Without it the library is the same program with nothing rendering it, and without
//! the dependencies only the drawing wants - which is what a session driven by something other
//! than a person at a terminal needs.
//!
//! `--serve` puts a socket in front of the session and `--connect` attaches to one. The session
//! belongs to the program running it rather than to whoever is looking at it: a turn carries on
//! with nobody attached, a question waits for somebody to come back and answer it, and a client
//! picking it up an hour later picks up the same session. See [`remote`] for why the records are
//! the half that cannot be lost and the fragments are the half that can, and for why there is no
//! authentication in the protocol and none is planned.
//!
//! The way in is [`wiring::Setup`], which assembles a kernel, a policy, the tools, the sandbox
//! and an [`app::App`] around them in the order they have to go in, and hands back the two
//! receivers a loop needs. [`headless::Headless`] is one such loop, [`remote::Server`] is the
//! second and the program's own is the third. A host with an event loop of its own wants none of
//! them, and [`app::App::submit`] is where it hands in a line. [`config::Settings`] is the JSON
//! the program's `--config-file` takes, for a host that would rather read its defaults out of a
//! file than hard-code them.
//!
//! What a session may spend is [`wiring::Setup::spend`], and it is enforced on the `App` rather
//! than in either loop: the accounting is in `on_event` and the refusal in `start_turn`. That is
//! how a host with a loop of its own gets the same ceiling as the program does.

#![deny(missing_docs)]
#![deny(unsafe_code)]

// note: Linux only, on the two architectures the network gate has a filter for. What makes the
// `shell` tool something a model may be handed is Landlock, `openat2` beneath a directory and the
// gate, and every one of them is Linux's; elsewhere the shell ran unconfined behind a question
// read off the command's name. 0.15.1 is the last version that builds anywhere else, and is where
// to start from for a port.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!(
    "kamchatka builds for Linux on x86_64 and aarch64 only; 0.15.1 is the last version that \
     builds anywhere else"
);

/// A System One engine running on this machine; see `SYSTEM1_ADVISOR_COMMAND`.
#[cfg(feature = "advise")]
pub mod advisor;
pub mod app;
pub mod args;
pub mod attach;
pub mod clipboard;
pub mod config;
pub mod endpoint;
pub mod gate;
pub mod headless;
pub mod introspect;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod remote;
pub mod sandbox;
pub mod stopping;
pub mod tools;
#[cfg(feature = "tui")]
pub mod ui;
pub mod wiring;

pub mod help;
