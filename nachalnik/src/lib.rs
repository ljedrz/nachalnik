#![deny(missing_docs)]
#![deny(unsafe_code)]

//! **nachalnik** is a small, honest agent runtime: an execution loop around a language model in
//! which the context, the tools, the permissions and the requests are explicit, inspectable
//! state rather than hidden behavior.
//!
//! The agent is not the boss. You are.
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
//! [`Kernel::step`] performs exactly one of those transitions and returns the [`State`] it
//! produced; [`Kernel::turn`] repeats until the model ends its turn or somebody has to decide
//! something. `Finished` carries the model's own [`StopReason`], because a turn that ran out of
//! output tokens is not the same thing as one that ended. [`State::Requesting`] and [`State::Executing`] mean the loop is already being
//! driven, and a second [`Kernel::step`] is [`Error::Busy`] rather than a second request.
//!
//! Every other state is a resting state, and whatever you change while the kernel rests is what
//! the next request will contain. [`State::Ready`] exists for exactly that reason: the model has
//! said which tools it wants, nothing has run yet, and you can look first.
//!
//! [`Kernel::interrupt`] can be called from anywhere and stops the loop before the next
//! transition. Stopping something already in flight is cooperative, because the kernel owns
//! neither the socket nor the future: a [`Provider`] that checks [`DeltaSink::is_interrupted`]
//! and a [`Tool`] that checks [`OutputSink::is_interrupted`] can hand back what they have, and
//! whatever they hand back is recorded like any other turn.
//!
//! # What it does not do
//!
//! There is no system prompt in this crate. No instructions, no personality, no planning
//! ritual, no "think step by step", no default tools, no filesystem access, no process
//! spawning, no HTTP client, no subagents, no MCP, no background activity, no `/context`
//! renderer, no permission table, and no automatic context management unless you install a
//! [`Compactor`] - which then reports every single thing it did.
//!
//! # What it does not protect you from
//!
//! There is no sandbox here, and there is not going to be one in this crate. It is worth saying so
//! plainly in a library that uses the word *permissions*.
//!
//! The kernel executes nothing: no filesystem code, no network code, no process spawning. Every
//! side effect in a session happens inside a [`Tool`] you wrote and registered, so there is
//! nothing here to contain, and containment - a jail, a namespace, `seccomp`, a container - goes
//! inside your tool or around the whole process.
//!
//! What it does enforce is one thing: a call the [`PermissionPolicy`] refused is never handed to
//! [`Tool::invoke`], and the refusal is recorded as an [`Event`] and as a tool result the model is
//! told about. That is a decision point with a paper trail, not a boundary. Four consequences,
//! none of them a bug:
//!
//! - A [`Capability`] is a tool's own declaration, not a verified property; the kernel has nothing
//!   to check it against.
//! - [`Capability::Shell`] subsumes every other one, so a policy that allows it has allowed all of
//!   them whatever it answers about the rest.
//! - A policy that reads a command's text is a heuristic. It can make a refusal real for what was
//!   written; it cannot stop a program that reaches the network some other way. Confinement that
//!   *can* belongs where the process is spawned - see `kamchatka`, which puts its `shell` tool
//!   under Landlock and so turns `network: deny` into a refused `connect` syscall.
//! - Anything in the context is something the model reads, and it can carry instructions. What
//!   this runtime offers against that is the policy - which nothing in a model's output reaches
//!   except as a tool name and arguments - and a context you can see before the request goes.
//!
//! # The parts are yours
//!
//! The kernel does not own a UI, an editor, a model, or a tool. Those are your side of the
//! interface:
//!
//! | trait | you provide | the kernel provides |
//! | --- | --- | --- |
//! | [`Provider`] | a model, however you reach it | the request, verbatim |
//! | [`Tool`] | what the model can do | the schema, the gating, the recording |
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
//!         ModelInfo {
//!             context_limit: Some(8_192),
//!             ..ModelInfo::new("example", "parrot")
//!         }
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
//! assert!(kernel.undo());
//! # Ok(())
//! # }
//! ```
//!
//! # Where the state lives
//!
//! - [`Context`] is a list of identified [`ContextItem`]s. Nothing is ever silently dropped:
//!   removal is a state change ([`Kernel::set_state`]), so a removed item can still be listed,
//!   inspected, and restored - and that holds even for an output limit, which records the whole
//!   of what a tool said beside the shortened copy the model is shown.
//! - [`Projection`] is what the context turns into on the wire, complete with what was left out
//!   and why.
//! - [`Event`] is everything that happens - including every state transition - broadcast live
//!   and recorded in an append-only [`Session`] log that survives changes to the client and the
//!   model.
//! - The token figures are a [`TokenCounter`]'s, and the default one is an estimate that comes
//!   out low. [`Calibrating`] closes the loop instead of guessing better: the kernel reports what
//!   a request was estimated at beside what the provider charged for it, and the counter corrects
//!   itself from the first response.
//! - [`Snapshot`] is where it all ended up, which is a different question: [`Kernel::snapshot`]
//!   and [`Kernel::resume`] carry a session across processes, because a log of events that name
//!   their items cannot rebuild the items.
//!
//! # Features
//!
//! Both are off by default, because neither is part of the runtime:
//!
//! - `selectors`: [`selectors::Selector`], a small language for naming context items
//!   (`17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`) that resolves to
//!   [`ContextId`]s a client can show before acting on them.
//! - `test`: a scripted [`Provider`], a few dummy [`Tool`]s, off-the-shelf permission policies
//!   and a mechanical [`Compactor`], for testing an agent without a network.

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

pub use crate::{
    compaction::{Budget, CompactionPlan, CompactionReport, Compactor, Removed},
    config::Config,
    context::{Context, ContextId, ContextItem, ContextKind, ContextState},
    error::{BoxError, Error, Result},
    event::{Delta, DeltaSink, Event, OutputSink},
    kernel::{Kernel, State, StateChange},
    model::{
        Content, Message, ModelInfo, ModelRequest, ModelResponse, Params, Provider, Role,
        StopReason, ToolCall, ToolCallId, Usage,
    },
    permissions::{
        AskAlways, Capability, Grant, GrantSource, PermissionId, PermissionPolicy,
        PermissionRequest, Verdict,
    },
    projection::{LinearProjector, Projection, Projector, Skipped},
    session::{Record, Session, Snapshot},
    tokens::{BytesPerToken, Calibrating, Calibration, TokenCounter},
    tool::{Tool, ToolOutput, ToolSpec},
};
