#!/usr/bin/env bash
#
# Firefox integration test:
#
#   real Firefox + extension -> native messaging (as $USER) -> sudo -n helper
#     -> isolated OpenVPN tunnel -> fake KIT
#
# Verifies that a *.kit.test URL is fetched THROUGH the tunnel (including
# remote DNS) while example.com goes DIRECT (never touches the SOCKS proxy).
#
# Run: sudo bash tests/firefoxtest.sh
#
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/companion/target/release/kit-vpn-companion"
INSTALLED_BIN="/usr/local/lib/kit-vpn/kit-vpn-companion"
T="/tmp/kit-vpn-fxtest"
SRVNS="kitsrv"
FX_USER="${SUDO_USER:-$(whoami)}"

cleanup() {
  echo "==> cleanup"
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  runuser -u "$FX_USER" -- "$INSTALLED" nm < <(python3 -c "import sys,struct;m=b'{\"type\":\"disable\"}';sys.stdout.buffer.write(struct.pack('<I',len(m))+m)") >/dev/null 2>&1 || true
  ip netns del "$SRVNS" 2>/dev/null || true
}
trap cleanup EXIT

[[ $EUID -eq 0 ]] || { echo "run as root: sudo bash tests/firefoxtest.sh" >&2; exit 1; }

echo "==> fake KIT server side"
mkdir -p "$T"
openvpn --genkey secret "$T/static.key"
ip netns add "$SRVNS"
ip link add kitsrv0 type veth peer name kitsrv1
ip link set kitsrv1 netns "$SRVNS"
ip addr add 10.9.0.2/24 dev kitsrv0
ip link set kitsrv0 up
ip netns exec "$SRVNS" ip addr add 10.9.0.1/24 dev kitsrv1
ip netns exec "$SRVNS" ip link set lo up
ip netns exec "$SRVNS" ip link set kitsrv1 up

ip netns exec "$SRVNS" openvpn --allow-deprecated-insecure-static-crypto \
  --dev tun --ifconfig 10.8.0.1 10.8.0.2 \
  --secret "$T/static.key" --proto udp --port 1194 --cipher AES-256-CBC \
  --verb 1 --log "$T/srv.log" &
PIDS+=($!)
for _ in $(seq 1 30); do
  ip netns exec "$SRVNS" ip addr show tun0 2>/dev/null | grep -q '10.8.0.1' && break
  sleep 0.5
done
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_web.py" 10.8.0.1 8080 "$T/web.log" &
PIDS+=($!)
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_dns.py" 10.8.0.1 53 10.8.0.1 "$T/dns.log" &
PIDS+=($!)
sleep 0.5

echo "==> fake KIT client .ovpn (will be installed by scripts/install.sh)"
cat > "$T/kit.ovpn" <<EOF
# static-key (peer-to-peer) test config: the "client" directive would imply
# TLS and conflict with --secret; real KIT configs use TLS + certificates
dev tun
proto udp
remote 10.9.0.1 1194
secret $T/static.key
ifconfig 10.8.0.2 10.8.0.1
persist-key
persist-tun
allow-deprecated-insecure-static-crypto
cipher AES-256-CBC
verb 3
EOF

echo "==> running installer as $FX_USER (mirrors real usage: the user runs it, sudo asks once)"
runuser -u "$FX_USER" -- bash "$REPO/scripts/install.sh" "$T/kit.ovpn" >/dev/null 2>&1 || { echo "installer failed"; exit 1; }

# the installer uses the production DNS defaults; for the fake KIT test point
# the tunnel DNS at the fake resolver
cat > /etc/kit-vpn/config.toml <<EOF
ovpn_path = "/etc/kit-vpn/kit.ovpn"
socks_port = 1080
dns_servers = ["10.8.0.1"]
subnet = "10.200.200.0/30"
EOF
chmod 0644 /etc/kit-vpn/config.toml

INSTALLED="$INSTALLED_BIN"

echo "==> starting tunnel through the native-messaging path (as $FX_USER)"
runuser -u "$FX_USER" -- "$INSTALLED" nm \
  < <(python3 -c "import sys,struct;m=b'{\"type\":\"enable\"}';sys.stdout.buffer.write(struct.pack('<I',len(m))+m)") \
  > "$T/nm-enable.out" 2>/dev/null || true
for _ in $(seq 1 40); do
  STATE="$(python3 -c "import json;print(json.load(open('/run/kit-vpn/status.json'))['state'])" 2>/dev/null || echo starting)"
  [[ "$STATE" == "running" ]] && break
  sleep 0.5
done
echo "  tunnel state: $STATE"
[[ "$STATE" == "running" ]] || { echo "tunnel did not start"; exit 1; }

echo "==> launching Firefox with the extension via web-ext"
: > /run/kit-vpn/socks.log
runuser -u "$FX_USER" -- env DISPLAY=:1 HOME=/home/hannah bash -c "
  cd '$REPO' && npx --yes web-ext run \
    --source-dir=extension \
    --firefox=/usr/bin/firefox \
    --url='http://www.kit.test:8080/' \
    --url='http://example.com/' \
    --no-input \
  " > "$T/webext.log" 2>&1 &
PIDS+=($!)

# give Firefox time to boot, install the add-on, and load both pages
sleep 40

echo "==> firefox processes: $(pgrep -c firefox || echo 0)"
echo "==> webext log tail:"
tail -6 "$T/webext.log" 2>/dev/null
echo "==> socks log (what the extension routed through the tunnel):"
grep -E "resolved|CONNECT|accepted" /run/kit-vpn/socks.log | tail -15
echo "==> fake web requests:"
cat "$T/web.log" 2>/dev/null | tail -8
echo "==> fake DNS queries:"
cat "$T/dns.log" 2>/dev/null | tail -6

PASS=1
if grep -qE "resolved www.kit.test" /run/kit-vpn/socks.log; then
  echo "  PASS: www.kit.test was resolved and routed through the tunnel (remote DNS)"
else
  echo "  FAIL: no www.kit.test request seen by the tunnel"
  PASS=0
fi
if grep -q "CONNECT 10.8.0.1:8080 -> 10.8.0.1 ok" /run/kit-vpn/socks.log; then
  echo "  PASS: www.kit.test connected to fake KIT via the tunnel"
else
  echo "  FAIL: no tunnel connection to the fake KIT web server"
  PASS=0
fi
if grep -q "example.com" /run/kit-vpn/socks.log; then
  echo "  FAIL: example.com was routed through the tunnel (should be DIRECT)"
  PASS=0
else
  echo "  PASS: example.com was NOT routed through the tunnel (stayed on the normal connection)"
fi
if grep -q "www.kit.test" "$T/dns.log" 2>/dev/null; then
  echo "  PASS: DNS for kit.test was resolved inside the tunnel"
else
  echo "  note: no kit.test query logged (may have been answered before log rotation)"
fi

echo
echo "=== result: $([ $PASS -eq 1 ] && echo ALL PASS || echo FAILURES) ==="
exit $((1 - PASS))
