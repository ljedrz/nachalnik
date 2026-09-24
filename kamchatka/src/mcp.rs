//! MCP servers, spawned and offered as tools like any other.
//!
//! note: a thin wrapper around [`nachalnik_mcp`] and nothing more - it spawns what it was told to
//! spawn, installs the tools, and hands back the servers. It is in the library rather than in
//! `main.rs` because an embedder that wants somebody else's tools should not have to re-derive the
//! one part of this that is not obvious: the name.

use std::{collections::HashSet, sync::Arc};

use nachalnik::{Kernel, Tool};
use nachalnik_mcp::Server;

use crate::tools::Careful;

/// Starts the servers that were asked for, and puts their tools in the same list as the rest.
///
/// note: the name matters more than it looks. It prefixes every tool the server offers and it is
/// what `--allow-server <name>` grants permission to - so `files=npx -y ...` is one server and
/// not the next one. Taken from the program instead it would be `npx` or `python3` for most of
/// the servers people actually run, which is why `name=command` is accepted and worth giving.
///
/// note: the policy is told which tools came from which server, rather than working it out from
/// their names. A prefix is optional and is dropped when a name would not otherwise fit, so the
/// only thing that reliably knows where a tool came from is whatever installed it - which is
/// here.
///
/// note: the servers have to outlive every session that offers their tools. Dropping one takes its
/// child process with it, and leaves its tools registered and unable to answer.
///
/// note: every server is spawned and listed before any tool goes into the kernel, so an `Err`
/// leaves the kernel and the policy as they were. Installing as it went would leave the tools of
/// the servers before the one that failed registered, and their servers dropped with the `Err`.
pub async fn attach(
    kernel: &Kernel,
    policy: &Careful,
    specs: &[String],
) -> Result<Vec<Server>, String> {
    let mut servers = Vec::new();
    let mut offered = Vec::new();
    let mut taken: HashSet<String> = kernel.tool_ids().into_iter().collect();

    for spec in specs {
        let (name, line) = named(spec);
        let mut words = line.split_whitespace();
        let program = words.next().ok_or("an MCP server needs a command to run")?;

        let mut command = tokio::process::Command::new(program);
        command.args(words);

        let server = Server::spawn(name, command)
            .await
            .map_err(|e| format!("`{line}` did not answer the handshake: {e}"))?;
        let tools = server
            .tools()
            .await
            .map_err(|e| format!("`{line}` would not list its tools: {e}"))?;
        // note: refused rather than let stand, as a failed handshake is. Two servers under one
        // name - two `npx` lines with no `name=` - offer their tools under the same identifiers,
        // and installing the second would quietly take the first one's out from under it
        let clashes = claim(&mut taken, &tools);
        if !clashes.is_empty() {
            return Err(format!(
                "`{line}` offers {} under a name another server's tools already have; give each \
                 server its own with `name=command`",
                clashes.join(", ")
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

/// Installs the tools of servers that are already running into another kernel, as `/restart`
/// does, and hands back a sentence for each server whose tools were left out.
///
/// note: held to the rule `attach` is. A server's list can change between one session and the
/// next, and [`Server::install`] reports a clash in `replaced` only after the tool that was there
/// first is gone - so a server offering an identifier something else already has is left out
/// whole, before any of its tools go in.
///
/// note: left out rather than refused, because a session is already running: a server short is
/// worth saying and not worth ending a run over.
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
        if !clashes.is_empty() {
            left_out.push(format!(
                "`{}` now offers {} under a name another tool already has, so none of its tools \
                 are in this session",
                server.name(),
                clashes.join(", ")
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

/// The identifiers among `tools` that are taken already or that `tools` offers twice. `taken`
/// gains the rest only when there are none, so a server left out claims nothing.
fn claim(taken: &mut HashSet<String>, tools: &[Arc<dyn Tool>]) -> Vec<String> {
    let mut claimed = HashSet::new();
    let clashes: Vec<String> = tools
        .iter()
        .map(|tool| tool.spec().id)
        .filter(|id| taken.contains(id) || !claimed.insert(id.clone()))
        .collect();
    if clashes.is_empty() {
        taken.extend(claimed);
    }

    clashes
}

/// The name a server is given by one `--mcp` spec, and the command line that starts it.
///
/// note: its own function so that `--allow-server` and `--deny-server` can be held to the names
/// before anything is spawned, by the same reading `attach` gives them.
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
