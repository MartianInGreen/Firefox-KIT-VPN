//! Root helper operations (`kit-vpn-companion helper enable|disable`).
//!
//! The sudoers rule installed by `scripts/install.sh` whitelists exactly
//! these two invocations, so Native Messaging can never pass arbitrary
//! arguments. All arguments/values come from the root-owned config in
//! /etc/kit-vpn, never from the extension.

use std::os::unix::io::AsRawFd;

use crate::config;
use crate::netns;
use crate::util::{log, log_init, read_status, Status};

pub fn enable() -> Result<String, String> {
    let _ = std::fs::create_dir_all(config::RUN_DIR);
    let _ = std::fs::set_permissions(
        config::RUN_DIR,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    );
    log_init("/run/kit-vpn/helper.log");
    log("helper enable: start");

    let cfg = config::load()?;
    let ovpn_path = cfg
        .ovpn_path
        .clone()
        .unwrap_or_else(|| config::DEFAULT_OVPN_PATH.to_string());
    if !std::path::Path::new(&ovpn_path).is_file() {
        return Err(format!(
            "OpenVPN configuration not found at {} — run scripts/install.sh with your KIT .ovpn file",
            ovpn_path
        ));
    }

    // ---- already running? don't touch anything, just report state ----
    // A supervisor counts as "running" only while its status is one of the
    // healthy states. If it is alive but stuck (e.g. it never finished a
    // previous shutdown), force-kill it so a fresh tunnel can come up.
    if let Ok(pid_str) = std::fs::read_to_string(config::PID_PATH) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
                let st = read_status();
                let healthy = st
                    .as_ref()
                    .map(|s| {
                        matches!(
                            s.state.as_str(),
                            "running" | "starting" | "reconnecting"
                        )
                    })
                    .unwrap_or(false);
                if healthy {
                    return Ok(st.unwrap().json());
                }
                log(&format!(
                    "supervisor pid {} alive but not healthy — force-killing",
                    pid
                ));
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    Some(nix::sys::signal::Signal::SIGKILL),
                );
                for _ in 0..50 {
                    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let _ = std::fs::remove_file(config::PID_PATH);
            }
        }
    }

    // ---- exclusive lock held by the supervisor for its lifetime ----
    // Raw flock, never explicitly unlocked: the lock lives while any process
    // holds the fd, so the daemonized supervisor keeps it and a second
    // `enable` (e.g. from a Firefox/extension reload) cannot tear the tunnel
    // down — it just gets the current status.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(config::LOCK_PATH)
        .map_err(|e| format!("cannot open lock file: {}", e))?;
    if unsafe { nix::libc::flock(lock.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } != 0 {
        // the lock is held: by a healthy tunnel (report it) or by a stuck
        // supervisor (wait briefly for it to go away, then try again)
        for _ in 0..30 {
            let st = read_status();
            let healthy = st
                .as_ref()
                .map(|s| matches!(s.state.as_str(), "running" | "starting" | "reconnecting"))
                .unwrap_or(false);
            if healthy {
                return Ok(st.unwrap().json());
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if let Some(st) = read_status() {
            return Ok(st.json());
        }
        return Err("another enable is in progress".to_string());
    }
    let _lock_fd = lock; // keep the fd open; the forked supervisor inherits it

    // ---- choose subnet & set up the namespace ----
    let subnet = cfg.subnet.clone().unwrap_or_else(|| config::DEFAULT_SUBNET.to_string());
    let (host_ip, ns_ip, prefix) = netns::pick_subnet(&subnet)?;
    netns::setup(&host_ip, &ns_ip, prefix)?;

    // ---- route the OpenVPN control channel around any other host VPN ----
    // (e.g. a Tailscale exit node), so KIT sees a direct (ISP) connection.
    let ovpn_text = std::fs::read_to_string(&ovpn_path).unwrap_or_default();
    let direct_ips = netns::remote_v4_ips(&ovpn_text);
    netns::add_direct_rules(&direct_ips);
    log(&format!("direct-route rules for KIT servers: {:?}", direct_ips));

    // ---- daemonize; the child becomes the supervisor ----
    match unsafe { nix::unistd::fork() }.map_err(|e| format!("fork: {}", e))? {
        nix::unistd::ForkResult::Parent { .. } => {
            log("helper enable: daemonized, returning");
            Ok(Status::new("starting", "tunnel starting", cfg.socks_port()).json())
        }
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::setsid();
            if let Ok(devnull) = std::fs::File::open("/dev/null") {
                let fd = devnull.as_raw_fd();
                let _ = nix::unistd::dup2(fd, 0);
                let _ = nix::unistd::dup2(fd, 1);
                let _ = nix::unistd::dup2(fd, 2);
            }
            let _ = std::env::set_current_dir("/");
            let port = cfg.socks_port();
            crate::supervisor::run(cfg, host_ip, ns_ip, prefix, port);
            std::process::exit(0);
        }
    }
}

pub fn disable() -> Result<String, String> {
    let _ = std::fs::create_dir_all(config::RUN_DIR);
    let _ = std::fs::set_permissions(
        config::RUN_DIR,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    );
    log_init("/run/kit-vpn/helper.log");
    log("helper disable: start");

    if let Ok(pid_str) = std::fs::read_to_string(config::PID_PATH) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            let pid = nix::unistd::Pid::from_raw(pid);
            let _ = nix::sys::signal::kill(pid, Some(nix::sys::signal::Signal::SIGTERM));
            for _ in 0..100 {
                if nix::sys::signal::kill(pid, None).is_err() {
                    break; // gone
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    // Even if the supervisor died uncleanly, remove any stale state.
    let _ = netns::cleanup_netns();
    let _ = std::fs::remove_file(config::PID_PATH);

    // drop the direct-routing rules for the KIT VPN servers again
    let ovpn_path = config::DEFAULT_OVPN_PATH;
    if let Ok(ovpn_text) = std::fs::read_to_string(ovpn_path) {
        let direct_ips = netns::remote_v4_ips(&ovpn_text);
        netns::remove_direct_rules(&direct_ips);
    }

    let st = Status::new("stopped", "tunnel stopped", 0);
    write_status_or_keep(&st);
    log("helper disable: done");
    Ok(st.json())
}

fn write_status_or_keep(st: &Status) {
    // Only write "stopped" if the file exists or we just cleaned up; keep it
    // simple and always write it.
    crate::util::write_status(st);
}
