"""A hostile OpenAI-compatible endpoint: the model id picks the misbehaviour."""
import gzip
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODES = ["ok", "tool", "badargs", "unknowntool", "dupid", "r429", "r500", "trunc", "garbage",
         "hang", "drip", "length", "nousage", "toolong", "empty", "huge", "nonstream", "gzip",
         "manytools", "nullcontent", "weirdusage", "reasoning", "toolthentext", "html", "slowheaders"]
COUNT = {}
LOCK = threading.Lock()


def chunk(delta, finish=None, usage=None, idx=0):
    body = {"id": "x", "object": "chat.completion.chunk", "created": 0, "model": "m",
            "choices": [{"index": idx, "delta": delta, "finish_reason": finish}]}
    if usage is not None:
        body["usage"] = usage
    return "data: " + json.dumps(body) + "\n\n"


USAGE = {"prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        sys.stderr.write("%s %s\n" % (self.command, self.path))

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            data = {"data": [{"id": m, "context_length": 20000 if m != "toolong" else 1000}
                             for m in MODES]}
            raw = json.dumps(data).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        else:
            self.send_error(404)

    def sse(self, parts, delay=0.0):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        for p in parts:
            self.wfile.write(p.encode())
            self.wfile.flush()
            if delay:
                time.sleep(delay)
        self.close_connection = True

    def plain(self, code, body, headers=()):
        raw = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("content-length", str(len(raw)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(raw)
        self.close_connection = True

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        req = json.loads(self.rfile.read(length) or b"{}")
        mode = req.get("model", "ok")
        with LOCK:
            COUNT[mode] = COUNT.get(mode, 0) + 1
            n = COUNT[mode]
        last = req.get("messages", [{}])[-1]
        answered_tool = last.get("role") == "tool"
        sys.stderr.write("  mode=%s n=%d msgs=%d last=%s\n" % (mode, n, len(req.get("messages", [])), last.get("role")))
        text = lambda t: [chunk({"role": "assistant", "content": t}), chunk({}, "stop", USAGE), "data: [DONE]\n\n"]

        def call(name, args, cid="call_1", idx=0):
            return chunk({"role": "assistant", "tool_calls": [{"index": idx, "id": cid, "type": "function",
                          "function": {"name": name, "arguments": args}}]})

        if mode == "ok":
            return self.sse(text("fine."))
        if mode == "tool" or mode == "toolthentext":
            if answered_tool:
                return self.sse(text("I read it."))
            parts = []
            if mode == "toolthentext":
                parts.append(chunk({"role": "assistant", "content": "Let me look. "}))
            parts += [call("fs", json.dumps({"call": {"action": "read", "path": "README.md"}})),
                      chunk({}, "tool_calls", USAGE), "data: [DONE]\n\n"]
            return self.sse(parts)
        if mode == "badargs":
            if answered_tool:
                return self.sse(text("the arguments were bad."))
            return self.sse([call("fs", '{"call": {"action": "read", "path": "REA'),
                             chunk({}, "tool_calls", USAGE), "data: [DONE]\n\n"])
        if mode == "unknowntool":
            if answered_tool:
                return self.sse(text("no such tool then."))
            return self.sse([call("rm_rf", '{"path": "/"}'), chunk({}, "tool_calls", USAGE), "data: [DONE]\n\n"])
        if mode == "dupid":
            if answered_tool and n > 3:
                return self.sse(text("done reading."))
            return self.sse([call("fs", json.dumps({"call": {"action": "glob", "pattern": "*"}}), cid="call_0"),
                             chunk({}, "tool_calls", USAGE), "data: [DONE]\n\n"])
        if mode == "r429":
            if n % 3 != 0:
                return self.plain(429, '{"error":{"message":"rate limited"}}', [("retry-after", "1"), ("content-type", "application/json")])
            return self.sse(text("after the wait."))
        if mode == "r500":
            return self.plain(500, '{"error":{"message":"internal"}}', [("content-type", "application/json")])
        if mode == "trunc":
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            self.wfile.write(chunk({"role": "assistant", "content": "half a sen"}).encode())
            self.wfile.write(b'data: {"id":"x","choices":[{"delta":{"content":"ten')
            self.wfile.flush()
            self.close_connection = True
            return
        if mode == "garbage":
            return self.sse([chunk({"role": "assistant", "content": "a"}), "data: this is not json\n\n",
                             chunk({}, "stop", USAGE), "data: [DONE]\n\n"])
        if mode == "hang":
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            self.wfile.flush()
            time.sleep(600)
            return
        if mode == "slowheaders":
            time.sleep(600)
            return
        if mode == "drip":
            parts = [chunk({"role": "assistant", "content": "w%d " % i}) for i in range(10)]
            return self.sse(parts + [chunk({}, "stop", USAGE), "data: [DONE]\n\n"], delay=3)
        if mode == "length":
            return self.sse([chunk({"role": "assistant", "content": "This answer is cut off in the mid"}),
                             chunk({}, "length", USAGE), "data: [DONE]\n\n"])
        if mode == "nousage":
            return self.sse([chunk({"role": "assistant", "content": "no usage here"}), chunk({}, "stop"), "data: [DONE]\n\n"])
        if mode == "weirdusage":
            return self.sse([chunk({"role": "assistant", "content": "odd usage"}),
                             chunk({}, "stop", {"prompt_tokens": -5, "completion_tokens": 10**12, "total_tokens": "lots"}),
                             "data: [DONE]\n\n"])
        if mode == "toolong":
            return self.plain(400, json.dumps({"error": {"message": "This endpoint's maximum context length is 1000 tokens. However, you requested about 4321 tokens (4000 of text input, 321 of tool input). Please reduce the length."}}),
                              [("content-type", "application/json")])
        if mode == "empty":
            return self.sse(['data: {"id":"x","choices":[]}\n\n', "data: [DONE]\n\n"])
        if mode == "huge":
            return self.sse([chunk({"role": "assistant", "content": "z" * 65536}) for _ in range(32)] +
                            [chunk({}, "stop", USAGE), "data: [DONE]\n\n"])
        if mode == "nonstream":
            return self.plain(200, json.dumps({"id": "x", "choices": [{"index": 0, "message": {"role": "assistant", "content": "not streamed"}, "finish_reason": "stop"}], "usage": USAGE}),
                              [("content-type", "application/json")])
        if mode == "gzip":
            raw = gzip.compress("".join(text("zipped")).encode())
            return self.plain(200, raw, [("content-type", "text/event-stream"), ("content-encoding", "gzip")])
        if mode == "manytools":
            if answered_tool:
                return self.sse(text("all forty read."))
            parts = [call("fs", json.dumps({"call": {"action": "read", "path": "README.md"}}), cid="c%d" % i, idx=i) for i in range(40)]
            return self.sse(parts + [chunk({}, "tool_calls", USAGE), "data: [DONE]\n\n"])
        if mode == "nullcontent":
            return self.sse([chunk({"role": "assistant", "content": None}), chunk({}, "stop", USAGE), "data: [DONE]\n\n"])
        if mode == "reasoning":
            return self.sse([chunk({"role": "assistant", "reasoning": "thinking hard..."}),
                             chunk({"reasoning": " still thinking"}),
                             chunk({"content": "the answer"}), chunk({}, "stop", USAGE), "data: [DONE]\n\n"])
        if mode == "html":
            return self.plain(502, "<html><body><h1>502 Bad Gateway</h1>" + "x" * 5000 + "</body></html>", [("content-type", "text/html")])
        return self.sse(text("unknown mode " + mode))


if __name__ == "__main__":
    port = int(sys.argv[1])
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
