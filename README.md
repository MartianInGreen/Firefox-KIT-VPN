# KIT VPN — per-site OpenVPN for Firefox

A deliberately small project: a **Firefox extension** that routes only
configured `*.kit.edu` domains through an **isolated KIT OpenVPN tunnel**,
while everything else stays on your normal connection (existing system VPN
included).

```
Firefox request
       |
       +-- non-KIT domain --> DIRECT (normal system connection)
       |
       +-- KIT domain
               |
               v
        Firefox proxy API (proxy.onRequest)
               |
               v
        127.0.0.1:1080 (SOCKS5, remote DNS)
               |
               v
        local companion (Rust)
               |
               v
        isolated network namespace
               |
               v
        OpenVPN tun device  ->  KIT
```

No global/system-wide VPN is created, the host's default route is never
touched, and requests assigned to the tunnel **fail closed** when the tunnel
is down (no silent fallback).

---

## Components

| Path               | What it is |
|--------------------|------------|
| `extension/`       | Firefox WebExtension (MV2, proxy API + Native Messaging) |
| `companion/`       | Rust binary: netns/OpenVPN/SOCKS5 supervisor + Native Messaging process |
| `scripts/install.sh` | one-shot installer (binary, config, sudoers rule, native manifest) |
| `tests/`           | e2e test harness with a *fake KIT* behind a real OpenVPN tunnel |

## How it works (the important part)

The KIT OpenVPN instance runs in a private network namespace (`kitvpn`):

* **OpenVPN runs on the host.** Its encrypted control connection uses the
  host's normal routing, so it coexists with any existing system VPN. To
  stop KIT seeing your connection arrive from another VPN provider (e.g. a
  Tailscale exit node), the companion adds destination-based policy rules
  at enable time that route the KIT OpenVPN server addresses (the `remote`
  lines of your `.ovpn`) directly through the host's `main` table — your
  normal ISP connection — bypassing the other VPN. The rules are removed on
  disable; only those server IPs are affected.
* **The `tun` device is moved into the namespace** the moment OpenVPN
  creates it (`--route-noexec` guarantees the host never gains routes
  through the tunnel). All tunnel *data* therefore lives inside the
  namespace; nothing else on the host can enter it.
* **A SOCKS5 server runs inside the namespace** (bound to the private veth
  address `10.200.200.2`, reachable only via the veth). A tiny relay on the
  host exposes it on `127.0.0.1:1080` — the *only* listener Firefox ever
  talks to.
* **Remote DNS**: Firefox sends hostnames (proxyDNS), the in-namespace SOCKS
  server resolves them through the tunnel via the configured DNS servers
  (default KIT: `129.13.10.90/91`, or `dhcp-option DNS` from your .ovpn).
  KIT-internal names never touch your host resolver.
* **Fail closed**: the namespace has no default route except via `tun`. If
  the tunnel drops, the route disappears and SOCKS connects fail — Firefox
  requests for KIT domains fail instead of leaking onto the host network.
* **Teardown** removes the namespace (and with it the veth pair) completely.

Host-side footprint while enabled: the connected `/30` route for the veth
and the SOCKS relay socket on `127.0.0.1`. Nothing else.

## Install

Prerequisites (Linux):

* Firefox (any recent version)
* `openvpn`, `iproute2` (`ip`), `sudo`
* Rust (only to build the companion; the installer runs `cargo build`)

Get your KIT `.ovpn` file (from the KIT/SCC VPN portal), then:

```bash
git clone <this-repo> && cd Firefox-KIT-VPN
bash scripts/install.sh /path/to/kit.ovpn
```

> **Username/password?** KIT's standard VPN configs authenticate with **client
> certificates embedded in the `.ovpn`** (`<ca>`, `<cert>`, `<key>`) — there is
> no username/password prompt. If your setup instead uses `auth-user-pass`,
> pass a second file with the username on line 1 and the password on line 2:
>
> ```bash
> bash scripts/install.sh /path/to/kit.ovpn /path/to/auth.txt
> ```
>
> The credentials are copied to `/etc/kit-vpn/auth.txt` (root-only, `0600`) and
> handed to OpenVPN via `--auth-user-pass`; they are never sent to the
> extension and never appear in Native Messaging.

This will (each `sudo` asks once):

1. Build and install the companion to `/usr/local/lib/kit-vpn/kit-vpn-companion`
2. Copy your `.ovpn` to `/etc/kit-vpn/kit.ovpn` (root-only) and write
   `/etc/kit-vpn/config.toml`
3. Add a `sudoers.d` rule granting **your user** passwordless access to
   exactly two commands: `kit-vpn-companion helper enable|disable`
   (nothing else)
4. Install the Native Messaging manifest to
   `~/.mozilla/native-messaging-hosts/kit_vpn_companion.json`

Then load the extension:

1. Firefox → `about:debugging` → *This Firefox* → *Load Temporary Add-on*
2. Select `extension/manifest.json` (id `kit-vpn@kit-vpn.local`)

The popup shows the tunnel state. KIT routing is on by default; toggle it
off to leave Firefox completely untouched.

## Configuration

`/etc/kit-vpn/config.toml` (root-owned):

```toml
ovpn_path      = "/etc/kit-vpn/kit.ovpn"
socks_port     = 1080              # 127.0.0.1 port (auto-scans upward if busy)
dns_servers    = ["129.13.10.90", "129.13.10.91"]   # tunnel DNS
subnet         = "10.200.200.0/30" # private veth subnet (auto-picks a free one)
auth_user_pass = "/etc/kit-vpn/auth.txt"   # optional: username/password file
```

Domains are configured in the extension popup (default `*.kit.edu`, which
also matches `kit.edu` itself; add e.g. `*.scc.kit.edu` as needed — anything
ending in `.kit.edu` is already covered).

## Security notes

* The SOCKS listener exposed to Firefox binds **only** to `127.0.0.1`.
* The in-namespace SOCKS server binds a private veth address that is not
  reachable from any other interface.
* No shell is ever used; the helper only ever executes `openvpn`, `ip`,
  `sysctl` with fixed argument lists, and Native Messaging messages are
  limited to `status` / `enable` / `disable` — no paths, no commands.
* The `.ovpn` (which contains private keys) stays root-owned, `0600`.
* If the tunnel disconnects, tunneled requests fail; they never fall back
  to the normal connection.

## Testing

The test suite builds a **fake KIT** (a second namespace with a real
OpenVPN server in static-key mode, a fake web server and a fake DNS that
answers `*.kit.test`) and verifies the whole stack:

```bash
sudo bash tests/e2e.sh          # full e2e: isolation, routing, DNS, fail-closed, NM/sudo path
sudo bash tests/firefoxtest.sh  # real Firefox (web-ext) routing through the tunnel
node tests/domainmatch.test.js  # extension domain-matching unit tests
cd companion && cargo test      # companion unit tests
```

The e2e asserts, among other things: the host default route and policy
rules are untouched, an existing system VPN (the test machine runs
Tailscale) is unaffected, `*.kit.test` reaches the fake KIT via the tunnel
with DNS resolved *inside* the tunnel, `example.com` stays direct, LAN
addresses are unreachable through the proxy, and everything fails closed
when the tunnel is killed.

## Troubleshooting

| Symptom | Check |
|---|---|
| Popup shows `companion unavailable` | the native manifest or sudoers rule is missing → re-run `scripts/install.sh`; the extension must be loaded with id `kit-vpn@kit-vpn.local` |
| Popup shows `error` | `tail /run/kit-vpn/helper.log` and `/run/kit-vpn/openvpn.log` (root). If the log mentions auth failure, your `.ovpn` needs credentials → re-run `install.sh` with an `auth.txt` (username line 1, password line 2) |
| Tunnel up but KIT sites fail | your `.ovpn` needs interactive auth (`auth-user-pass`)? cert-based KIT configs are fine |
| KIT internal names don't resolve | ensure `dns_servers` in `/etc/kit-vpn/config.toml` are the KIT DNS servers (or add `dhcp-option DNS` lines to your .ovpn) |

Runtime state lives under `/run/kit-vpn/` (status, supervisor/helper logs,
openvpn logs). The supervisor daemon can be stopped at any time with
`sudo /usr/local/lib/kit-vpn/kit-vpn-companion helper disable` (or via the
extension toggle).

## Limitations

* IPv4 only (the tunnel is IPv4; IPv6 KIT traffic is not routed through it).
* The OpenVPN control connection (the encrypted link to `vpn.scc.kit.edu`
  itself) uses the host's normal routing by design.
* One KIT tunnel at a time; one Firefox user per machine.

This is **not** a generic VPN manager — it is specifically a Firefox
extension that sends configured KIT domains through one isolated OpenVPN
connection while leaving everything else untouched.
