//! MCP servers, spawned and offered as tools like any other.
//!
//! note: a thin wrapper around [`nachalnik_mcp`]. It spawns servers, installs their tools, and
//! hands back the servers. It is in the library rather than `main.rs` so an embedder using
//! another's tools does not have to re-derive the name.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use nachalnik::{Kernel, Tool};
use nachalnik_mcp::Server;

use crate::tools::Careful;

/// Starts the requested servers and puts their tools in the same list as the rest.
///
/// note: the name prefixes every tool the server offers and is what `--auto-allow-server <name>`
/// grants permission to, so `files=npx -y ...` is one server and not the next. Taken from the
/// program it would be `npx` or `python3` for most servers, which is why `name=command` is
/// accepted.
///
/// note: the policy is told which tools came from which server rather than working it out from
/// their names. A prefix is optional and is dropped when a name would not otherwise fit, so only
/// the installer reliably knows where a tool came from.
///
/// note: servers must outlive every session that offers their tools. Dropping one takes its child
/// process with it and leaves its tools registered and unable to answer.
///
/// note: every server is spawned and listed before any tool goes into the kernel, so an `Err`
/// leaves the kernel and policy unchanged. Installing as it went would leave earlier servers' tools
/// registered and their servers dropped with the `Err`.
pub async fn attach(
    kernel: &Kernel,
    policy: &Careful,
    specs: &[String],
) -> Result<Vec<Server>, String> {
    attach_within(kernel, policy, specs, None).await
}

/// [`attach`], but every call to a server's tools is abandoned after waiting `within`; `None` waits
/// as long as the server takes.
///
/// note: a server that hangs, or answers with something `rmcp` drops (an id with neither a result
/// nor an error), left the call waiting until the turn was interrupted. There is no startup limit:
/// the first `npx -y` can take minutes to download its package, and each server is named on
/// standard error before it starts.
pub async fn attach_within(
    kernel: &Kernel,
    policy: &Careful,
    specs: &[String],
    within: Option<std::time::Duration>,
) -> Result<Vec<Server>, String> {
    let mut servers = Vec::new();
    let mut offered = Vec::new();
    let mut taken: HashSet<String> = kernel.tool_ids().into_iter().collect();

    // note: refused before anything is spawned, whatever the servers offer. The name is what
    // `--auto-allow-server` grants and what answering "always" at the prompt answers for, so two
    // servers under one name would be granted together whichever was meant
    let mut names = HashMap::new();
    for spec in specs {
        let (name, line) = named(spec);
        if let Some(first) = names.insert(name.clone(), line) {
            return Err(format!(
                "`{first}` and `{line}` are both named `{name}`; give each server its own with \
                 `name=command`"
            ));
        }
    }

    for spec in specs {
        let (name, line) = named(spec);
        let mut words = line.split_whitespace();
        let program = words.next().ok_or_else(|| {
            format!("`{spec}` names no command to run, which an MCP server needs")
        })?;

        let mut command = tokio::process::Command::new(program);
        command.args(words);

        // note: told apart by what failed because both arrive as `Error::Connect` and only the
        // spawn fails with an I/O error. A program that is not there never reached a handshake, and
        // saying it did not answer one sends somebody looking at the server
        let mut server = Server::spawn(name, command).await.map_err(|e| match &e {
            nachalnik_mcp::Error::Connect(why) if why.is::<std::io::Error>() => {
                format!("`{line}` could not be started: {why}")
            }
            _ => format!("`{line}` did not answer the handshake: {e}"),
        })?;
        if let Some(within) = within {
            server = server.answering_within(within);
        }
        let tools = server
            .tools()
            .await
            .map_err(|e| format!("`{line}` would not list its tools: {e}"))?;
        // note: refused rather than let stand, as a failed handshake is. Two names can still put
        // two tools under one identifier - `a` offering `b__c` and `a__b` offering `c`, or two long
        // ones cut short to fit - and installing the second would quietly take the first one's
        // identifier out from under it.
        //
        // note: a server offering one name twice is refused as well. Nothing `name=command` does
        // can separate its two tools
        let clashes = claim(&mut taken, &tools);
        let (elsewhere, twice): (Vec<String>, Vec<String>) =
            clashes.into_iter().partition(|id| taken.contains(id));
        if !elsewhere.is_empty() {
            return Err(format!(
                "`{line}` offers {} under a name another server's tools already have; give each \
                 server its own with `name=command`",
                elsewhere.join(", ")
            ));
        }
        if !twice.is_empty() {
            return Err(format!(
                "`{line}` offers more than one tool called {}",
                twice.join(", ")
            ));
        }
        offered.push(tools);
        servers.push(server);
    }

    for (server, tools) in servers.iter().zip(offered) {
        for tool in tools {
            policy.came_from(tool.spec().id, server.name());
            kernel.add_tool(tool);
        }
    }

    Ok(servers)
}

/// Installs the tools of running servers into another kernel, as `/restart` does, and returns a
/// sentence for each server whose tools were left out.
///
/// note: the same rule as `attach`. A server's list can change between sessions, and
/// [`Server::install`] reports a clash in `replaced` only after the tool already there is gone, so
/// a server offering an identifier something else already has is left out whole before any of its
/// tools go in.
///
/// note: left out rather than refused because a session is already running: report a short server,
/// do not end a run over it.
pub async fn reinstall(kernel: &Kernel, policy: &Careful, servers: &[Server]) -> Vec<String> {
    let mut taken: HashSet<String> = kernel.tool_ids().into_iter().collect();
    let mut left_out = Vec::new();

    for server in servers {
        let tools = match server.tools().await {
            Ok(tools) => tools,
            Err(e) => {
                left_out.push(format!(
                    "`{}` would not list its tools again: {e}",
                    server.name()
                ));
                continue;
            }
        };
        let clashes = claim(&mut taken, &tools);
        // as `attach` does: a name another tool has, or one the server offers twice; a duplicate
        // within the server does not depend on the other tools
        let (elsewhere, twice): (Vec<String>, Vec<String>) =
            clashes.into_iter().partition(|id| taken.contains(id));
        if !elsewhere.is_empty() {
            left_out.push(format!(
                "`{}` now offers {} under a name another tool already has, so none of its tools \
                 are in this session",
                server.name(),
                elsewhere.join(", ")
            ));
            continue;
        }
        if !twice.is_empty() {
            left_out.push(format!(
                "`{}` now offers more than one tool called {}, so none of its tools are in this \
                 session",
                server.name(),
                twice.join(", ")
            ));
            continue;
        }
        for tool in tools {
            policy.came_from(tool.spec().id, server.name());
            kernel.add_tool(tool);
        }
    }

    left_out
}

/// Identifiers in `tools` that are already taken or offered twice. `taken` gains the rest only when
/// there are none, so a left-out server claims nothing.
fn claim(taken: &mut HashSet<String>, tools: &[Arc<dyn Tool>]) -> Vec<String> {
    let mut claimed = HashSet::new();
    let mut clashes: Vec<String> = tools
        .iter()
        .map(|tool| tool.spec().id)
        .filter(|id| taken.contains(id) || !claimed.insert(id.clone()))
        .collect();
    // one name each, however many of its copies clashed: a name taken already and offered twice
    // was each copy a clash of its own, and said as "py__add, py__add"
    clashes.sort();
    clashes.dedup();
    if clashes.is_empty() {
        taken.extend(claimed);
    }

    clashes
}

/// The server name and command line from one `--mcp` spec.
///
/// note: a separate function so `--auto-allow-server` and `--auto-deny-server` can be checked
/// against the names before anything is spawned, using the same reading `attach` gives them.
pub(crate) fn named(spec: &str) -> (String, &str) {
    // `env FOO=bar cmd` is a command rather than a name, which is what the guard is for
    let (name, line) = match spec.split_once('=') {
        Some((name, rest))
            if !name.is_empty() && !name.contains(char::is_whitespace) && !name.contains('/') =>
        {
            (name.to_owned(), rest.trim())
        }
        _ => (String::new(), spec),
    };
    let name = match (name.is_empty(), line.split_whitespace().next()) {
        (true, Some(program)) => std::path::Path::new(program)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(program)
            .to_owned(),
        _ => name,
    };

    (name, line)
}
