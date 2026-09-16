//! MCP servers, spawned and offered as tools like any other.
//!
//! note: a thin wrapper around [`nachalnik_mcp`] and nothing more - it spawns what it was told to
//! spawn, installs the tools, and hands back the servers. It is in the library rather than in
//! `main.rs`, where it was, because an embedder that wants somebody else's tools should not have
//! to re-derive the one part of this that is not obvious: the name.

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
/// note: the servers have to outlive the session. Dropping one takes its child process, and its
/// tools, with it.
pub async fn attach(
    kernel: &Kernel,
    policy: &Careful,
    specs: &[String],
) -> Result<Vec<Server>, String> {
    let mut servers = Vec::new();

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
        let installed = server
            .install(kernel)
            .await
            .map_err(|e| format!("`{line}` would not list its tools: {e}"))?;
        for tool in &installed.added {
            policy.came_from(tool, server.name());
        }
        servers.push(server);
    }

    Ok(servers)
}
