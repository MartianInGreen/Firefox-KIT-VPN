#!/usr/bin/env bash
# Phase 1: bring up the fake KIT + tunnel (needs root). Fast.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLED=/usr/local/lib/kit-vpn/kit-vpn-companion
T=/tmp/kit-vpn-fxtest
SRVNS=kitsrv
FX_USER="${SUDO_USER:-$(whoami)}"

"$INSTALLED" helper disable >/dev/null 2>&1 || true
ip netns del "$SRVNS" 2>/dev/null || true
pkill -9 -f "fake_kit_(web|dns)" 2>/dev/null || true

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
for _ in $(seq 1 30); do
  ip netns exec "$SRVNS" ip addr show tun0 2>/dev/null | grep -q '10.8.0.1' && break
  sleep 0.5
done
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_web.py" 10.8.0.1 8080 "$T/web.log" &
ip netns exec "$SRVNS" python3 "$REPO/tests/fake_kit_dns.py" 10.8.0.1 53 10.8.0.1 "$T/dns.log" &
sleep 0.5
rm -f "$T/web.log" "$T/dns.log"

# start tunnel via the real NM path (as the user, sudo -n helper)
runuser -u "$FX_USER" -- "$INSTALLED" nm \
  < <(python3 -c "import sys,struct;m=b'{\"type\":\"enable\"}';sys.stdout.buffer.write(struct.pack('<I',len(m))+m)") \
  > "$T/nm-enable.out" 2>/dev/null || true
for _ in $(seq 1 40); do
  STATE="$(python3 -c "import json;print(json.load(open('/run/kit-vpn/status.json'))['state'])" 2>/dev/null || echo starting)"
  [[ "$STATE" == "running" ]] && break
  sleep 0.5
done
echo "tunnel state: $STATE"
: > /run/kit-vpn/socks.log
echo "fake KIT + tunnel ready"
