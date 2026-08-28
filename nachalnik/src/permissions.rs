use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ToolCall, ToolCallId};
#[cfg(doc)]
use crate::{Config, Kernel, Tool};

/// A class of side effect a [`Tool`] declares it needs.
///
/// note: These are labels the kernel compares and reports; it cannot verify them. A tool that
/// declares `Read` and then opens a socket is lying, and the only defense is that the user
/// chose to register it.
///
/// note: [`Capability::Shell`] subsumes every other one, and it is worth saying so out loud
/// because a list of capabilities invites being read as a list of boundaries. A command can read,
/// write, and reach the network; a policy that allows `Shell` has allowed all of it, whatever it
/// answers about the rest. That is not a flaw in the labels - it is what a shell *is* - but a
/// client that showed `shell: allow` beside `network: deny` without saying so would be reporting
/// a restriction that does not exist. What closes the gap is the arguments: a
/// [`PermissionPolicy`] is handed the call the model actually made
/// ([`PermissionRequest::args`]), so it can judge `curl https://…` against whatever it thinks of
/// the network. See `kamchatka`'s `Careful` for one that does, and for an honest account of what
/// a heuristic over a command line is and is not worth.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Reading data that is not already in the context.
    Read,
    /// Creating or replacing data.
    Write,
    /// Modifying existing data in place.
    Edit,
    /// Executing commands.
    Shell,
    /// Talking to the network.
    ///
    /// note: this one attracts the question of whether it earns its place, since a session that
    /// also has a shell can reach the network through it whatever this says. The answer is that
    /// the objection is not about this variant: [`Capability::Read`] is exactly as unverifiable,
    /// and every one of these is a label rather than a boundary. Where there is no shell - an
    /// agent whose tools all come from MCP servers, an editor integration with `read`, `write`
    /// and `fetch` - refusing this refuses the whole of what the registered tools can do, which
    /// is a complete answer rather than a partial one. It is kept for that case, and the case
    /// where it is not complete is [`Capability::Shell`]'s note, above.
    Network,
    /// Anything else, named by the tool.
    Custom(String),
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
            Self::Edit => f.write_str("edit"),
            Self::Shell => f.write_str("shell"),
            Self::Network => f.write_str("network"),
            Self::Custom(name) => f.write_str(name),
        }
    }
}

/// A [`PermissionPolicy`]'s answer about a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Run it.
    Allow,
    /// Do not run it until someone decides; the kernel will stop and ask.
    Ask,
    /// Do not run it.
    Deny,
}

impl Verdict {
    /// Returns the stricter of the two verdicts, `Deny` being the strictest and `Allow` the
    /// most permissive.
    pub fn strictest(self, other: Self) -> Self {
        match (self, other) {
            (Self::Deny, _) | (_, Self::Deny) => Self::Deny,
            (Self::Ask, _) | (_, Self::Ask) => Self::Ask,
            _ => Self::Allow,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        };
        f.write_str(s)
    }
}

/// A resolved permission: the answer a tool call is actually executed (or not) under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    /// The call may proceed.
    Allow,
    /// The call may not proceed.
    Deny,
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        };
        f.write_str(s)
    }
}

/// Where a [`Grant`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GrantSource {
    /// The active [`PermissionPolicy`] answered directly.
    Policy,
    /// The policy asked, and the answer came from [`Kernel::decide`].
    User,
    /// The call was cancelled via [`Kernel::cancel_pending_calls`].
    Cancellation,
}

/// The identifier of a permission request, used to answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PermissionId(pub u64);

impl fmt::Display for PermissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Everything known about a tool call at the moment permission for it is considered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The identifier used to answer this request with [`Kernel::decide`].
    pub id: PermissionId,
    /// The call the model requested.
    pub call: ToolCallId,
    /// The [`crate::ToolSpec::id`] of the tool.
    pub tool: String,
    /// The capabilities the tool declared.
    pub capabilities: Vec<Capability>,
    /// The arguments the model produced, verbatim.
    ///
    /// note: Shared with the call itself, so that asking about a tool call does not copy what it
    /// was going to do.
    pub args: Arc<Value>,
}

impl PermissionRequest {
    pub(crate) fn new(id: PermissionId, call: &ToolCall, capabilities: Vec<Capability>) -> Self {
        Self {
            id,
            call: call.id.clone(),
            tool: call.tool.clone(),
            capabilities,
            args: call.args.clone(),
        }
    }
}

/// Decides whether a tool call may run.
///
/// note: The policy is deliberately independent of the model: nothing in a model's output can
/// reach it except the tool name and the arguments, both of which are data. A model asking
/// nicely - or insisting that it has already been granted permission - has no effect.
#[async_trait::async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Returns the verdict for the given call.
    ///
    /// note: This is `async` so that an interactive policy can do the asking itself and return
    /// [`Verdict::Allow`] or [`Verdict::Deny`]. Returning [`Verdict::Ask`] instead pushes the
    /// question up to whoever is driving the kernel, which is usually what a client wants.
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict;

    /// What this is, for a client that wants to say which one is installed.
    ///
    /// note: The default is the implementing type's own path, which costs an implementor nothing
    /// and is right often enough to be worth having. Override it to say something friendlier. It
    /// is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// A [`PermissionPolicy`] that asks about everything.
///
/// note: This is the kernel's default, and the only policy it ships: it grants nothing
/// implicitly, which is the only honest starting point for a runtime that has no idea what
/// tools it has been given. Real policies - a capability table, an allowlist of commands, a
/// policy that consults a person - are userland; see the `test` feature and the examples.
#[derive(Debug, Clone, Copy, Default)]
pub struct AskAlways;

#[async_trait::async_trait]
impl PermissionPolicy for AskAlways {
    async fn evaluate(&self, _request: &PermissionRequest) -> Verdict {
        Verdict::Ask
    }
}
