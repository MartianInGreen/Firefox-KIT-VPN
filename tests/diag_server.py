#!/usr/bin/env python3
"""Tiny HTTP listener that records diagnostics from the test extension."""
import http.server
import socketserver
import sys

LOG = sys.argv[1] if len(sys.argv) > 1 else "/tmp/diag.log"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 9123


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(LOG, "a") as f:
            f.write(self.path + "\n")
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a):
        pass


socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", PORT), H) as srv:
    srv.serve_forever()
