"""A logging pass-through to OpenRouter: proxy.py PORT LOGPREFIX writes every exchange to LOGPREFIX.NNN.

Point a run at it with BASE=http://127.0.0.1:PORT/v1. The bytes the endpoint sent are what decides
whether a broken tool call was the model, the upstream, or this workspace's stream assembly;
rawcheck.py reads them.
"""
import http.client
import itertools
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UP = "openrouter.ai"
LOG = sys.argv[2]
N = itertools.count()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def relay(self, method):
        body = self.rfile.read(int(self.headers.get("content-length", 0) or 0))
        conn = http.client.HTTPSConnection(UP, context=ssl.create_default_context(), timeout=600)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "accept-encoding", "content-length", "connection")}
        headers["accept-encoding"] = "identity"
        conn.request(method, "/api" + self.path, body=body or None, headers=headers)
        resp = conn.getresponse()
        n = next(N)
        with open("%s.%03d" % (LOG, n), "wb") as log:
            log.write(b"REQUEST " + body[:200000] + b"\n\nRESPONSE\n")
            self.send_response(resp.status)
            for k, v in resp.getheaders():
                if k.lower() not in ("transfer-encoding", "connection", "content-length", "content-encoding"):
                    self.send_header(k, v)
            self.send_header("connection", "close")
            self.end_headers()
            while True:
                chunk = resp.read1(65536) if hasattr(resp, "read1") else resp.read(65536)
                if not chunk:
                    break
                log.write(chunk)
                log.flush()
                self.wfile.write(chunk)
                self.wfile.flush()
        self.close_connection = True

    def do_POST(self):
        self.relay("POST")

    def do_GET(self):
        self.relay("GET")


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
