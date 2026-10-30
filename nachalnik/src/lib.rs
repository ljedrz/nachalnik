#![deny(missing_docs)]
#![deny(unsafe_code)]

//! **nachalnik** is a small agent runtime: an execution loop around a language model in which the
//! context, the tools, the permissions and the requests are explicit, inspectable state rather than
//! hidden behaviour.
//!
//! # The loop is a state machine
//!
//! ```text
//!   Idle ── step ──> Requesting ──(no tool calls)──> Finished
//!   Ready                │
//!     ▲                  ├──(calls, all decided)──> Ready ── step ──> Executing ──> Idle
//!     │                  │
//!     └── decide ── Deciding <──(calls, one to ask about)
//! ```
//!
//! [`Kernel::step`] performs one of those transitions and returns the [`State`] it produced;
//! [`Kernel::turn`] repeats until the model ends its turn or somebody has to decide something.
//! `Finished` carries the model's own [`StopReason`], since running out of output tokens is not
//! ending a turn. In [`State::Requesting`] and [`State::Executing`] the loop is already being
//! driven, and a second [`Kernel::step`] is [`Error::Busy`], not a second request.
//!
//! Every other state is a resting state, and whatever you change while the kernel rests is what the
//! next request contains. In [`State::Ready`] the model has said which tools it wants and nothing
//! has run yet, so you can look first.
//!
//! [`Kernel::interrupt`] can be called from anywhere and stops the loop before the next transition.
//! Stopping what is already in flight is cooperative, because the kernel owns neither the socket
//! nor the future: a [`Provider`] that checks [`DeltaSink::is_interrupted`] and a [`Tool`] that
//! checks [`OutputSink::is_interrupted`] can return what they have, which is recorded like any
//! other turn.
//!
//! # What it does not do
//!
//! No system prompt, instructions or personality; no default tools, filesystem access, process
//! spawning or HTTP client; no subagents, MCP or background activity; no `/context` renderer or
//! permission table; and no automatic context management unless you install a [`Compactor`], which
//! reports everything it did.
//!
//! # What it does not protect you from
//!
//! There is no sandbox in this crate, and there will not be one. The kernel executes nothing: every
//! side effect happens inside a [`Tool`] you wrote and registered, so containment (a jail, a
//! namespace, `seccomp`, a container) goes inside your tool or around the whole process.
//!
//! It enforces one thing: a call the [`PermissionPolicy`] refused is never handed to
//! [`Tool::invoke`], and the refusal is recorded as an [`Event`] and as a tool result the model
//! sees. That is a decision point with a record, not a boundary:
//!
//! - A [`Capability`] is a tool's own declaration, not a verified property; the kernel has nothing
//!   to check it against.
//! - `exec:run` subsumes every other capability, so a policy that allows it has allowed all of
//!   them, whatever it answers about the rest.
//! - A policy that reads a command's text is a heuristic: it can refuse what was written, not a
//!   program that reaches the network another way. Confinement that can stop that belongs where the
//!   process is spawned; `kamchatka` runs its `shell` tool under Landlock and a seccomp filter that
//!   holds every attempt to reach the network, so `network: deny` is enforced by the operating
//!   system rather than judged by a command's name.
//! - Anything in the context is read by the model and can carry instructions. Against that, this
//!   runtime offers the policy, which nothing in a model's output reaches except as a tool name and
//!   arguments, and a context you can inspect before the request goes.
//!
//! # The parts are yours
//!
//! The kernel does not own a UI, an editor, a model or a tool; those are your side of the
//! interface:
//!
//! | trait | you provide | the kernel provides |
//! | --- | --- | --- |
//! | [`Provider`] | a model, however you reach it | the request, verbatim |
//! | [`Tool`] | what the model can do, and its schema | sending the schema, the gating, the recording |
//! | [`PermissionPolicy`] | what is allowed | the question, and the refusal |
//! | [`Projector`] | the shape of a request | the context it is projected from |
//! | [`TokenCounter`] | how tokens are counted | every number it reports, and what each request really cost |
//! | [`Compactor`] | what to drop when it gets full | the veto on pinned items, and the report |
//!
//! # How to use it
//!
//! 1. create a [`Kernel`] with a [`Config`]
//! 2. give it a [`Provider`], and whichever [`Tool`]s and [`PermissionPolicy`] you want
//! 3. [`Kernel::push`] context and [`Kernel::step`] (or [`Kernel::turn`]) the loop
//! 4. read [`Kernel::subscribe`] to see what is happening, and [`Kernel::items`] plus
//!    [`Kernel::budget`] to see what the next request will cost
//!
//! ```
//! use std::sync::Arc;
//!
//! use nachalnik::{
//!     async_trait, BoxError, Config, ContextItem, ContextState, DeltaSink, Kernel, ModelInfo,
//!     ModelRequest, ModelResponse, Provider, State, StopReason,
//! };
//!
//! // a provider is anything that can answer a request
//! struct Parrot;
//!
//! #[async_trait]
//! impl Provider for Parrot {
//!     fn info(&self) -> ModelInfo {
//!         ModelInfo::new("example", "parrot").with_context_limit(8_192)
//!     }
//!
//!     async fn respond(
//!         &self,
//!         request: ModelRequest,
//!         _deltas: DeltaSink,
//!     ) -> Result<ModelResponse, BoxError> {
//!         let last = request.messages.last().and_then(|m| m.content.clone());
//!         Ok(ModelResponse::text(last.unwrap_or_default().to_text().into_owned()))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let kernel = Kernel::new(Config::default());
//! kernel.set_provider(Arc::new(Parrot));
//!
//! // context is added explicitly, and every item can be named afterwards
//! let file = kernel.push(ContextItem::file("src/parser.rs", "fn parse() {}").pinned());
//! kernel.push(ContextItem::user("why is this failing?"));
//!
//! // exactly what is about to be sent, before it is sent
//! assert_eq!(kernel.preview_request()?.messages.len(), 2);
//! assert_eq!(kernel.state(), State::Idle);
//!
//! // the loop
//! let State::Finished { item, stop } = kernel.turn().await? else {
//!     panic!("nothing needed deciding")
//! };
//! assert!(kernel.item(item).is_some());
//! assert_eq!(stop, StopReason::EndTurn);
//!
//! // and the context remains yours
//! kernel.set_state([file], ContextState::Excluded, Some("too big".into()));
//! assert!(kernel.undo().unwrap());
//! # Ok(())
//! # }
//! ```
//!
//! # Where the state lives
//!
//! - [`Context`] is a list of identified [`ContextItem`]s. Nothing is silently dropped: removal is
//!   a state change ([`Kernel::set_state`]), so a removed item can still be listed, inspected and
//!   restored. That holds for an output limit too: unless [`Config::keep_truncated_output`] is
//!   turned off, the whole of what a tool returned is kept beside the truncated copy the model is
//!   shown. [`ContextState::Elided`] is between in and out: the item stays in the request as a
//!   short marker, so a tool result stops costing what it holds and still answers its call, where
//!   excluding it would take the call out too.
//! - An assistant turn is recorded as the model produced it: three slots where a provider reports a
//!   content slot, a reasoning slot and a list of calls, or [`Content::Blocks`] where it reports an
//!   ordered sequence, because the order is part of the message and cannot be recovered once
//!   flattened. [`LinearProjector::send_blocks`] decides which shape the next request uses, and
//!   flattening reports in [`Projection::repairs`] what it cost.
//! - [`Projection`] is what the context becomes on the wire, with what was left out and why.
//! - [`Event`] is everything that happens, including every state transition, broadcast live and
//!   recorded in an append-only [`Session`] log that survives changes to the client and the model.
//! - Token figures come from a [`TokenCounter`]; the default is an estimate that comes out low.
//!   [`Calibrating`] corrects it: the kernel reports each request's estimate beside what the
//!   provider charged, and the counter corrects itself from the first response large enough to
//!   learn from. A counter that cannot price something says so rather than returning `0`:
//!   [`TokenCounter::uncounted`] feeds [`Budget::uncounted`] and [`ContextItem::uncounted`], so a
//!   figure that is a floor is never mistaken for a complete one. A [`Content::Blob`] is the usual
//!   case, since a picture's cost is a formula over its dimensions that each vendor defines
//!   differently; [`Blob::meta`] carries the inputs, and `examples/pricing_a_picture.rs` writes one
//!   out.
//! - [`Snapshot`] is where it all ended up: [`Kernel::snapshot`] and [`Kernel::resume`] carry a
//!   session across processes, because a log whose events name their items cannot rebuild them.
//!
//! # Surviving a crash
//!
//! A snapshot restores state, not the world. The kernel can tell you afterwards which call the
//! model asked for and never got an answer to, but not whether what that call would have done got
//! done, because the kernel did not do it. That half is the application's:
//!
//! - A [`ToolCallId`] is durable: it survives a [`Snapshot`], and [`Snapshot::used_calls`] stops a
//!   resumed session issuing it again, so an external operation keyed on it can be looked up later.
//!   An identifier a tool mints inside `invoke` dies with the process.
//! - A call with no result is findable: the assistant item holds the call and no
//!   [`ContextKind::ToolResult`] answers it. Whether its effect happened, only the external system
//!   can say.
//! - Returning [`ToolOutput::error`] is not the crash case; that is an answer, recorded and read by
//!   the model. The crash case is a process that died with the call in flight.
//!
//! Checkpointing is two writes, and their order decides what a crash can cost: **copy, write, drop,
//! then snapshot**. [`Kernel::history_since`] returns clones and the kernel keeps its copy;
//! [`Kernel::drain_history`] returns the only copy, so draining before writing leaves a window in
//! which the records exist nowhere. The snapshot goes last because a snapshot behind the log is a
//! state the log can explain, while one ahead of it is a state nothing accounts for.
//! `tests/crash.rs` checks this.
//!
//! # Features
//!
//! Both are off by default, because neither is part of the runtime:
//!
//! - `selectors`: [`selectors::Selector`], a small language for naming context items (`17`,
//!   `10-15`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`) that resolves to
//!   [`ContextId`]s a client can show before acting on them.
//! - `test`: a scripted [`Provider`], a few dummy [`Tool`]s, off-the-shelf permission policies and
//!   a mechanical [`Compactor`], for testing an agent without a network.

mod compaction;
mod config;
mod context;
mod error;
mod event;
mod kernel;
mod model;
mod permissions;
mod projection;
mod session;
mod tokens;
mod tool;

#[cfg(feature = "selectors")]
pub mod selectors;
#[cfg(feature = "test")]
pub mod test;

pub use async_trait::async_trait;

#[allow(deprecated)]
pub use crate::compaction::Removed;

pub use crate::{
    compaction::{Budget, CompactionPlan, CompactionReport, Compactor, ReportItem},
    config::Config,
    context::{Context, ContextId, ContextIds, ContextItem, ContextKind, ContextState},
    error::{BoxError, Error, Result},
    event::{Delta, DeltaSink, Event, OutputSink},
    kernel::{Kernel, State, StateChange},
    model::{
        Blob, Block, Content, Message, ModelInfo, ModelRequest, ModelResponse, Overrun, Params,
        Part, Provider, Role, StopReason, TooLong, ToolCall, ToolCallId, Usage,
    },
    permissions::{
        AskAlways, Capability, Domain, Grant, GrantSource, PermissionId, PermissionPolicy,
        PermissionRequest, Verdict,
    },
    projection::{LinearProjector, Projection, Projector, Skipped},
    session::{FORMAT, Record, Session, Snapshot},
    tokens::{BytesPerToken, Calibrating, Calibration, TokenCounter},
    tool::{Tool, ToolOutput, ToolSpec},
};
