//! Minimal DNS client used by the in-namespace SOCKS5 server for remote DNS.
//!
//! Queries are sent from inside the network namespace, so they are routed
//! through the OpenVPN tunnel (default route = tun0). KIT-internal names are
//! therefore resolved by KIT's own DNS and never leak to the host resolver.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Resolve `name` to an IPv4 address using the given DNS servers (in order).
pub fn resolve_a(name: &str, servers: &[String], timeout_ms: u64) -> Option<Ipv4Addr> {
    if servers.is_empty() {
        return None;
    }
    let query = build_query(name)?;
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(timeout_ms))).ok()?;

    for srv in servers {
        let addr: SocketAddr = match format!("{}:53", srv).parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        if sock.send_to(&query, addr).is_err() {
            continue;
        }
        let mut buf = [0u8; 4096];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(ip) = parse_response(&buf[..n], &query) {
                    return Some(ip);
                }
            }
            Err(_) => continue,
        }
    }
    None
}

fn build_query(name: &str) -> Option<Vec<u8>> {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .subsec_nanos() as u16;
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes()); // ID
    q.extend_from_slice(&[0x01, 0x00]);     // flags: RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QD=1
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // QTYPE=A, QCLASS=IN
    Some(q)
}

fn parse_response(buf: &[u8], query: &[u8]) -> Option<Ipv4Addr> {
    if buf.len() < 12 {
        return None;
    }
    if buf[0..2] != query[0..2] {
        return None; // ID mismatch
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 == 0 {
        return None; // not a response
    }
    if flags & 0x000F != 0 {
        return None; // rcode != NOERROR
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]);
    let an = u16::from_be_bytes([buf[6], buf[7]]);
    if qd == 0 || an == 0 {
        return None;
    }
    let mut off = 12;
    for _ in 0..qd {
        off = skip_name(buf, off)?;
        off += 4; // QTYPE + QCLASS
    }
    for _ in 0..an {
        off = skip_name(buf, off)?;
        if off + 10 > buf.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let rclass = u16::from_be_bytes([buf[off + 2], buf[off + 3]]);
        let rdlen = u16::from_be_bytes([buf[off + 8], buf[off + 9]]) as usize;
        off += 10;
        if off + rdlen > buf.len() {
            return None;
        }
        if rtype == 1 && rclass == 1 && rdlen == 4 {
            return Some(Ipv4Addr::new(buf[off], buf[off + 1], buf[off + 2], buf[off + 3]));
        }
        off += rdlen;
    }
    None
}

fn skip_name(buf: &[u8], mut off: usize) -> Option<usize> {
    loop {
        if off >= buf.len() {
            return None;
        }
        let l = buf[off];
        if l == 0 {
            return Some(off + 1);
        }
        if l & 0xC0 == 0xC0 {
            return Some(off + 2); // compression pointer
        }
        off += 1 + l as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_builds_and_parses_roundtrip() {
        let q = build_query("www.kit.edu").unwrap();
        // craft a response by echoing the question + one A record
        let mut r = q.clone();
        r[2..4].copy_from_slice(&[0x81, 0x80]); // QR + RD + RA
        r[6..8].copy_from_slice(&[0, 1]); // ANCOUNT=1
        // answer: pointer to question name at offset 12, A record
        let mut ans = Vec::new();
        ans.extend_from_slice(&[0xC0, 0x0C]);
        ans.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        ans.extend_from_slice(&[129, 13, 10, 90]);
        r.extend_from_slice(&ans);
        assert_eq!(parse_response(&r, &q), Some(Ipv4Addr::new(129, 13, 10, 90)));
    }
}
