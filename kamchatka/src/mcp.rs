//! MCP servers, spawned and offered as tools like any other.
//!
//! note: a thin wrapper around [`nachalnik_mcp`] and nothing more - it spawns what it was told to
//! spawn, installs the tools, and hands back the servers. It is in the library rather than in
//! `main.rs` because an embedder that wants somebody else's tools should not have to re-derive the
//! one part of this that is not obvious: the name.

use nachalnik::Kernel;
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
    let mut taken: std::collections::HashSet<String> = kernel.tool_ids().into_iter().collect();

    for spec in specs {
        // `env FOO=bar cmd` is a command rather than a name, which is what the guard is for
        let (name, line) = match spec.split_once('=') {
            Some((name, rest))
                if !name.is_empty()
                    && !name.contains(char::is_whitespace)
                    && !name.contains('/') =>
            {
                (name.to_owned(), rest.trim())
            }
            _ => (String::new(), spec.as_str()),
        };

        let mut words = line.split_whitespace();
        let program = words.next().ok_or("an MCP server needs a command to run")?;
        let name = match name.is_empty() {
            false => name,
            true => std::path::Path::new(program)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(program)
                .to_owned(),
        };

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
        let clashes: Vec<String> = tools
            .iter()
            .map(|tool| tool.spec().id)
            .filter(|id| !taken.insert(id.clone()))
            .collect();
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
