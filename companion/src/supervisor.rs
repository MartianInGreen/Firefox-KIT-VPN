//! Supervisor: daemonized after `helper enable`. Owns the OpenVPN and SOCKS5
//! children (both inside the namespace), the host relay, the default-route
//! switch-over when the tunnel comes up, reconnects, and fail-closed teardown.
//!
//! Status is written to /run/kit-vpn/status.json; the unprivileged
//! Native-Messaging process reads it for polling.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{self, Config};
use crate::netns;
use crate::relay;
use crate::util::{log, Status, write_status};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_OPENVPN_RESTARTS: u32 = 3;
const RESTART_DELAY: Duration = Duration::from_secs(10);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_: nix::libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    let action = nix::sys::signal::SigAction::new(
        nix::sys::signal::SigHandler::Handler(handle_sigterm),
        nix::sys::signal::SaFlags::SA_RESTART,
        nix::sys::signal::SigSet::empty(),
    );
    unsafe {
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action);
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGINT, &action);
    }
}

pub fn run(
    cfg: Config,
    _host_ip: String,
    _ns_ip: String,
    _prefix: u8,
    port: u16,
) {
    install_signal_handlers();

    let _ = std::fs::create_dir_all(config::RUN_DIR);
    let _ = std::fs::write(config::PID_PATH, format!("{}\n", std::process::id()));

    let mut status = Status::new("starting", "starting tunnel", port);
    write_status(&status);

    // ---- host relay on 127.0.0.1 (pick a free port) ----
    let socks_port = match relay::bind_free_port(port) {
        Some(p) => p,
        None => {
            status = Status::new("error", "cannot bind 127.0.0.1 proxy port", port);
            status.error = Some(format!("no free port near {}", port));
            write_status(&status);
            wait_for_stop();
            cleanup(None, None, &status);
            return;
        }
    };
    status.socks_port = socks_port;
    let target = format!("{}:{}", _ns_ip, port);
    let relay_addr = format!("127.0.0.1:{}", socks_port);
    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let stop = stop_flag.clone();
        std::thread::spawn(move || relay::run(&relay_addr, &target, stop));
    }

    // ---- children inside the namespace ----
    let mut openvpn = match spawn_openvpn(&cfg) {
        Ok(c) => Some(c),
        Err(e) => {
            status = Status::new("error", &e, socks_port);
            status.error = Some(e.clone());
            write_status(&status);
            wait_for_stop();
            cleanup(None, None, &status);
            return;
        }
    };
    let mut socks = match spawn_socks(&_ns_ip, port, &cfg.dns()) {
        Ok(c) => Some(c),
        Err(e) => {
            status = Status::new("error", &e, socks_port);
            status.error = Some(e.clone());
            write_status(&status);
            wait_for_stop();
            cleanup(openvpn, None, &status);
            return;
        }
    };

    let mut restarts: u32 = 0;
    let mut last_restart = Instant::now();
    let mut state_key = String::new();

    loop {
        if STOP.load(Ordering::Relaxed) {
            break;
        }

        // if the namespace is gone (e.g. torn down by external interference)
        // there is nothing left to supervise — exit and let a fresh enable
        // rebuild everything
        if !netns::ns_exists() {
            log("network namespace disappeared; exiting (re-enable to start fresh)");
            set_state(
                &mut status,
                &mut state_key,
                "error",
                "network namespace disappeared; re-enable the tunnel",
                socks_port,
            );
            break;
        }

        // ---- reap / restart OpenVPN ----
        if let Some(child) = openvpn.as_mut() {
            if let Some(code) = try_exit(child) {
                log(&format!("openvpn exited (code {})", code));
                openvpn = None;
                if restarts < MAX_OPENVPN_RESTARTS
                    && last_restart.elapsed() >= RESTART_DELAY
                {
                    restarts += 1;
                    last_restart = Instant::now();
                    match spawn_openvpn(&cfg) {
                        Ok(c) => {
                            openvpn = Some(c);
                            set_state(
                                &mut status,
                                &mut state_key,
                                "starting",
                                &format!("restarting openvpn (attempt {})", restarts),
                                socks_port,
                            );
                        }
                        Err(e) => {
                            status.error = Some(e.clone());
                            set_state(
                                &mut status,
                                &mut state_key,
                                "error",
                                &e,
                                socks_port,
                            );
                        }
                    }
                } else {
                    status.error = Some(format!(
                        "openvpn exited after {} restart(s); giving up",
                        restarts
                    ));
                    set_state(
                        &mut status,
                        &mut state_key,
                        "error",
                        "openvpn exited; give up",
                        socks_port,
                    );
                }
            }
        }

        // ---- reap / restart the SOCKS5 server ----
        if let Some(child) = socks.as_mut() {
            if try_exit(child).is_some() {
                log("socks5 child exited; restarting");
                socks = None;
                match spawn_socks(&_ns_ip, port, &cfg.dns()) {
                    Ok(c) => socks = Some(c),
                    Err(e) => log(&format!("cannot restart socks5: {}", e)),
                }
            }
        }

        // ---- tunnel state: host tun -> move into netns -> default route ----
        if netns::host_tun_ready() {
            netns::move_tun_to_netns();
        }
        let tun_up = netns::ns_tun_ready();
        if tun_up {
            let _ = netns::ensure_ns_tun_default();
            if status.state != "running" {
                set_state(
                    &mut status,
                    &mut state_key,
                    "running",
                    "tunnel up",
                    socks_port,
                );
            }
        } else if status.state == "running" {
            set_state(
                &mut status,
                &mut state_key,
                "reconnecting",
                "tunnel down; waiting for reconnect",
                socks_port,
            );
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    // ---- graceful teardown ----
    log("supervisor stopping");
    let final_status = Status::new("stopped", "stopped", socks_port);
    write_status(&final_status);
    // free the pidfile FIRST so a concurrent `enable` is not blocked by a
    // supervisor that is still finishing its cleanup
    let _ = std::fs::remove_file(config::PID_PATH);
    cleanup(openvpn, socks, &final_status);
}

fn wait_for_stop() {
    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn set_state(status: &mut Status, key: &mut String, state: &str, detail: &str, port: u16) {
    let k = format!("{}|{}", state, detail);
    if *key == k {
        return;
    }
    *key = k;
    status.state = state.to_string();
    status.detail = detail.to_string();
    status.socks_port = port;
    log(&format!("status -> {} ({})", state, detail));
    write_status(status);
}

fn try_exit(child: &mut Child) -> Option<i32> {
    match child.try_wait() {
        Ok(Some(st)) => Some(st.code().unwrap_or(-1)),
        _ => None,
    }
}

fn spawn_openvpn(cfg: &Config) -> Result<Child, String> {
    let ovpn = cfg.ovpn_path.clone().unwrap_or_else(|| config::DEFAULT_OVPN_PATH.to_string());
    let openvpn_bin = crate::util::find_bin("openvpn").unwrap_or_else(|| "openvpn".to_string());
    if let Ok(text) = std::fs::read_to_string(&ovpn) {
        let remotes = crate::config::parse_remotes(&text);
        log(&format!("openvpn remotes: {:?}", remotes));
    }
    // Runs on the HOST (control channel uses normal host routing). The tun
    // device is moved into the namespace by the supervisor once configured.
    let mut cmd = Command::new(&openvpn_bin);
    cmd.args([
        "--config",
        ovpn.as_str(),
        "--dev",
        config::TUN_DEV,
        "--dev-type",
        "tun",
        "--route-noexec",
        // userspace data path: DCO (ovpn-dco) devices cannot be moved into
        // another network namespace, so force the well-tested tun path
        "--disable-dco",
        // do not let OpenVPN run its dns-updown script on the host (KIT
        // pushes dhcp-option DNS; we resolve through the tunnel instead)
        "--script-security",
        "0",
        "--auth-retry",
        "nointeract",
        "--auth-nocache",
        "--verb",
        "3",
        "--log",
        "/run/kit-vpn/openvpn.log",
        "--status",
        "/run/kit-vpn/openvpn-status.log",
        "30",
    ]);
    // auth-user-pass: read credentials from a root-only file (no prompting;
    // the supervisor has no terminal). Overrides any bare `auth-user-pass`
    // line in the .ovpn. KIT's cert-based configs don't need this.
    if let Some(auth) = cfg.auth_user_pass.clone() {
        if std::path::Path::new(&auth).is_file() {
            cmd.args(["--auth-user-pass", auth.as_str()]);
        } else {
            log(&format!(
                "auth_user_pass file not found: {} — continuing without it",
                auth
            ));
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| format!("cannot spawn openvpn: {}", e))
}

fn spawn_socks(ns_ip: &str, port: u16, dns_servers: &[String]) -> Result<Child, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate own binary: {}", e))?
        .to_string_lossy()
        .to_string();
    let dns_arg = dns_servers.join(",");
    let port_s = port.to_string();
    let args = [
        exe.as_str(),
        "child-socks",
        "--bind-ip",
        ns_ip,
        "--port",
        port_s.as_str(),
        "--dns",
        dns_arg.as_str(),
        "--log",
        "/run/kit-vpn/socks.log",
    ];
    netns::spawn_in_netns(&args)
}

fn cleanup(openvpn: Option<Child>, socks: Option<Child>, _status: &Status) {
    let mut children: Vec<Child> = vec![];
    if let Some(c) = openvpn {
        children.push(c);
    }
    if let Some(c) = socks {
        children.push(c);
    }
    for mut c in children {
        stop_child(&mut c);
    }
    let _ = netns::cleanup_netns();
}

fn stop_child(c: &mut Child) {
    let pid = nix::unistd::Pid::from_raw(c.id() as i32);
    let _ = nix::sys::signal::kill(pid, Some(nix::sys::signal::Signal::SIGTERM));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(_)) = c.try_wait() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = c.kill();
            // bounded wait: never block the supervisor's teardown forever
            let hard = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(Some(_)) = c.try_wait() {
                    return;
                }
                if Instant::now() >= hard {
                    log(&format!("stop_child: giving up on pid {}", c.id()));
                    return; // will be reparented to init and reaped there
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
