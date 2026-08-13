//! Small shared helpers: file logging, subprocess helpers, atomic status
//! writes, and a generic TCP relay used by both the host relay and the
//! in-namespace SOCKS server.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Mutex, Once};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config;

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static LOG_INIT: Once = Once::new();

/// Initialise file logging (idempotent per process).
pub fn log_init(path: &str) {
    LOG_INIT.call_once(|| {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        if let Ok(mut guard) = LOG.lock() {
            *guard = f;
        }
    });
}

pub fn log(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{}] {}", secs, msg);
    if let Ok(mut guard) = LOG.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{}", line);
        }
    }
    eprintln!("{}", line);
}

/// Locate a system binary (absolute paths are preferred because the helper
/// runs through sudo with a minimal PATH).
pub fn find_bin(name: &str) -> Option<String> {
    let candidates = [
        format!("/usr/local/sbin/{}", name),
        format!("/usr/local/bin/{}", name),
        format!("/usr/sbin/{}", name),
        format!("/sbin/{}", name),
        format!("/usr/bin/{}", name),
        format!("/bin/{}", name),
    ];
    for c in candidates {
        if std::path::Path::new(&c).is_file() {
            return Some(c);
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            let p = format!("{}/{}", dir, name);
            if std::path::Path::new(&p).is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn bin(name: &str) -> String {
    find_bin(name).unwrap_or_else(|| name.to_string())
}

/// Run a command with an argument vector, capturing stdout. Errors include
/// stderr text. No shell is involved.
pub fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    log(&format!("run: {} {}", program, args.join(" ")));
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("failed to execute {}: {}", program, e))?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        let msg = if stderr.is_empty() {
            format!("{} {} exited with {}", program, args.join(" "), out.status)
        } else {
            format!(
                "{} {} failed: {}",
                program,
                args.join(" "),
                stderr.lines().last().unwrap_or("")
            )
        };
        return Err(msg);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn ip(args: &[&str]) -> Result<String, String> {
    run_cmd(&bin("ip"), args)
}

/// Run `ip netns exec NETNS ip ...`.
pub fn ns_ip(args: &[&str]) -> Result<String, String> {
    let mut full: Vec<&str> = vec!["netns", "exec", config::NETNS, "ip"];
    full.extend_from_slice(args);
    ip(&full)
}

/// Atomically write a file (temp + rename).
pub fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    let tmp = format!("{}.tmp", path);
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: String, // stopped|starting|running|reconnecting|error|unavailable
    pub detail: String,
    pub socks_port: u16,
    pub pid: u32,
    pub since: u64,
    pub error: Option<String>,
}

impl Status {
    pub fn new(state: &str, detail: &str, socks_port: u16) -> Self {
        Status {
            state: state.to_string(),
            detail: detail.to_string(),
            socks_port,
            pid: std::process::id(),
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            error: None,
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

pub fn write_status(s: &Status) {
    let _ = write_atomic(config::STATUS_PATH, &s.json());
}

pub fn read_status() -> Option<Status> {
    let text = std::fs::read_to_string(config::STATUS_PATH).ok()?;
    serde_json::from_str(&text).ok()
}

/// Bidirectional byte relay between two TCP streams.
pub fn relay(mut a: TcpStream, mut b: TcpStream) {
    let mut a2 = match a.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut b2 = match b.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let t1 = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a, &mut b2);
        let _ = b2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b, &mut a2);
    let _ = a2.shutdown(std::net::Shutdown::Write);
    let _ = t1.join();
}
