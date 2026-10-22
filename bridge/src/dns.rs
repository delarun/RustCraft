//! Minecraft's server address lookup: the `_minecraft._tcp.<name>` SRV record.
//! A minimal DNS client over UDP, asking the system's resolvers (from
//! /etc/resolv.conf) or, where there is none, public ones.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

const FALLBACK_RESOLVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];
const TIMEOUT: Duration = Duration::from_secs(2);
const TYPE_SRV: u16 = 33;

/// The target and port of `name`'s Minecraft SRV record, if it has one.
pub fn minecraft_srv(name: &str) -> Option<(String, u16)> {
    let question = format!("_minecraft._tcp.{}", name.trim_end_matches('.'));
    for resolver in resolvers() {
        if let Some(answer) = query_srv(&question, resolver) {
            return answer;
        }
    }
    None
}

fn resolvers() -> Vec<SocketAddr> {
    let configured: Vec<SocketAddr> = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|address| address.trim().parse::<std::net::IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if configured.is_empty() {
        FALLBACK_RESOLVERS.iter().map(|ip| SocketAddr::new(ip.parse().unwrap(), 53)).collect()
    } else {
        configured
    }
}

/// None: the resolver did not answer. Some(None): no such record.
fn query_srv(name: &str, resolver: SocketAddr) -> Option<Option<(String, u16)>> {
    let socket = UdpSocket::bind(if resolver.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    socket.set_read_timeout(Some(TIMEOUT)).ok()?;
    let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.subsec_nanos() & 0xFFFF) as u16;
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]); // recursion desired, one question
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Some(None);
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&TYPE_SRV.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes()); // class IN
    socket.send_to(&packet, resolver).ok()?;
    let mut buffer = [0u8; 1500];
    let (len, _) = socket.recv_from(&mut buffer).ok()?;
    let response = &buffer[..len];
    if response.len() < 12 || response[0..2] != id.to_be_bytes() {
        return None;
    }
    let rcode = response[3] & 0x0F;
    if rcode != 0 {
        return Some(None);
    }
    let answers = u16::from_be_bytes([response[6], response[7]]);
    let mut at = 12;
    at = skip_name(response, at)? + 4; // the question
    let mut best: Option<(u16, u16, String, u16)> = None;
    for _ in 0..answers {
        at = skip_name(response, at)?;
        let ty = u16::from_be_bytes([*response.get(at)?, *response.get(at + 1)?]);
        let length = u16::from_be_bytes([*response.get(at + 8)?, *response.get(at + 9)?]) as usize;
        let data = at + 10;
        if ty == TYPE_SRV && length >= 7 {
            let field = |i: usize| Some(u16::from_be_bytes([*response.get(data + i)?, *response.get(data + i + 1)?]));
            let (priority, weight, port) = (field(0)?, field(2)?, field(4)?);
            let target = read_name(response, data + 6)?;
            let better = best.as_ref().is_none_or(|(p, w, _, _)| priority < *p || (priority == *p && weight > *w));
            if better && !target.is_empty() {
                best = Some((priority, weight, target, port));
            }
        }
        at = data + length;
    }
    Some(best.map(|(_, _, target, port)| (target, port)))
}

/// The offset after a (possibly compressed) name at `at`.
fn skip_name(packet: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *packet.get(at)?;
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        if len == 0 {
            return Some(at + 1);
        }
        at += 1 + len as usize;
    }
}

fn read_name(packet: &[u8], mut at: usize) -> Option<String> {
    let mut labels = Vec::new();
    for _ in 0..64 {
        let len = *packet.get(at)?;
        if len & 0xC0 == 0xC0 {
            at = ((len as usize & 0x3F) << 8) | *packet.get(at + 1)? as usize;
            continue;
        }
        if len == 0 {
            return Some(labels.join("."));
        }
        labels.push(String::from_utf8_lossy(packet.get(at + 1..at + 1 + len as usize)?).into_owned());
        at += 1 + len as usize;
    }
    None
}

#[cfg(test)]
mod tests {
    /// Needs network access: RUSTCRAFT_DNS_TEST=1 cargo test.
    #[test]
    fn live_srv() {
        if std::env::var_os("RUSTCRAFT_DNS_TEST").is_none() {
            return;
        }
        let (target, port) = super::minecraft_srv("hypixel.net").expect("hypixel.net has an SRV record");
        assert_eq!((target.as_str(), port), ("mc.hypixel.net", 25565));
        assert!(super::minecraft_srv("example.com").is_none());
    }
}
