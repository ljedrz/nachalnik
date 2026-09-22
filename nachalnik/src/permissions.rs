//! Who may run what: the capabilities a tool declares, the policy asked about a call, and the
//! answer it gives.
//!
//! note: a decision point with a paper trail rather than a boundary. Exactly one thing is
//! enforced - a refused call is never handed to [`Tool::invoke`](crate::Tool::invoke) - and every
//! type below is a label the kernel compares and reports rather than a property it can check.
//! Containment belongs inside a tool or around the whole process.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ToolCall, ToolCallId};
#[cfg(doc)]
use crate::{Config, Kernel, Tool};

/// The family of side effect an operation belongs to: what a rule is written about.
///
/// note: three the runtime can name and everything else by its own name, which is exactly the
/// split there was before. `nachalnik` ships no tools, so it can vouch for the domains any agent
/// has - a filesystem, a process, a socket - and cannot know that a client calls one of its own
/// `context`. Those arrive as [`Domain::Other`], and the client that invented them is the one
/// with names for them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Domain {
    /// Files: reading them, listing them, searching them, writing them, changing them.
    Fs,
    /// Running a process.
    Exec,
    /// Talking to the network.
    Net,
    /// Anything else, named by whoever brought it.
    Other(String),
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fs => f.write_str("fs"),
            Self::Exec => f.write_str("exec"),
            Self::Net => f.write_str("net"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

impl From<&str> for Domain {
    fn from(name: &str) -> Self {
        match name {
            "fs" => Self::Fs,
            "exec" => Self::Exec,
            "net" => Self::Net,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// What a [`Tool`] does, as one operation in one domain: `fs:read`, `context:revise`.
///
/// note: two levels and no more, because the whole of what a permission rule has to be is
/// obvious. A domain is a thing that can be acted on and an operation is an act on it, so a rule
/// is either about the thing (`fs`) or about one act (`fs:read`) and there is no third question
/// to ask. It was a flat list of tool-shaped names before - `read` the capability, declared by
/// `read` the tool - which read as a tautology on the screen and left the granularity a tool
/// happened to offer as the granularity a rule could have.
///
/// note: these are labels the kernel compares and reports; it cannot verify them. A tool that
/// declares `fs:read` and then opens a socket is lying, and the only defense is that the user
/// chose to register it.
///
/// note: `exec:run` subsumes every other one, and it is worth saying so out loud because a list
/// of capabilities invites being read as a list of boundaries. A command can read, write, and
/// reach the network; a policy that allows `exec` has allowed all of it, whatever it answers
/// about the rest. That is not a flaw in the labels - it is what a shell *is* - but a client that
/// showed `exec: allow` beside `net: deny` without saying so would be reporting a restriction
/// that does not exist. What closes the gap is the arguments: a [`PermissionPolicy`] is handed
/// the call the model actually made ([`PermissionRequest::args`]), so it can judge `curl https://…`
/// against whatever it thinks of the network. See `kamchatka`'s `Careful` for one that does, and
/// for an honest account of what a heuristic over a command line is and is not worth.
///
/// note: `net` attracts the question of whether it earns its place, since a session that also has
/// a shell can reach the network through it whatever this says. The answer is that the objection
/// is not about that domain: `fs:read` is exactly as unverifiable, and every one of these is a
/// label rather than a boundary. Where there is no shell - an agent whose tools all come from MCP
/// servers, an editor integration that reads, writes and fetches - refusing it refuses the whole
/// of what the registered tools can do, which is a complete answer rather than a partial one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
#[non_exhaustive]
pub struct Capability {
    /// The thing being acted on.
    pub domain: Domain,
    /// The act, in that domain's own vocabulary.
    pub op: String,
}

impl Capability {
    /// One, from a domain and an operation in it.
    ///
    /// note: neither half may hold a colon, and nothing here enforces it. A capability is written
    /// as `domain:op` - by [`fmt::Display`], and by serde, which is declared `into = "String"` -
    /// and [`Capability::parse`] reads exactly one colon back, so `Capability::of(Domain::Fs,
    /// "read:all")` serializes to text that its own deserializer refuses and a log record carrying
    /// it cannot be read back. The operation is a client's own vocabulary and the constructor does
    /// not police it; a name with a colon in it is naming two things.
    pub fn of(domain: Domain, op: impl Into<String>) -> Self {
        Self {
            domain,
            op: op.into(),
        }
    }

    /// One in [`Domain::Fs`]: `read`, `glob`, `grep`, `write`, `edit`.
    ///
    /// note: three of these for the three domains this crate can name, because they are what
    /// every agent has and writing `Capability::of(Domain::Fs, "read")` at each of them buys
    /// nothing. The operation is still a string, which is the point: what the acts on a
    /// filesystem *are* is the client's vocabulary, not this crate's.
    pub fn fs(op: impl Into<String>) -> Self {
        Self::of(Domain::Fs, op)
    }

    /// One in [`Domain::Exec`], which in practice is `run`.
    pub fn exec(op: impl Into<String>) -> Self {
        Self::of(Domain::Exec, op)
    }

    /// One in [`Domain::Net`], which in practice is `reach`.
    pub fn net(op: impl Into<String>) -> Self {
        Self::of(Domain::Net, op)
    }

    /// Reads one back from `domain:op`, or says what is wrong with the text.
    ///
    /// note: the inverse of [`fmt::Display`], which is what makes a rule spelled on a command
    /// line or read off a screen the same rule. Exactly one colon, and neither half empty: the
    /// two ways to write a subject that names no operation are `fs` (a domain, and a different
    /// kind of rule) and `fs:`, which is a typo.
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((domain, op)) = text.split_once(':') else {
            return Err(format!(
                "`{text}` names no operation; it should be `domain:op`"
            ));
        };
        if domain.is_empty() || op.is_empty() || op.contains(':') {
            return Err(format!("`{text}` is not `domain:op`"));
        }

        Ok(Self::of(Domain::from(domain), op))
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.domain, self.op)
    }
}

impl From<Capability> for String {
    fn from(capability: Capability) -> Self {
        capability.to_string()
    }
}

impl TryFrom<String> for Capability {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
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

    /// Why this policy refused a call, in words the model can act on; `None` when it has nothing
    /// to add beyond the refusal itself.
    ///
    /// note: the kernel calls this only for a call it is about to refuse, and puts what comes
    /// back into the tool result the model reads. Defaulted, because a policy with nothing to say
    /// should implement nothing: the kernel still reports whether a refusal was a standing rule
    /// or an answer to this one call, which is the part it knows on its own.
    ///
    /// note: the reason itself is emphatically not the kernel's. It is made of a policy's own
    /// vocabulary - which capability, which path rule, which of several subjects actually did it
    /// - and a kernel that invented one would be guessing at somebody else's decision. This is
    /// the question, not the answer.
    ///
    /// note: what a refused agent most needs to know is whether trying again is worth anything,
    /// and until this existed the answer was on the person's screen and nowhere else - a policy
    /// that knew exactly why had no way to say so. A downstream policy needing a core change to
    /// do an ordinary thing is the sign of a seam that is not finished.
    ///
    /// note: the argument is the whole of the [`PermissionRequest`] [`Self::evaluate`] was asked
    /// about, and it is the same value rather than one built again to answer this - the kernel is
    /// holding it either way. The two methods are halves of one question, *what do you say about
    /// this call* and *why did you say it*, and asking the second with only an identifier meant a
    /// policy whose reason is a function of the arguments had to remember what it had decided a
    /// moment earlier, keyed by call. Remembering means bounding, and a bound means the reason
    /// can have fallen out of it by the time the kernel asks.
    fn why(&self, request: &PermissionRequest) -> Option<String> {
        let _ = request;

        None
    }

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
