"""An MCP server for `--mcp` to spawn, in plain Python and with nothing installed.

It is here rather than borrowed from `nachalnik-mcp/tests/foreign_server.py` because the two are
for different things and this crate's suite should stand on its own. That one is about the
protocol: three tools, annotations, an error result, and a second implementation for the bridge to
be measured against. This one is about the *child process* - `kamchatka::mcp::attach` is handed a
command line and splits it on whitespace, so what it needs is a path it can run, and one tool is
enough to prove a call crossed the boundary and came back.

Newline-delimited JSON-RPC over stdin and stdout, which is what the transport speaks.
"""

import json
import sys
import time

TOOLS = [
    {
        "name": "hang",
        "description": "never answers",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "add",
        "description": "adds two numbers",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
        },
    }
]


def result_for(method, params):
    if method == "initialize":
        return {
            # echoing the client's version back keeps this file from going stale every time the
            # specification is revised
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "arithmetic", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        args = params.get("arguments") or {}
        # a tool that will not stop, for the test about what a second `ctrl+c` is for. Nothing else
        # in that workspace refuses to stop: a provider watches the interrupt while it waits, and a
        # `shell` command is killed outright
        if params.get("name") == "hang":
            time.sleep(600)
        if params.get("name") == "add":
            total = args.get("a", 0) + args.get("b", 0)
            return {"content": [{"type": "text", "text": str(total)}]}
        return {
            "content": [{"type": "text", "text": "no such tool"}],
            "isError": True,
        }

    return None


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue

        # notifications carry no id and are never answered
        if "id" not in message:
            continue

        result = result_for(message.get("method"), message.get("params") or {})
        if result is None:
            answer = {
                "jsonrpc": "2.0",
                "id": message["id"],
                "error": {"code": -32601, "message": "method not found"},
            }
        else:
            answer = {"jsonrpc": "2.0", "id": message["id"], "result": result}

        sys.stdout.write(json.dumps(answer) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
