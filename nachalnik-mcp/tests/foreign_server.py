"""A minimal MCP server in plain Python, for the bridge to talk to.

It exists so that the bridge is tested against something that is not the same SDK it is built
on: newline-delimited JSON-RPC over stdin and stdout, hand-written, with no dependencies. If the
bridge only ever spoke to an `rmcp` server, what would be under test is a round trip through one
library rather than a protocol.
"""

import json
import sys

TOOLS = [
    {
        "name": "add",
        "description": "adds two numbers",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
        },
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "explodes",
        "description": "always reports a failure",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "shout",
        "description": "returns its argument in capitals",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
]


def result_for(method, params):
    if method == "initialize":
        return {
            # echoing the client's version back is the friendly answer, and keeps this file
            # from going stale every time the specification is revised
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "foreign", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "add":
            total = args.get("a", 0) + args.get("b", 0)
            return {"content": [{"type": "text", "text": str(total)}]}
        if name == "explodes":
            return {
                "content": [{"type": "text", "text": "it went wrong over here"}],
                "isError": True,
            }
        if name == "shout":
            said = str(args.get("text", "")).upper()
            return {"content": [{"type": "text", "text": said}]}
        return {"content": [{"type": "text", "text": f"no tool {name}"}], "isError": True}

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
