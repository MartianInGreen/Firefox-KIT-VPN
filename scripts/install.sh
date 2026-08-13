#!/usr/bin/env bash
#
# Install the KIT VPN companion.
#
#   bash scripts/install.sh /path/to/kit.ovpn [auth.txt]
#
#   auth.txt (optional): two lines — username, password — used only when the
#   .ovpn requires `auth-user-pass`. KIT's standard configs authenticate with
#   embedded client certificates and do NOT need this.
#
# Steps (all sudo steps ask once):
#   1. cargo build --release (companion/)
#   2. install the binary to /usr/local/lib/kit-vpn/kit-vpn-companion
#   3. copy the KIT .ovpn to /etc/kit-vpn/kit.ovpn (root-only)
#   4. write /etc/kit-vpn/config.toml
#   5. add a sudoers rule granting YOUR user passwordless access to exactly:
#        <binary> helper enable
#        <binary> helper disable
#   6. install the Native Messaging manifest for your Firefox profile
#
# Then load the extension from extension/ via about:debugging
# (Load Temporary Add-on -> extension/manifest.json).
#
set -euo pipefail

OVPN="${1:-}"
AUTH="${2:-}"
if [[ -z "$OVPN" ]]; then
  echo "usage: bash scripts/install.sh /path/to/kit.ovpn [auth.txt]" >&2
  exit 2
fi
if [[ ! -f "$OVPN" ]]; then
  echo "error: $OVPN not found" >&2
  exit 1
fi
if [[ -n "$AUTH" && ! -f "$AUTH" ]]; then
  echo "error: $AUTH not found" >&2
  exit 1
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="/usr/local/lib/kit-vpn"
# who the sudoers rule is granted to: explicit override, then the invoking
# user, then the current user. Never "root" from a nested-sudo context.
USER_NAME="${KITVPN_USER:-${SUDO_USER:-$(whoami)}}"
NM_HOSTS_DIR="${HOME}/.mozilla/native-messaging-hosts"
NM_NAME="kit_vpn_companion"

echo "==> building companion (release)"
cargo build --release --manifest-path "$REPO/companion/Cargo.toml"

echo "==> installing binary"
sudo install -d -m 0755 "$PREFIX"
sudo install -m 0755 "$REPO/companion/target/release/kit-vpn-companion" "$PREFIX/kit-vpn-companion"

echo "==> copying OpenVPN config"
sudo install -d -m 0755 /etc/kit-vpn
sudo install -m 0600 "$OVPN" /etc/kit-vpn/kit.ovpn
if [[ -n "$AUTH" ]]; then
  echo "==> copying credentials file (root-only)"
  sudo install -m 0600 "$AUTH" /etc/kit-vpn/auth.txt
  sudo "$PREFIX/kit-vpn-companion" config --ovpn /etc/kit-vpn/kit.ovpn --auth-user-pass /etc/kit-vpn/auth.txt
else
  sudo "$PREFIX/kit-vpn-companion" config --ovpn /etc/kit-vpn/kit.ovpn
fi

echo "==> granting passwordless sudo for the helper (enable/disable only)"
sudo tee "/etc/sudoers.d/kit-vpn" >/dev/null <<EOF
# Allow the KIT VPN extension to start/stop its isolated tunnel.
${USER_NAME} ALL=(root) NOPASSWD: ${PREFIX}/kit-vpn-companion helper enable
${USER_NAME} ALL=(root) NOPASSWD: ${PREFIX}/kit-vpn-companion helper disable
EOF
sudo chmod 0440 /etc/sudoers.d/kit-vpn

echo "==> installing Native Messaging manifest for ${USER_NAME}"
mkdir -p "$NM_HOSTS_DIR"
sed "s|__INSTALL_PATH__|${PREFIX}/kit-vpn-companion|" "$REPO/native/kit_vpn_companion.json.in" \
  > "$NM_HOSTS_DIR/${NM_NAME}.json"
chmod 0600 "$NM_HOSTS_DIR/${NM_NAME}.json"

echo
echo "Install complete."
echo "Next steps:"
echo "  1. Open Firefox -> about:debugging -> This Firefox -> Load Temporary Add-on"
echo "     and select extension/manifest.json (id: kit-vpn@kit-vpn.local)"
echo "  2. The popup should show 'running' once the tunnel connects."
echo "  3. Verify: curl --socks5-hostname 127.0.0.1:1080 http://www.kit.edu/"
echo
echo "Logs: /run/kit-vpn/*.log   Status: /run/kit-vpn/status.json"
