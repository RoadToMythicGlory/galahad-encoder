//! Local + public IP discovery.
//!
//! In SRT *listener* mode the player must hand the caller an address to connect
//! to. We surface two best-effort answers so the UI can pre-fill the IP box:
//!   * `public` - the internet-facing IP (for callers coming over the WAN;
//!     usually still needs a forwarded port). Fetched from a plain-text IP echo
//!     service over HTTP with short timeouts.
//!   * `local`  - the LAN IP of the default route (for callers on the same
//!     network). Determined with the classic connected-UDP-socket trick, which
//!     sends no packets.
//!
//! Both are optional; discovery never blocks stream start and degrades quietly
//! when the machine is offline.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs, UdpSocket};
use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;

const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpInfo {
    /// Internet-facing IP, if it could be resolved.
    pub public: Option<String>,
    /// LAN IP of the default route, if any.
    pub local: Option<String>,
}

/// Discover both the local and public IP (best-effort).
pub fn detect() -> IpInfo {
    IpInfo {
        public: public_ip(),
        local: local_ip(),
    }
}

/// LAN IP via a connected UDP socket. Connecting only sets the socket's default
/// route so `local_addr` reports the outbound interface; no datagrams are sent.
pub fn local_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let addr = sock.local_addr().ok()?;
    let ip = addr.ip();
    if ip.is_unspecified() {
        None
    } else {
        Some(ip.to_string())
    }
}

/// Public IP via a plain-text echo service. Tries a few providers in turn.
fn public_ip() -> Option<String> {
    const PROVIDERS: &[(&str, &str)] = &[
        ("api.ipify.org", "/"),
        ("checkip.amazonaws.com", "/"),
        ("ifconfig.me", "/ip"),
    ];
    for (host, path) in PROVIDERS {
        if let Some(ip) = http_get_ip(host, path) {
            return Some(ip);
        }
    }
    None
}

fn http_get_ip(host: &str, path: &str) -> Option<String> {
    let addr = (host, 80u16).to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&addr, HTTP_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT)).ok()?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: galahad-encoder\r\n\
         Accept: text/plain\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    parse_ip_from_response(&text)
}

/// Extract the first IPv4 literal from an HTTP response (after the header block
/// if present). Robust to chunked bodies: chunk-size hex lines never parse as
/// dotted IPv4.
fn parse_ip_from_response(response: &str) -> Option<String> {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(response);
    parse_ipv4(body)
}

fn parse_ipv4(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace())
        .map(str::trim)
        .find(|token| Ipv4Addr::from_str(token).is_ok())
        .map(|token| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ip_from_plain_body() {
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n203.0.113.9";
        assert_eq!(
            parse_ip_from_response(response),
            Some("203.0.113.9".to_string())
        );
    }

    #[test]
    fn parses_ip_with_trailing_newline() {
        let response = "HTTP/1.1 200 OK\r\n\r\n198.51.100.4\n";
        assert_eq!(
            parse_ip_from_response(response),
            Some("198.51.100.4".to_string())
        );
    }

    #[test]
    fn parses_ip_from_chunked_body() {
        // Chunk size "b" then the IP then terminating chunk.
        let response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n192.0.2.55\r\n0\r\n\r\n";
        assert_eq!(
            parse_ip_from_response(response),
            Some("192.0.2.55".to_string())
        );
    }

    #[test]
    fn ignores_non_ip_bodies() {
        let response = "HTTP/1.1 500 Error\r\n\r\nservice unavailable";
        assert_eq!(parse_ip_from_response(response), None);
    }

    #[test]
    fn rejects_out_of_range_octets() {
        assert_eq!(parse_ipv4("999.1.1.1"), None);
    }
}
