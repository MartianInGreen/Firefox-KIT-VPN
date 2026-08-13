#!/usr/bin/env bash
#
# End-to-end test for the KIT-VPN companion.
#
# Requires root and a working host network. It builds a *fake KIT* behind a
# REAL OpenVPN tunnel (static-key mode) running in a separate server
# namespace, then exercises the actual companion end to end:
#
#   curl --socks5-hostname 127.0.0.1:1080 http://www.kit.test:8080/
#        -> host relay  -> kitvpn netns -> tun0 (real OpenVPN) -> fake KIT
#
# and verifies that the host routing / existing VPN (e.g. Tailscale) is
# untouched and that everything fails closed when the tunnel is down.
#
# Usage: sudo bash tests/e2e.sh
#
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/companion/target/release/kit-vpn-companion"
T="/tmp/kit-vpn-e2e"
SRVNS="kitsrv"
CLINS="kitvpn"
KEY="$T/static.key"
CLIENT_OVPN="/etc/kit-vpn/kit.ovpn"
STATUS="/run/kit-vpn/status.json"
PROXY="127.0.0.1:1080"

PASS=0
FAIL=0

ok()   { PASS=$((PASS+1)); echo "  PASS: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }
check(){ if eval "$2"; then ok "$1"; else bad "$1"; fi }

PIDS=()
cleanup() {
  echo "==> cleanup"
  "$BIN" helper disable >/dev/null 2>&1 || true
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  ip netns del "$SRVNS" 2>/dev/null || true
  ip netns del "$CLINS" 2>/dev/null || true
  rm -rf "$T"
}
trap cleanup EXIT

if [[ $EUID -ne 0 ]]; then
  echo "must run as root: sudo bash tests/e2e.sh" >&2
  exit 1
fi

# remove stale state from interrupted runs
ip netns del "$SRVNS" 2>/dev/null || true
ip netns del "$CLINS" 2>/dev/null || true

echo "==> building companion"
cargo build --release --manifest-path "$REPO/companion/Cargo.toml" || exit 1

echo "==> stopping any previous instance + cleaning stale state"
"$BIN" helper disable >/dev/null 2>&1 || true
sleep 0.3

echo "==> fake KIT server side"
mkdir -p "$T"
openvpn --genkey secret "$KEY"

# server namespace "kitsrv" (the fake KIT network)
ip netns add "$SRVNS"
ip link add kitsrv0 type veth peer name kitsrv1
ip link set kitsrv1 netns "$SRVNS"
ip addr add 10.9.0.2/24 dev kitsrv0
ip link set kitsrv0 up
ip netns exec "$SRVNS" ip addr add 10.9.0.1/24 dev kitsrv1
ip netns exec "$SRVNS" ip link set lo up
ip netns exec "$SRVNS" ip link set kitsrv1 up
# route back to the client control-channel source through the host
ip netns exec "$SRVNS" ip route add 10.200.200.0/30 via 10.9.0.2 dev kitsrv1

# OpenVPN server in the fake KIT netns (real tunnel endpoint)
ip netns exec "$SRVNS" openvpn --allow-deprecated-insecure-static-crypto \
  --dev tun --ifconfig 10.8.0.1 10.8.0.2 \
  --secret "$KEY" --proto udp --port 1194 --cipher AES-256-CBC \
  --verb 2 --log "$T/srv.log" &
PIDS+=($!)

# wait until the tunnel device exists in the fake KIT netns
for _ in $(seq 1 30); do
  ip netns exec "$SRVNS" ip addr show tun0 2>/dev/null | grep -q '10.8.0.1' && break
  sleep 0.5
done
if ! ip netns exec "$SRVNS" ip addr show tun0 2>/dev/null | grep -q '10.8.0.1'; then
  echo "ERROR: fake KIT openvpn server did not create tun0" >&2
  cat "$T/srv.log" >&2 2>/dev/null || true
  exit 1
fi

# web + DNS inside the fake KIT netns, bound to the tunnel IP
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_web.py" 10.8.0.1 8080 &
PIDS+=($!)
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_dns.py" 10.8.0.1 53 10.8.0.1 "$T/dns.log" &
PIDS+=($!)
sleep 0.5

echo "==> installing client config"
mkdir -p /etc/kit-vpn
cat > "$CLIENT_OVPN" <<EOF
dev tun
proto udp
remote 10.9.0.1 1194
secret $KEY
ifconfig 10.8.0.2 10.8.0.1
persist-key
persist-tun
allow-deprecated-insecure-static-crypto
cipher AES-256-CBC
verb 3
EOF
chmod 600 "$CLIENT_OVPN"

cat > /etc/kit-vpn/config.toml <<EOF
ovpn_path = "/etc/kit-vpn/kit.ovpn"
socks_port = 1080
dns_servers = ["10.8.0.1"]
subnet = "10.200.200.0/30"
EOF

echo "==> baseline host routing (for later diff)"
ip route show table main | grep -v '^10\.200\.200\.0/30 dev kitvpn0 ' > "$T/route-main.before"
ip rule show > "$T/rules.before"
ip route show table 52 2>/dev/null > "$T/route-52.before" || true

echo "==> enable tunnel"
OUT="$("$BIN" helper enable)"
echo "  helper enable -> $OUT"

STATE="starting"
for _ in $(seq 1 40); do
  STATE="$(python3 -c "import json;print(json.load(open('$STATUS'))['state'])" 2>/dev/null || echo starting)"
  [[ "$STATE" == "running" ]] && break
  sleep 0.5
done
echo "  tunnel state = $STATE"
check "tunnel reaches running" "[[ '$STATE' == 'running' ]]"

echo "==> routing checks"
check "host default route untouched" \
  "diff -q <(ip route show table main | grep -v '^10\.200\.200\.0/30 dev kitvpn0 ') '$T/route-main.before' >/dev/null"
check "no policy-routing rules added" \
  "diff -q <(ip rule show) '$T/rules.before' >/dev/null"
# tailscale refreshes its own table periodically; just assert we never add
# anything of ours to it
MATCHES="$(ip route show table 52 2>/dev/null | grep -E 'kitvpn|10\\.200\\.200' || true)"
if [[ -z "$MATCHES" ]]; then
  ok "existing VPN table (52 / tailscale) has no KIT entries"
else
  echo "  table-52 matches: $MATCHES"
  bad "existing VPN table (52 / tailscale) has no KIT entries"
fi

echo "==> KIT traffic through the tunnel"
# the first request right after tunnel-up can transiently race the relay/socks
# startup; retry a couple of times before declaring failure
BODY=""
for _ in 1 2 3; do
  BODY="$(curl -s --socks5-hostname "$PROXY" --max-time 6 http://www.kit.test:8080/ 2>/dev/null || true)"
  [[ "$BODY" == *FAKE-KIT-OK* ]] && break
  sleep 1
done
if [[ "$BODY" != *FAKE-KIT-OK* ]]; then
  echo "---- diagnostics (tunnel up but request failed) ----"
  echo "-- client netns routes --"; ip netns exec "$CLINS" ip route 2>&1
  echo "-- client netns tun0 --"; ip netns exec "$CLINS" ip addr show tun0 2>&1
  echo "-- server netns routes --"; ip netns exec "$SRVNS" ip route 2>&1
  echo "-- server netns tun0 --"; ip netns exec "$SRVNS" ip addr show tun0 2>&1
  echo "-- fake services --"; pgrep -af fake_kit 2>&1 || echo none
  echo "-- srv openvpn log --"; tail -15 "$T/srv.log" 2>/dev/null
  echo "-- client openvpn log (full) --"; cat /run/kit-vpn/openvpn.log 2>/dev/null
  echo "-- direct tunnel data test (from client netns, no proxy) --"
  echo "  (client tun TX before):"
  ip netns exec "$CLINS" ip -s link show kitvpn-tun 2>/dev/null | grep -A1 'TX:' | tail -1
  ip netns exec "$CLINS" curl -s --max-time 4 http://10.8.0.1:8080/ 2>&1 || echo "direct curl failed: $?"
  echo "  (client tun TX after):"
  ip netns exec "$CLINS" ip -s link show kitvpn-tun 2>/dev/null | grep -A1 'TX:' | tail -1
  echo "  (server tun RX after):"
  ip netns exec "$SRVNS" ip -s link show tun0 2>/dev/null | grep -A1 'RX:' | tail -1
  echo "  (server tun TX after):"
  ip netns exec "$SRVNS" ip -s link show tun0 2>/dev/null | grep -A1 'TX:' | tail -1
  echo "-- udp echo test (kitvpn -> host -> kitsrv, bypassing openvpn) --"
  ip netns exec "$SRVNS" python3 - <<'PY' &
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("10.9.0.1", 1337))
s.settimeout(6)
try:
    data, addr = s.recvfrom(4096)
    s.sendto(b"ECHO:" + data, addr)
except socket.timeout:
    pass
PY
  ECHO_PID=$!
  sleep 0.3
  echo "  (client tun TX/RX before)"; ip netns exec "$CLINS" ip -s link show kitvpn-tun 2>/dev/null | grep -A2 'RX:' | tail -2
  ip netns exec "$CLINS" python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(4)
s.sendto(b"ping", ("10.9.0.1", 1337))
try:
    data, _ = s.recvfrom(4096)
    print("udp echo result:", data.decode())
except Exception as e:
    print("udp echo ERROR:", e)
PY
  echo "  (client tun TX/RX after)"; ip netns exec "$CLINS" ip -s link show kitvpn-tun 2>/dev/null | grep -A2 'RX:' | tail -2
  echo "  (server tun RX/TX)"; ip netns exec "$SRVNS" ip -s link show tun0 2>/dev/null | grep -A2 'RX:' | tail -2
  kill "$ECHO_PID" 2>/dev/null || true
  echo "-- fake dns query log --"; cat "$T/dns.log" 2>/dev/null || echo "(no queries received)"
  echo "-- curl verbose --"; curl -v --socks5-hostname "$PROXY" --max-time 4 http://www.kit.test:8080/ 2>&1 | tail -12
fi
check "www.kit.test reached fake KIT via tunnel (remote DNS + data)" "[[ \"\$BODY\" == *FAKE-KIT-OK* ]]"

BODY2="$(curl -s --socks5-hostname "$PROXY" --max-time 6 http://internal.kit.test:8080/x || true)"
check "internal.kit.test (hostname only resolvable via tunnel DNS)" "[[ \"\$BODY2\" == *FAKE-KIT-OK* ]]"

echo "==> non-KIT traffic stays on the normal connection"
check "direct https example.com still works (normal route)" \
  "curl -s --max-time 5 https://example.com/ | grep -qi example"

echo "==> fail-closed: LAN address through the proxy must fail (no leak to host net)"
if curl -s --socks5-hostname "$PROXY" --max-time 3 "http://192.168.178.1:22/" >/dev/null 2>&1; then
  bad "LAN address reachable through proxy (leak!)"
else
  ok "LAN address not reachable through proxy"
fi

echo "==> fail-closed: tunnel down -> tunneled requests fail, no fallback"
OPENVPN_PID="$(pgrep -f 'openvpn --config /etc/kit-vpn/kit.ovpn' | head -1)"
check "found client openvpn pid" "[[ -n \"\$OPENVPN_PID\" ]]"
kill -TERM "$OPENVPN_PID" 2>/dev/null || true
sleep 1
if curl -s --socks5-hostname "$PROXY" --max-time 3 http://www.kit.test:8080/ >/dev/null 2>&1; then
  bad "request succeeded after tunnel teardown (should fail closed)"
else
  ok "requests fail after tunnel teardown (fail-closed)"
fi
# even a public IP must not leak through the dead tunnel
if curl -s --socks5-hostname "$PROXY" --max-time 3 http://1.1.1.1/ >/dev/null 2>&1; then
  bad "public IP reachable through dead tunnel (leak!)"
else
  ok "no fallback to host network while tunnel is down"
fi

echo "==> disable + teardown"
"$BIN" helper disable >/dev/null
sleep 1
check "netns removed after disable" "! ip netns show | grep -q '^$CLINS'"
# teardown is asynchronous; give the supervisor a few seconds to reap children
VETH_GONE=no
for _ in $(seq 1 20); do
  if ! ip link show kitvpn0 >/dev/null 2>&1; then VETH_GONE=yes; break; fi
  sleep 0.3
done
check "veth removed after disable" "[[ \"$VETH_GONE\" == yes ]]"
check "socks port closed after disable" "! ss -ltn | grep -q '127.0.0.1:1080'"
check "status reports stopped" \
  "python3 -c \"import json,sys; sys.exit(0 if json.load(open('$STATUS'))['state']=='stopped' else 1)\""

# ---------------------------------------------------------------------------
# Native-messaging path as the unprivileged user (sudo -n helper), i.e. the
# exact path Firefox uses: extension -> nm process (user) -> sudo -> helper.
# ---------------------------------------------------------------------------
echo "==> native messaging roundtrip as unprivileged user (sudo -n path)"
E2E_USER="${SUDO_USER:-$(whoami)}"
if [[ "$E2E_USER" != "root" ]]; then
  sudo tee /etc/sudoers.d/kit-vpn-e2e >/dev/null <<EOF
$E2E_USER ALL=(root) NOPASSWD: $BIN helper enable
$E2E_USER ALL=(root) NOPASSWD: $BIN helper disable
EOF
  chmod 0440 /etc/sudoers.d/kit-vpn-e2e

  nm_roundtrip() { # $1 = message, $2 = expected state
    python3 -c "import sys,struct; m=sys.argv[1].encode(); sys.stdout.buffer.write(struct.pack('<I',len(m))+m)" "$1" \
      | runuser -u "$E2E_USER" -- "$BIN" nm > "$T/nm.out" 2>/dev/null || true
    python3 - "$T/nm.out" "$2" <<'PY'
import json, struct, sys
raw = open(sys.argv[1], 'rb').read()
if len(raw) < 4:
    print("  nm: no reply"); sys.exit(1)
n = struct.unpack('<I', raw[:4])[0]
msg = json.loads(raw[4:4+n])
state = msg.get('state')
print("  nm reply:", state, "|", msg.get('detail', '')[:80])
sys.exit(0 if state == sys.argv[2] else 1)
PY
  }

  check "nm enable (as user) starts the tunnel" "nm_roundtrip '{\"type\":\"enable\"}' starting"
  sleep 0.5
  STATE2="starting"
  for _ in $(seq 1 40); do
    STATE2="$(python3 -c "import json;print(json.load(open('$STATUS'))['state'])" 2>/dev/null || echo starting)"
    [[ "$STATE2" == "running" ]] && break
    sleep 0.5
  done
  check "tunnel running after nm enable" "[[ \"$STATE2\" == running ]]"
  check "nm status (as user) reports running" "nm_roundtrip '{\"type\":\"status\"}' running"
  check "nm disable (as user) stops the tunnel" "nm_roundtrip '{\"type\":\"disable\"}' stopped"
  sleep 1
  check "netns removed after nm disable" "! ip netns show | grep -q '^$CLINS'"
  rm -f /etc/sudoers.d/kit-vpn-e2e
else
  echo "  (skipped: running as root already)"
fi

echo
echo "==============================================="
echo " e2e result: $PASS passed, $FAIL failed"
echo "==============================================="
[[ $FAIL -eq 0 ]]
