//! Native Messaging mode: spawned by Firefox (unprivileged), speaks the
//! standard length-prefixed JSON framing on stdin/stdout.
//!
//! Protocol (extension -> companion):
//!   {"type":"status"}                    -> current status
//!   {"type":"enable"}                    -> start tunnel (idempotent)
//!   {"type":"disable"}                   -> stop tunnel
//!
//! Replies always have the shape of a status object with an extra "type"
//! field. The process exits when stdin closes.

use std::io::{BufReader, Read, Write};

use crate::util::{log, log_init, read_status, Status};

const MAX_MESSAGE: usize = 4 * 1024 * 1024;

pub fn run() {
    let log_path = if let Ok(h) = std::env::var("HOME") {
        format!("{}/.cache/kit-vpn/nm.log", h)
    } else {
        "/tmp/kit-vpn-nm.log".to_string()
    };
    log_init(&log_path);
    log("nm process started");

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        match read_message(&mut reader) {
            Ok(Some(payload)) => {
                if let Some(reply) = handle(&payload) {
                    let _ = write_message(&reply);
                }
            }
            Ok(None) => break, // EOF: Firefox closed the port
            Err(e) => {
                log(&format!("nm read error: {}", e));
                break;
            }
        }
    }
    log("nm process exiting");
}

/// Read one length-prefixed message (u32 LE length + JSON bytes).
fn read_message<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut len_buf = [0u8; 4];
    let mut got = 0usize;
    while got < 4 {
        match r.read(&mut len_buf[got..]) {
            Ok(0) => {
                if got == 0 {
                    return Ok(None);
                }
                return Err("EOF mid-header".to_string());
            }
            Ok(n) => got += n,
            Err(e) => return Err(format!("read: {}", e)),
        }
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_MESSAGE {
        return Err(format!("message too large: {} bytes", len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|e| format!("read payload: {}", e))?;
    Ok(Some(buf))
}

fn write_message(payload: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn handle(payload: &[u8]) -> Option<Vec<u8>> {
    let msg: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => {
            log(&format!("nm: invalid JSON: {}", e));
            return Some(json_reply(error_status(&format!("invalid JSON: {}", e))));
        }
    };
    let msg_type = msg
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match msg_type.as_str() {
        "status" => Some(json_reply(current_status())),
        "enable" => {
            let st = match run_helper("enable") {
                Ok(json) => parse_helper_status(&json).unwrap_or_else(|| {
                    error_status("companion returned an invalid response")
                }),
                Err(e) => error_status(&e),
            };
            Some(json_reply(st))
        }
        "disable" => {
            let st = match run_helper("disable") {
                Ok(json) => parse_helper_status(&json)
                    .unwrap_or_else(|| error_status("companion returned an invalid response")),
                Err(e) => error_status(&e),
            };
            Some(json_reply(st))
        }
        other => {
            log(&format!("nm: unknown message type: {}", other));
            Some(json_reply(error_status(&format!("unknown message type: {}", other))))
        }
    }
}

fn current_status() -> Status {
    read_status().unwrap_or_else(|| Status::new("stopped", "tunnel not running", 0))
}

fn error_status(detail: &str) -> Status {
    let mut st = Status::new("error", detail, 0);
    st.error = Some(detail.to_string());
    st
}

fn parse_helper_status(json: &str) -> Option<Status> {
    serde_json::from_str(json).ok()
}

fn json_reply(st: Status) -> Vec<u8> {
    // Wrap the status into a reply object with a "type" field.
    let mut v = serde_json::to_value(&st).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(ref mut map) = v {
        map.insert("type".to_string(), serde_json::Value::String("status".to_string()));
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| b"{}".to_vec())
}

/// Run the root helper through `sudo -n` (or directly when already root).
fn run_helper(op: &str) -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate companion binary: {}", e))?
        .to_string_lossy()
        .to_string();

    let output = if unsafe { nix::libc::geteuid() } == 0 {
        std::process::Command::new(&exe)
            .args(["helper", op])
            .output()
    } else {
        let sudo = crate::util::find_bin("sudo").unwrap_or_else(|| "sudo".to_string());
        std::process::Command::new(&sudo)
            .args(["-n", &exe, "helper", op])
            .output()
    }
    .map_err(|e| format!("failed to run privileged helper: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.contains("password") || stderr.contains("not allowed") {
            "passwordless sudo is not configured for the KIT VPN helper — re-run scripts/install.sh (it grants the helper NOPASSWD access)".to_string()
        } else if !stderr.is_empty() {
            format!("helper failed: {}", stderr)
        } else if !stdout.trim().is_empty() {
            format!("helper failed: {}", stdout.trim())
        } else {
            format!("helper exited with {}", output.status)
        };
        return Err(detail);
    }
    Ok(stdout)
}
