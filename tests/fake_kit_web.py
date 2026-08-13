#!/usr/bin/env python3
"""Tiny HTTP server used by the e2e test as the "fake KIT" web service.

Usage: fake_kit_web.py <bind-ip> <port>
Responds with a marker body so tests can assert traffic arrived via the tunnel.
"""
import http.server
import socketserver
import sys


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if LOGFILE:
            with open(LOGFILE, "a") as f:
                f.write("{} {}\n".format(self.client_address[0], self.path))
        body = b"FAKE-KIT-OK " + self.path.encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(body)

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass


def main():
    global LOGFILE
    LOGFILE = sys.argv[3] if len(sys.argv) > 3 else None
    host = sys.argv[1]
    port = int(sys.argv[2])
    socketserver.ThreadingTCPServer.allow_reuse_address = True
    with socketserver.ThreadingTCPServer((host, port), Handler) as srv:
        srv.serve_forever()


if __name__ == "__main__":
    main()
