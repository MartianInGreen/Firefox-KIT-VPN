//! kit-vpn-companion — local companion for the KIT-only Firefox extension.
//!
//! Modes:
//!   (no args) / nm           Native Messaging loop (spawned by Firefox)
//!   helper enable|disable    Root operations (via sudo, whitelisted in sudoers)
//!   child-socks ...          In-namespace SOCKS5 server (spawned by supervisor)
//!   config --ovpn <file>     Installer: copy an .ovpn + write config
//!   --version | --help

mod config;
mod dns;
mod helper;
mod netns;
mod nm;
mod relay;
mod socks5;
mod supervisor;
mod util;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        None | Some("nm") => {
            nm::run();
            ExitCode::SUCCESS
        }
        // Firefox launches native messaging hosts with the manifest path and
        // the extension ID as arguments (argv[1], argv[2]). Accept that as
        // Native-Messaging mode.
        Some(other) if other.starts_with('/') => {
            util::log(&format!(
                "invoked by Firefox (argv[1]={}, argv[2]={}); entering native-messaging mode",
                other,
                args.get(1).map(|s| s.as_str()).unwrap_or("")
            ));
            nm::run();
            ExitCode::SUCCESS
        }
        Some("helper") => match args.get(1).map(|s| s.as_str()) {
            Some("enable") => helper_result(helper::enable()),
            Some("disable") => helper_result(helper::disable()),
            other => {
                eprintln!("helper: unknown operation {:?}", other);
                ExitCode::from(2)
            }
        },
        Some("child-socks") => {
            let (bind_ip, port, dns, logf) = parse_child_socks(&args[1..]);
            util::log_init(&logf);
            util::log(&format!(
                "child-socks: bind {}:{} dns={:?}",
                bind_ip, port, dns
            ));
            socks5::run_server(&bind_ip, port, &dns);
            ExitCode::SUCCESS
        }
        Some("config") => match args.get(1).map(|s| s.as_str()) {
            Some("--ovpn") => match args.get(2) {
                Some(path) => {
                    // optional: --auth-user-pass <file> (username/password)
                    let auth = if args.get(3).map(|s| s.as_str()) == Some("--auth-user-pass") {
                        args.get(4).map(|s| s.as_str())
                    } else {
                        None
                    };
                    match config::install_ovpn(path, auth) {
                        Ok(msg) => {
                            println!("{}", msg);
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("{}", e);
                            ExitCode::from(1)
                        }
                    }
                }
                None => {
                    eprintln!(
                        "usage: kit-vpn-companion config --ovpn <file.ovpn> [--auth-user-pass <file>]"
                    );
                    ExitCode::from(2)
                }
            },
            _ => {
                eprintln!("usage: kit-vpn-companion config --ovpn <file.ovpn> [--auth-user-pass <file>]");
                ExitCode::from(2)
            }
        },
        Some("--version") => {
            println!("kit-vpn-companion {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown mode: {}", other);
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn helper_result(r: Result<String, String>) -> ExitCode {
    match r {
        Ok(json) => {
            println!("{}", json);
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Always emit JSON so the Native-Messaging side can parse it.
            let v = serde_json::json!({
                "state": "error",
                "detail": e,
                "error": e,
                "socks_port": 0,
                "pid": 0,
                "since": 0
            });
            println!("{}", v);
            ExitCode::from(1)
        }
    }
}

fn parse_child_socks(args: &[String]) -> (String, u16, Vec<String>, String) {
    let mut bind_ip = String::new();
    let mut port: u16 = 0;
    let mut dns: Vec<String> = Vec::new();
    let mut logf = "/run/kit-vpn/socks.log".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bind-ip" => {
                i += 1;
                if i < args.len() {
                    bind_ip = args[i].clone();
                }
            }
            "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().unwrap_or(0);
                }
            }
            "--dns" => {
                i += 1;
                if i < args.len() {
                    dns = args[i].split(',').map(|s| s.to_string()).collect();
                }
            }
            "--log" => {
                i += 1;
                if i < args.len() {
                    logf = args[i].clone();
                }
            }
            _ => {}
        }
        i += 1;
    }
    (bind_ip, port, dns, logf)
}

fn print_usage() {
    println!(
        "kit-vpn-companion {} — KIT-only OpenVPN tunnel companion\n\
         \n\
         usage:\n\
         \x20 kit-vpn-companion                     Native Messaging mode (run by Firefox)\n\
         \x20 kit-vpn-companion helper enable       start tunnel (root, via sudo)\n\
         \x20 kit-vpn-companion helper disable      stop tunnel (root, via sudo)\n\
         \x20 kit-vpn-companion config --ovpn FILE  install an .ovpn + write /etc/kit-vpn/config.toml\n\
         \x20 kit-vpn-companion --version\n",
        env!("CARGO_PKG_VERSION")
    );
}
