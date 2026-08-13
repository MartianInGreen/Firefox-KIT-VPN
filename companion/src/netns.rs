//! Network namespace / veth / tun handling, all performed by the root
//! helper and fully automated.
//!
//! Topology:
//!
//!   Firefox -> 127.0.0.1:port (relay, host) -> kitvpn0/10.200.200.1
//!          -> kitvpn1/10.200.200.2 (in netns `kitvpn`) -> SOCKS5 server
//!          -> kitvpn-tun (OpenVPN device, moved into the netns) -> KIT
//!
//! OpenVPN itself runs on the host: its encrypted control connection uses
//! the host's normal routing (so it coexists with any existing system VPN
//! and needs no host forwarding or firewall rules). The tun device it
//! creates is moved into the namespace as soon as it is configured, so all
//! tunnel *data* stays inside the namespace. `--route-noexec` ensures the
//! host never gains routes through the tunnel.
//!
//! Inside the namespace the default route points at the tun device; when it
//! disappears the namespace has no route at all, so tunneled connections
//! fail closed. The host's routing table is only touched by the connected
//! /30 route for kitvpn0 (used by the relay to reach the SOCKS server).

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::config;
use crate::util::{ip, log, ns_ip, run_cmd};

/// Delete the namespace (and with it the veth pair). Idempotent.
pub fn cleanup_netns() -> Result<(), String> {
    let _ = ip(&["netns", "del", config::NETNS]);
    Ok(())
}

pub fn setup(host_addr: &str, ns_addr: &str, prefix: u8) -> Result<(), String> {
    let p = prefix.to_string();
    let host_cidr = format!("{}/{}", host_addr, p);
    let ns_cidr = format!("{}/{}", ns_addr, p);

    let r = (|| -> Result<(), String> {
        ip(&["netns", "add", config::NETNS])?;
        ip(&[
            "link",
            "add",
            config::VETH_HOST,
            "type",
            "veth",
            "peer",
            "name",
            config::VETH_NS,
        ])?;
        ip(&["link", "set", config::VETH_NS, "netns", config::NETNS])?;
        ip(&["addr", "add", host_cidr.as_str(), "dev", config::VETH_HOST])?;
        ip(&["link", "set", config::VETH_HOST, "up"])?;
        ns_ip(&["link", "set", "lo", "up"])?;
        ns_ip(&["addr", "add", ns_cidr.as_str(), "dev", config::VETH_NS])?;
        ns_ip(&["link", "set", config::VETH_NS, "up"])?;
        Ok(())
    })();

    if let Err(e) = r {
        let _ = cleanup_netns();
        return Err(e);
    }
    Ok(())
}

/// Choose a free /30 subnet. Host side = base+1, namespace side = base+2.
/// Tries the configured subnet first, then scans 10.200.x.0/30.
pub fn pick_subnet(configured: &str) -> Result<(String, String, u8), String> {
    let (net, prefix) = configured
        .split_once('/')
        .ok_or_else(|| "subnet must be a.b.c.d/30".to_string())?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|_| format!("bad subnet prefix in {}", configured))?;
    if prefix < 30 {
        return Err(format!("subnet prefix must be >= 30 (got {})", prefix));
    }
    let octets: Vec<u8> = net
        .split('.')
        .map(|o| o.parse::<u8>().map_err(|_| format!("bad subnet {}", configured)))
        .collect::<Result<_, _>>()?;
    if octets.len() != 4 {
        return Err(format!("bad subnet {}", configured));
    }
    let base = [octets[0], octets[1], octets[2], octets[3]];

    let mut tries: Vec<[u8; 4]> = vec![base];
    for x in 200..=255 {
        tries.push([10, 200, x, 0]);
    }
    for x in 0..200 {
        tries.push([10, 200, x, 0]);
    }

    for t in tries {
        if t[3] > 253 {
            continue;
        }
        let host_ip = format!("{}.{}.{}.{}", t[0], t[1], t[2], t[3] + 1);
        let ns_ip = format!("{}.{}.{}.{}", t[0], t[1], t[2], t[3] + 2);
        if !host_ip_in_use(&host_ip) {
            return Ok((host_ip, ns_ip, prefix));
        }
    }
    Err("no free subnet found (10.200.0.0/16 busy?)".to_string())
}

fn host_ip_in_use(addr: &str) -> bool {
    let probe = format!("{}/32", addr);
    match ip(&["-4", "addr", "show", "to", probe.as_str()]) {
        Ok(out) => !out.trim().is_empty(),
        Err(_) => true,
    }
}

/// True when OpenVPN created kitvpn-tun on the host AND configured an IPv4
/// address on it (i.e. it is ready to be moved into the namespace).
pub fn host_tun_ready() -> bool {
    ip(&["-4", "addr", "show", "dev", config::TUN_DEV])
        .map(|out| out.contains("inet "))
        .unwrap_or(false)
}

/// Move the tun device into the namespace, carrying its address over if the
/// kernel does not do so automatically.
pub fn move_tun_to_netns() {
    // capture the address on the host first (defensive)
    let addr_line = ip(&["-4", "-o", "addr", "show", "dev", config::TUN_DEV]).ok();
    // a device from a previous OpenVPN run may linger in the namespace
    // (e.g. a DCO device or after an unclean exit); drop it so the name is
    // free for the fresh device
    let _ = ns_ip(&["link", "del", config::TUN_DEV]);
    if let Err(e) = ip(&["link", "set", config::TUN_DEV, "netns", config::NETNS]) {
        log(&format!("move_tun_to_netns: {}", e));
        return;
    }
    // remove any residual host routes via the device (insurance)
    let _ = ip(&["route", "flush", "dev", config::TUN_DEV]);
    // moving resets the device: addresses do not survive a move and the link
    // comes back down, so restore both inside the namespace
    if let Some(line) = addr_line {
        if let Some((local, peer)) = parse_addr_line(&line) {
            if !ns_tun_has_addr() {
                // `ip addr add LOCAL peer PEER dev DEV` — separate tokens
                let mut args: Vec<&str> = vec!["addr", "add", local.as_str()];
                if let Some(p) = peer.as_ref() {
                    args.push("peer");
                    args.push(p);
                }
                args.push("dev");
                args.push(config::TUN_DEV);
                if let Err(e) = ns_ip(&args) {
                    log(&format!("move_tun_to_netns: re-add addr failed: {}", e));
                }
            }
        }
    }
    if let Err(e) = ns_ip(&["link", "set", config::TUN_DEV, "up"]) {
        log(&format!("move_tun_to_netns: link up failed: {}", e));
    }
}

/// Returns (local_address_with_prefix, optional_peer_address).
/// e.g. "55: kitvpn-tun    inet 10.8.0.2 peer 10.8.0.1/32 scope global"
///  -> ("10.8.0.2/32", Some("10.8.0.1"))
/// or   "55: kitvpn-tun    inet 10.8.0.2/24 brd ..."
///  -> ("10.8.0.2/24", None)
fn parse_addr_line(line: &str) -> Option<(String, Option<String>)> {
    let t: Vec<&str> = line.split_whitespace().collect();
    let idx = t.iter().position(|s| *s == "inet")?;
    let addr = t.get(idx + 1)?.to_string();
    let local = if addr.contains('/') {
        addr
    } else {
        format!("{}/32", addr) // point-to-point tun: local side is /32
    };
    let peer = if t.get(idx + 2) == Some(&"peer") {
        t.get(idx + 3)
            .map(|p| p.split('/').next().unwrap_or("").to_string())
    } else {
        None
    };
    Some((local, peer))
}

fn ns_tun_has_addr() -> bool {
    ns_ip(&["-4", "addr", "show", "dev", config::TUN_DEV])
        .map(|out| out.contains("inet "))
        .unwrap_or(false)
}

/// True when the tun device is present and up inside the namespace.
pub fn ns_tun_ready() -> bool {
    ns_ip(&["link", "show", config::TUN_DEV])
        .map(|out| out.contains("LOWER_UP"))
        .unwrap_or(false)
}

/// Make the namespace's default route point at the tunnel.
pub fn ensure_ns_tun_default() -> Result<(), String> {
    ns_ip(&["route", "replace", "default", "dev", config::TUN_DEV]).map(|_| ())
}

/// Resolve a hostname (or literal IPv4) to one IPv4 address using the host's
/// resolver. Returns None for IPv6-only or unresolvable names.
pub fn host_to_ip(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return Some(ip.to_string());
    }
    if host.contains(':') {
        return None; // IPv6 literal — not supported
    }
    let out = run_cmd("getent", &["ahostsv4", host]).ok()?;
    let first = out.lines().next()?.split_whitespace().next()?.to_string();
    Some(first)
}

/// Extract the IPv4 OpenVPN server addresses from the .ovpn, so the control
/// connection can be routed around any other VPN the host may run.
pub fn remote_v4_ips(ovpn: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (host, _port) in crate::config::parse_remotes(ovpn) {
        if let Some(ip) = host_to_ip(&host) {
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Route the KIT VPN control connection DIRECTLY (bypassing any other VPN on
/// the host, e.g. a Tailscale exit node). We add a destination-based policy
/// rule at a higher priority than typical VPN rules, consulting the host's
/// `main` table (whose default is the normal/ISP route). Only the encrypted
/// control channel to the KIT servers is affected; nothing else changes.
pub fn add_direct_rules(ips: &[String]) {
    for server_ip in ips {
        let addr = format!("{}/32", server_ip);
        match ip(&["rule", "add", "pref", "5000", "to", addr.as_str(), "lookup", "main"]) {
            Ok(_) => log(&format!("direct rule added: 5000 -> {} (main)", server_ip)),
            Err(e) => log(&format!("direct rule add for {}: {}", server_ip, e)),
        }
    }
}

/// Remove the direct-routing rules added by `add_direct_rules`.
pub fn remove_direct_rules(ips: &[String]) {
    for server_ip in ips {
        let addr = format!("{}/32", server_ip);
        let _ = ip(&["rule", "del", "pref", "5000", "to", addr.as_str(), "lookup", "main"]);
    }
}

/// True while the namespace still exists (its bind mount is present).
pub fn ns_exists() -> bool {
    std::path::Path::new(&format!("/var/run/netns/{}", config::NETNS)).exists()
}

/// Spawn `argv[0]` with the given arguments *inside* the network namespace.
/// The child calls setns(CLONE_NEWNET) before exec.
pub fn spawn_in_netns(argv: &[&str]) -> Result<std::process::Child, String> {
    let ns_path = format!("/var/run/netns/{}", config::NETNS);
    let ns_file = std::fs::File::open(&ns_path)
        .map_err(|e| format!("cannot open {}: {}", ns_path, e))?;

    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            match nix::sched::setns(&ns_file, nix::sched::CloneFlags::CLONE_NEWNET) {
                Ok(()) => Ok(()),
                Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
            }
        });
    }
    cmd.spawn()
        .map_err(|e| format!("cannot spawn {}: {}", argv[0], e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_parse() {
        let (host, ns, prefix) = pick_subnet("10.200.200.0/30").unwrap();
        assert_eq!(prefix, 30);
        // host and ns must be consecutive addresses in the same /30
        let h: Vec<u8> = host.split('.').map(|o| o.parse().unwrap()).collect();
        let n: Vec<u8> = ns.split('.').map(|o| o.parse().unwrap()).collect();
        assert_eq!(h[0..3], n[0..3]);
        assert_eq!(h[3] + 1, n[3]);
    }
}
