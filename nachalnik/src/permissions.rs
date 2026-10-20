//! Who may run what: the capabilities a tool declares, the policy asked about a call, and the
//! answer it gives.
//!
//! note: a decision point with a record, not a boundary. One thing is enforced: a refused call is
//! never handed to [`Tool::invoke`](crate::Tool::invoke). Every type below is a label the kernel
//! compares and reports, not a property it can check; containment belongs inside a tool or around
//! the whole process.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ToolCall, ToolCallId};
#[cfg(doc)]
use crate::{Kernel, Tool};

/// The family of side effect an operation belongs to: what a rule is written about.
///
/// note: the runtime names the three domains every agent has (a filesystem, processes, sockets);
/// anything else, such as a client's own `context`, arrives as [`Domain::Other`], named by whoever
/// invented it.
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
    ///
    /// note: `fs`, `exec` and `net` are not prevented here, but they read back as the variants
    /// above, so an `Other("fs")` is not equal to [`Domain::Fs`] until it is written out and read
    /// back. [`Domain::from`] gives the right one for any name.
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
/// note: two levels and no more: a domain is a thing acted on and an operation an act on it, so a
/// rule is about the thing (`fs`) or about one act (`fs:read`). A flat list of tool-shaped names
/// (`read` the capability, declared by `read` the tool) reads as a tautology and ties a rule's
/// granularity to a tool's.
///
/// note: labels the kernel compares and reports but cannot verify. A tool that declares `fs:read`
/// and opens a socket is lying, and the only defence is that the user chose to register it.
///
/// note: `exec:run` subsumes every other capability. A command can read, write and reach the
/// network, so a policy that allows `exec` has allowed all of it, whatever it answers about the
/// rest; that is what a shell is. A client showing `exec: allow` beside `net: deny` without saying
/// so would report a restriction that does not exist. The arguments close the gap: a
/// [`PermissionPolicy`] is handed the call the model made ([`PermissionRequest::args`]), so it can
/// judge `curl https://…` against its view of the network. `kamchatka`'s `Careful` does, and its
/// `reaches_the_network` shows what a heuristic over a command line is and is not worth.
///
/// note: `net` still earns its place. That a session with a shell can reach the network anyway is
/// no objection to this domain in particular: `fs:read` is just as unverifiable. Where there is no
/// shell - an agent whose tools all come from MCP servers, an editor integration that reads, writes
/// and fetches - refusing `net` refuses every way the registered tools have of reaching the
/// network.
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
    /// as `domain:op` (by [`fmt::Display`], and by serde, declared `into = "String"`) and
    /// [`Capability::parse`] reads exactly one colon back, so
    /// `Capability::of(Domain::Fs, "read:all")` serializes to text its own deserializer refuses,
    /// and a log record carrying it cannot be read back. The operation is the client's vocabulary
    /// and is not policed; a name with a colon in it names two things.
    pub fn of(domain: Domain, op: impl Into<String>) -> Self {
        Self {
            domain,
            op: op.into(),
        }
    }

    /// One in [`Domain::Fs`]: `read` or `write`, by what a call does to the files.
    ///
    /// note: one constructor for each domain this crate can name, since every agent has them. The
    /// operation stays a string, because which acts a filesystem has is the client's vocabulary.
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
    /// note: the inverse of [`fmt::Display`], so a rule spelled on a command line and one read off
    /// a screen are the same rule. Exactly one colon and neither half empty: `fs` is a domain, a
    /// different kind of rule, and `fs:` is a typo.
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
    /// The capabilities this call needs: [`crate::Tool::needs`], which may be narrower than
    /// everything the tool declares.
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
/// note: independent of the model: nothing in a model's output reaches it except the tool name and
/// the arguments, as data. A model asking nicely, or claiming it was already granted permission,
/// has no effect.
#[async_trait::async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Returns the verdict for the given call.
    ///
    /// note: `async` so an interactive policy can ask itself and return [`Verdict::Allow`] or
    /// [`Verdict::Deny`]. Returning [`Verdict::Ask`] pushes the question to whoever drives the
    /// kernel, which is usually what a client wants.
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict;

    /// Why this policy refused a call, in words the model can act on; `None` when it has nothing to
    /// add beyond the refusal itself.
    ///
    /// note: called only for a call this policy's own verdict refused, and what it returns goes
    /// into the tool result the model reads. A call the policy left to [`Kernel::decide`] and a
    /// person then refused is not asked about: the person had their own reason, and the policy's
    /// account of a decision it did not make would read to the model as the rule to work around.
    /// The default is `None`; the kernel still reports whether a refusal was a standing rule or an
    /// answer to this one call, which it knows on its own.
    ///
    /// note: the reason is the policy's, in its own vocabulary (which capability, which path rule,
    /// which of several subjects decided it); a kernel that invented one would be guessing at
    /// somebody else's decision.
    ///
    /// note: what a refused agent most needs to know is whether trying again is worth anything;
    /// without this, only the person's screen says why.
    ///
    /// note: it receives the same [`PermissionRequest`] [`Self::evaluate`] was asked about, not one
    /// rebuilt from an identifier, since the two methods are halves of one question. With only an
    /// identifier, a policy whose reason depends on the arguments would have to remember its
    /// decision keyed by call, and any bound on that memory could drop the reason before the kernel
    /// asks.
    fn why(&self, request: &PermissionRequest) -> Option<String> {
        let _ = request;

        None
    }

    /// The policy's name, for a client that wants to say which one is installed.
    ///
    /// note: the default is the implementing type's path; override it to say something friendlier.
    /// It is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// A [`PermissionPolicy`] that asks about everything.
///
/// note: the kernel's default and only policy; it grants nothing implicitly because the runtime
/// does not know what tools it has been given. Real policies such as a capability table, an
/// allowlist of commands, or a policy that consults a person are userland; see the `test` feature
/// and the examples.
#[derive(Debug, Clone, Copy, Default)]
pub struct AskAlways;

#[async_trait::async_trait]
impl PermissionPolicy for AskAlways {
    async fn evaluate(&self, _request: &PermissionRequest) -> Verdict {
        Verdict::Ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse` reads `domain:op` and refuses every spelling that does not name exactly one
    /// operation in one domain.
    ///
    /// note: one case per half of the guard. Each of these would otherwise come back as a
    /// capability no tool declares, so a rule written with it matches nothing and says nothing.
    #[test]
    fn a_capability_names_one_operation_in_one_domain() {
        assert_eq!(Capability::parse("fs:read"), Ok(Capability::fs("read")));

        for text in [":read", "fs:", "fs:read:all"] {
            assert!(Capability::parse(text).is_err(), "`{text}` was accepted");
        }
    }

    /// A verdict is displayed and serialized as the same word.
    ///
    /// note: `Display` is for a client's screen and serde is for logs, so a person and a program
    /// read the same word.
    #[test]
    fn a_verdict_is_shown_in_the_word_it_is_stored_in() {
        for verdict in [Verdict::Allow, Verdict::Ask, Verdict::Deny] {
            assert_eq!(
                serde_json::to_value(verdict).unwrap(),
                Value::String(verdict.to_string())
            );
        }
    }
}
