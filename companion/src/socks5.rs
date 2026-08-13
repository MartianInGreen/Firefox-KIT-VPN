//! SOCKS5 (RFC 1928) server, run *inside* the KIT network namespace.
//!
//! It binds to the namespace-side veth address (e.g. 10.200.200.2), which is
//! reachable only through the veth from the host relay. Hostnames are
//! resolved via `dns::resolve_a`, which routes through the tunnel, so DNS
//! never leaks. CONNECT failures are propagated to the client; there is no
//! fallback to the host network (fail-closed).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

use crate::dns;
use crate::util::{log, relay};

pub fn run_server(bind_ip: &str, port: u16, dns_servers: &[String]) {
    let addr = format!("{}:{}", bind_ip, port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("socks5: cannot bind {}: {}", addr, e));
            return;
        }
    };
    log(&format!("socks5: listening on {}", addr));
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let dns = dns_servers.to_vec();
                std::thread::spawn(move || handle(s, dns));
            }
            Err(_) => continue,
        }
    }
}

fn handle(mut s: TcpStream, dns_servers: Vec<String>) {
    log("socks5: connection accepted");
    // ---- greeting: VER=5, NMETHODS, METHODS ----
    let mut hdr = [0u8; 2];
    if s.read_exact(&mut hdr).is_err() || hdr[0] != 5 {
        return;
    }
    let nmethods = hdr[1] as usize;
    let mut methods = vec![0u8; nmethods.min(255)];
    if s.read_exact(&mut methods).is_err() {
        return;
    }
    if !methods.contains(&0) {
        let _ = s.write_all(&[5, 0xFF]); // no acceptable method
        return;
    }
    let _ = s.write_all(&[5, 0]); // no authentication

    // ---- request: VER=5, CMD, RSV, ATYP, DST.ADDR, DST.PORT ----
    let mut req = [0u8; 4];
    if s.read_exact(&mut req).is_err() || req[0] != 5 {
        return;
    }
    let cmd = req[1];
    if cmd != 1 {
        // only CONNECT is supported
        let _ = reply(&mut s, 7);
        return;
    }
    let atyp = req[3];

    let target: Vec<u8>;
    let mut port_bytes = [0u8; 2];
    match atyp {
        1 => {
            let mut a = [0u8; 4];
            if s.read_exact(&mut a).is_err() {
                return;
            }
            target = a.to_vec();
        }
        3 => {
            let mut len = [0u8; 1];
            if s.read_exact(&mut len).is_err() {
                return;
            }
            let mut name = vec![0u8; len[0] as usize];
            if s.read_exact(&mut name).is_err() {
                return;
            }
            target = name;
        }
        4 => {
            let _ = reply(&mut s, 8); // IPv6 not supported (tunnel is IPv4)
            return;
        }
        _ => {
            let _ = reply(&mut s, 8);
            return;
        }
    }
    if s.read_exact(&mut port_bytes).is_err() {
        return;
    }
    let port = u16::from_be_bytes(port_bytes);

    // ---- resolve (remote DNS through the tunnel) ----
    let ip: Ipv4Addr = match atyp {
        1 => Ipv4Addr::new(target[0], target[1], target[2], target[3]),
        3 => {
            let name = String::from_utf8_lossy(&target).to_string();
            match dns::resolve_a(&name, &dns_servers, 3000) {
                Some(ip) => {
                    log(&format!("socks5: resolved {} -> {}", name, ip));
                    ip
                }
                None => {
                    log(&format!("socks5: DNS resolution failed for {}", name));
                    let _ = reply(&mut s, 4); // host unreachable
                    return;
                }
            }
        }
        _ => return,
    };

    // ---- connect (through the tunnel; fails closed when tun0 is down) ----
    let dst = SocketAddr::V4(SocketAddrV4::new(ip, port));
    match TcpStream::connect_timeout(&dst, Duration::from_secs(10)) {
        Ok(upstream) => {
            log(&format!("socks5: CONNECT {} -> {} ok", dst, ip));
            let _ = reply(&mut s, 0);
            relay(s, upstream);
        }
        Err(e) => {
            let rep = match e.kind() {
                std::io::ErrorKind::ConnectionRefused => 5,
                std::io::ErrorKind::NetworkUnreachable
                | std::io::ErrorKind::HostUnreachable => 3,
                _ => 1,
            };
            log(&format!("socks5: CONNECT {} failed: {}", dst, e));
            let _ = reply(&mut s, rep);
        }
    }
}

fn reply(s: &mut TcpStream, rep: u8) -> std::io::Result<()> {
    // VER, REP, RSV, ATYP=IPv4, BND.ADDR=0.0.0.0, BND.PORT=0
    s.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0])
}
