#!/usr/bin/env python3
"""Tiny DNS responder used by the e2e test as the "fake KIT" DNS server.

Usage: fake_kit_dns.py <bind-ip> <port>

Answers A queries for any *.kit.test name with the configured answer IP
(default 10.8.0.1, the fake KIT web server address). Everything else gets
NXDOMAIN. This lets the test verify that hostname resolution happens through
the tunnel (remote DNS) and not via the host resolver.
"""
import socket
import struct
import sys

BIND = sys.argv[1]
PORT = int(sys.argv[2])
ANSWER = sys.argv[3] if len(sys.argv) > 3 else "10.8.0.1"
SUFFIX = ".kit.test"
LOGFILE = sys.argv[4] if len(sys.argv) > 4 else None


def log(msg):
    if LOGFILE:
        with open(LOGFILE, "a") as f:
            f.write(msg + "\n")


def parse_name(data, off):
    labels = []
    while True:
        if off >= len(data):
            raise ValueError("truncated")
        l = data[off]
        if l == 0:
            return b".".join(labels).decode(errors="replace"), off + 1
        if l & 0xC0:
            raise ValueError("compression in query not expected")
        labels.append(data[off + 1 : off + 1 + l])
        off += 1 + l


def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((BIND, PORT))
    sock.settimeout(1.0)
    while True:
        try:
            data, addr = sock.recvfrom(4096)
        except socket.timeout:
            continue
        if len(data) < 12:
            continue
        tid = data[0:2]
        try:
            name, off = parse_name(data, 12)
        except (ValueError, IndexError):
            continue
        log("query from {} for {} len={}".format(addr, name, len(data)))
        # echo the question (QNAME + QTYPE + QCLASS)
        qend = off + 4
        question = data[12:qend]
        if name.endswith(SUFFIX):
            flags = b"\x81\x80"  # QR RD RA
            ancount = 1
            answer = (
                b"\xc0\x0c"
                + struct.pack(">HHIH", 1, 1, 60, 4)
                + socket.inet_aton(ANSWER)
            )
        else:
            flags = b"\x81\x83"  # QR RD RA NXDOMAIN
            ancount = 0
            answer = b""
        resp = (
            tid
            + flags
            + struct.pack(">HHHH", 1, ancount, 0, 0)
            + question
            + answer
        )
        sock.sendto(resp, addr)


if __name__ == "__main__":
    main()
