//! Paths and configuration for the KIT VPN companion.
//!
//! The root-owned config lives in /etc/kit-vpn/config.toml and is written by
//! `scripts/install.sh` (or `kit-vpn-companion config --ovpn <file>`). The
//! OpenVPN file contains private keys, so it stays root-owned and is never
//! read by the unprivileged Native-Messaging process.

use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/kit-vpn/config.toml";
pub const DEFAULT_OVPN_PATH: &str = "/etc/kit-vpn/kit.ovpn";

pub const RUN_DIR: &str = "/run/kit-vpn";
pub const STATUS_PATH: &str = "/run/kit-vpn/status.json";
pub const PID_PATH: &str = "/run/kit-vpn/supervisor.pid";
pub const LOCK_PATH: &str = "/run/kit-vpn/lock";

/// Network namespace + veth names. A fixed private namespace per host; all
/// state is torn down on disable.
pub const NETNS: &str = "kitvpn";
pub const VETH_HOST: &str = "kitvpn0";
pub const VETH_NS: &str = "kitvpn1";
/// OpenVPN creates this tun device on the host; the supervisor moves it into
/// the namespace once it is configured.
pub const TUN_DEV: &str = "kitvpn-tun";

pub const DEFAULT_SUBNET: &str = "10.200.200.0/30";
pub const DEFAULT_SOCKS_PORT: u16 = 1080;
/// KIT's DNS servers: the current pushed one plus the long-standing SCC
/// resolvers (order matters — first is tried first).
pub const DEFAULT_DNS: [&str; 3] = ["141.3.175.71", "129.13.10.90", "129.13.10.91"];
pub const DEFAULT_AUTH_FILE: &str = "/etc/kit-vpn/auth.txt";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ovpn_path: Option<String>,
    pub socks_port: Option<u16>,
    pub dns_servers: Option<Vec<String>>,
    pub subnet: Option<String>,
    /// Path to a root-only credentials file (username on line 1, password on
    /// line 2) for `auth-user-pass` configs. Optional: KIT's standard configs
    /// authenticate with embedded client certificates and do not need this.
    pub auth_user_pass: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ovpn_path: Some(DEFAULT_OVPN_PATH.to_string()),
            socks_port: Some(DEFAULT_SOCKS_PORT),
            dns_servers: Some(DEFAULT_DNS.iter().map(|s| s.to_string()).collect()),
            subnet: Some(DEFAULT_SUBNET.to_string()),
            auth_user_pass: None,
        }
    }
}

impl Config {
    pub fn dns(&self) -> Vec<String> {
        self.dns_servers.clone().unwrap_or_else(|| {
            DEFAULT_DNS.iter().map(|s| s.to_string()).collect()
        })
    }

    pub fn socks_port(&self) -> u16 {
        self.socks_port.unwrap_or(DEFAULT_SOCKS_PORT)
    }
}

pub fn load() -> Result<Config, String> {
    let text = std::fs::read_to_string(DEFAULT_CONFIG_PATH)
        .map_err(|e| format!("cannot read {}: {}", DEFAULT_CONFIG_PATH, e))?;
    let cfg: Config =
        toml::from_str(&text).map_err(|e| format!("invalid {}: {}", DEFAULT_CONFIG_PATH, e))?;
    Ok(cfg)
}

/// Extract `remote host [port] [proto]` lines from an OpenVPN config.
/// This includes `remote` lines inside `<connection>` blocks, which look the
/// same textually.
pub fn parse_remotes(ovpn: &str) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    for line in ovpn.lines() {
        let t = line.trim();
        if !t.starts_with("remote ") {
            continue;
        }
        let mut parts = t.split_whitespace();
        parts.next(); // "remote"
        let host = match parts.next() {
            Some(h) if !h.is_empty() && !h.starts_with('#') => h,
            _ => continue,
        };
        let port = parts
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(1194);
        out.push((host.to_string(), port));
    }
    out
}

/// Extract `dhcp-option DNS x.x.x.x` lines (config-file variants).
pub fn parse_dhcp_dns(ovpn: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in ovpn.lines() {
        let mut it = line.trim().split_whitespace();
        if it.next() == Some("dhcp-option") && it.next() == Some("DNS") {
            if let Some(d) = it.next() {
                out.push(d.to_string());
            }
        }
    }
    out
}

/// Installer mode: copy a user-supplied .ovpn into the root-owned location
/// and (re)write the config file. `auth` is an optional 2-line credentials
/// file (username, password) for `auth-user-pass` configs. Only used by the
/// installer, not by Native Messaging.
pub fn install_ovpn(src: &str, auth: Option<&str>) -> Result<String, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("cannot stat {}: {}", src, e))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", src));
    }
    let text =
        std::fs::read_to_string(src).map_err(|e| format!("cannot read {}: {}", src, e))?;
    let looks_like_ovpn = text.contains("remote ")
        || text.contains("<ca>")
        || text.trim_start().starts_with("client");
    if !looks_like_ovpn {
        return Err(format!(
            "{} does not look like an OpenVPN configuration (no 'remote'/'client'/'<ca>')",
            src
        ));
    }

    std::fs::create_dir_all("/etc/kit-vpn")
        .map_err(|e| format!("cannot create /etc/kit-vpn: {}", e))?;
    std::fs::write(DEFAULT_OVPN_PATH, &text)
        .map_err(|e| format!("cannot write {}: {}", DEFAULT_OVPN_PATH, e))?;
    #[allow(clippy::permissions_set_readonly_false)]
    std::fs::set_permissions(
        DEFAULT_OVPN_PATH,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .ok();

    // optional credentials file for auth-user-pass configs
    if let Some(auth_src) = auth {
        let auth_text = std::fs::read_to_string(auth_src)
            .map_err(|e| format!("cannot read {}: {}", auth_src, e))?;
        if auth_text.lines().count() < 2 {
            return Err(format!(
                "{} should contain the username on line 1 and password on line 2",
                auth_src
            ));
        }
        std::fs::write(DEFAULT_AUTH_FILE, &auth_text)
            .map_err(|e| format!("cannot write {}: {}", DEFAULT_AUTH_FILE, e))?;
        #[allow(clippy::permissions_set_readonly_false)]
        std::fs::set_permissions(
            DEFAULT_AUTH_FILE,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .ok();
    }

    let dns = {
        let parsed = parse_dhcp_dns(&text);
        if parsed.is_empty() {
            DEFAULT_DNS.iter().map(|s| s.to_string()).collect()
        } else {
            parsed
        }
    };
    let dns_toml = dns
        .iter()
        .map(|d| format!("\"{}\"", d))
        .collect::<Vec<_>>()
        .join(", ");
    let auth_line = match auth {
        Some(_) => format!("\nauth_user_pass = \"{}\"", DEFAULT_AUTH_FILE),
        None => String::new(),
    };
    let cfg_toml = format!(
        "# /etc/kit-vpn/config.toml — written by kit-vpn-companion config\n\
         ovpn_path = \"{}\"\n\
         socks_port = {}\n\
         dns_servers = [{}]\n\
         subnet = \"{}\"{}\n",
        DEFAULT_OVPN_PATH, DEFAULT_SOCKS_PORT, dns_toml, DEFAULT_SUBNET, auth_line
    );
    std::fs::write(DEFAULT_CONFIG_PATH, &cfg_toml)
        .map_err(|e| format!("cannot write {}: {}", DEFAULT_CONFIG_PATH, e))?;
    std::fs::set_permissions(
        DEFAULT_CONFIG_PATH,
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .ok();

    let mut msg = format!("installed {} -> {}", src, DEFAULT_OVPN_PATH);
    if let Some(_a) = auth {
        msg.push_str(&format!(" (+ credentials -> {})", DEFAULT_AUTH_FILE));
    }
    msg.push_str(&format!(" (DNS: {})", dns.join(", ")));
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remotes() {
        let ovpn = "\
client
dev tun
remote vpn.scc.kit.edu 1194 udp
<connection>
remote vpn2.scc.kit.edu 443 tcp
</connection>
remote-random
";
        let r = parse_remotes(ovpn);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], ("vpn.scc.kit.edu".to_string(), 1194));
        assert_eq!(r[1], ("vpn2.scc.kit.edu".to_string(), 443));
    }

    #[test]
    fn parses_dhcp_dns() {
        let ovpn = "dhcp-option DNS 129.13.10.90\ndhcp-option DNS 129.13.10.91\n";
        assert_eq!(parse_dhcp_dns(ovpn), vec!["129.13.10.90", "129.13.10.91"]);
    }

    #[test]
    fn default_dns_fallback() {
        let ovpn = "client\n";
        assert!(parse_dhcp_dns(ovpn).is_empty());
    }
}
