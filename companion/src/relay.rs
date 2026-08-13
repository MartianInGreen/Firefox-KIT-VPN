//! Host-side TCP relay: exposes the SOCKS5 endpoint on 127.0.0.1 (the only
//! listener Firefox ever talks to) and forwards each connection through the
//! veth into the network namespace, where the real SOCKS5 server runs.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::util::{log, relay};

/// Find a free port starting at `port` (inclusive) on 127.0.0.1.
pub fn bind_free_port(port: u16) -> Option<u16> {
    for p in port..port.saturating_add(20) {
        if TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Some(p);
        }
    }
    None
}

pub fn run(listen_addr: &str, target: &str, stop: Arc<AtomicBool>) {
    let listener = match TcpListener::bind(listen_addr) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("relay: cannot bind {}: {}", listen_addr, e));
            return;
        }
    };
    log(&format!("relay: {} -> {}", listen_addr, target));
    for stream in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(client) = stream {
            let target = target.to_string();
            std::thread::spawn(move || {
                match TcpStream::connect(target.as_str()) {
                    Ok(upstream) => {
                        relay(client, upstream);
                    }
                    Err(e) => log(&format!("relay: connect {} failed: {}", target, e)),
                }
            });
        }
    }
}
