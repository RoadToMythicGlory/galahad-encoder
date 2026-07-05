//! SRT URL construction (caller and listener).
//!
//! Galahad Encoder can either *call* the matrix (the classic uplink: we connect
//! to an operator-supplied host + port) or *listen* (we bind a port and wait for
//! a caller to pull the feed from us). Listener mode is handy when the encoder
//! sits behind a known/forwarded address and the far side initiates the pull;
//! the player just hands the caller their IP + port. This module is deliberately
//! tiny and pure so it can be unit tested without any native dependencies.

use serde::{Deserialize, Serialize};

use crate::error::{EncoderError, Result};

/// Default SRT live latency in milliseconds. Matches the receiver examples in
/// `docs/MEDIA.md` (`latency=200`).
pub const DEFAULT_LATENCY_MS: u32 = 200;

/// MPEG-TS packet payload size recommended for SRT (7 * 188).
pub const SRT_PKT_SIZE: u32 = 1316;

/// Which side of the SRT handshake this encoder plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SrtMode {
    /// We connect out to `host:port` (default uplink behaviour).
    Caller,
    /// We bind `port` and wait for a caller to connect and pull the feed.
    Listener,
}

impl Default for SrtMode {
    fn default() -> Self {
        SrtMode::Caller
    }
}

impl SrtMode {
    fn as_url_mode(&self) -> &'static str {
        match self {
            SrtMode::Caller => "caller",
            SrtMode::Listener => "listener",
        }
    }
}

/// A validated SRT destination (caller) or bind target (listener).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtDestination {
    /// In caller mode: the remote host to connect to. In listener mode: an
    /// advisory address (the IP the player shares with the caller); the actual
    /// bind is always `0.0.0.0` so any interface can accept the connection.
    pub host: String,
    pub port: u16,
    pub latency_ms: u32,
    pub mode: SrtMode,
}

impl SrtDestination {
    /// Validate raw user input into a usable destination for the given mode.
    ///
    /// In caller mode `host` must be an IP/hostname (no scheme, no whitespace).
    /// In listener mode `host` is optional (informational only) since we bind
    /// all interfaces. The port must be 1-65535 in both modes.
    pub fn parse(host: &str, port: u32, latency_ms: u32, mode: SrtMode) -> Result<Self> {
        let host = host.trim();

        if matches!(mode, SrtMode::Caller) {
            if host.is_empty() {
                return Err(EncoderError::Config("destination host is empty".into()));
            }
            if host.contains("://") {
                return Err(EncoderError::Config(
                    "destination host must be an IP or hostname, not a URL".into(),
                ));
            }
            if host.chars().any(|c| c.is_whitespace()) {
                return Err(EncoderError::Config(
                    "destination host contains whitespace".into(),
                ));
            }
        }

        if port == 0 || port > u16::MAX as u32 {
            return Err(EncoderError::Config(format!(
                "port {port} is out of range (1-65535)"
            )));
        }
        let latency_ms = if latency_ms == 0 {
            DEFAULT_LATENCY_MS
        } else {
            latency_ms
        };
        Ok(Self {
            host: host.to_string(),
            port: port as u16,
            latency_ms,
            mode,
        })
    }

    /// Build the SRT URL FFmpeg will use. Callers connect to `host:port`;
    /// listeners bind `0.0.0.0:port` so any interface can accept the puller.
    pub fn to_url(&self) -> String {
        let host = match self.mode {
            SrtMode::Caller => self.host.as_str(),
            SrtMode::Listener => "0.0.0.0",
        };
        self.url_for(host, self.port)
    }

    fn url_for(&self, host: &str, port: u16) -> String {
        format!(
            "srt://{host}:{port}?mode={mode}&latency={latency}&pkt_size={pkt}",
            host = host,
            port = port,
            mode = self.mode.as_url_mode(),
            latency = self.latency_ms,
            pkt = SRT_PKT_SIZE,
        )
    }

    /// The one or more SRT URLs FFmpeg should output to.
    ///
    /// A caller yields a single URL. A listener that serves up to `max_callers`
    /// callers yields one `0.0.0.0` bind per consecutive port starting at
    /// `port` (port, port+1, ...), because a single SRT listener socket only
    /// accepts one peer; each caller connects to its own port. `max_callers` is
    /// clamped to 1-3 and ports past 65535 are dropped.
    pub fn endpoints(&self, max_callers: u8) -> Vec<String> {
        match self.mode {
            SrtMode::Caller => vec![self.to_url()],
            SrtMode::Listener => {
                let n = max_callers.clamp(1, 3) as u32;
                let base = self.port as u32;
                (0..n)
                    .map(|i| base + i)
                    .filter(|p| *p <= u16::MAX as u32)
                    .map(|p| self.url_for("0.0.0.0", p as u16))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_caller_url() {
        let dest = SrtDestination::parse("1.2.3.4", 9003, 200, SrtMode::Caller).unwrap();
        assert_eq!(
            dest.to_url(),
            "srt://1.2.3.4:9003?mode=caller&latency=200&pkt_size=1316"
        );
    }

    #[test]
    fn builds_listener_url_binds_all_interfaces() {
        let dest = SrtDestination::parse("203.0.113.7", 9003, 200, SrtMode::Listener).unwrap();
        assert_eq!(
            dest.to_url(),
            "srt://0.0.0.0:9003?mode=listener&latency=200&pkt_size=1316"
        );
    }

    #[test]
    fn listener_allows_empty_host() {
        let dest = SrtDestination::parse("", 9003, 200, SrtMode::Listener).unwrap();
        assert!(dest.to_url().starts_with("srt://0.0.0.0:9003?mode=listener"));
    }

    #[test]
    fn trims_host_and_defaults_latency() {
        let dest = SrtDestination::parse("  example.lan  ", 9001, 0, SrtMode::Caller).unwrap();
        assert_eq!(dest.host, "example.lan");
        assert_eq!(dest.latency_ms, DEFAULT_LATENCY_MS);
    }

    #[test]
    fn rejects_empty_host_in_caller_mode() {
        assert!(SrtDestination::parse("   ", 9001, 200, SrtMode::Caller).is_err());
    }

    #[test]
    fn rejects_scheme_in_host() {
        assert!(SrtDestination::parse("srt://1.2.3.4", 9001, 200, SrtMode::Caller).is_err());
    }

    #[test]
    fn rejects_bad_port() {
        assert!(SrtDestination::parse("1.2.3.4", 0, 200, SrtMode::Caller).is_err());
        assert!(SrtDestination::parse("1.2.3.4", 70000, 200, SrtMode::Caller).is_err());
        assert!(SrtDestination::parse("", 0, 200, SrtMode::Listener).is_err());
    }

    #[test]
    fn caller_has_single_endpoint_regardless_of_max() {
        let dest = SrtDestination::parse("1.2.3.4", 9003, 200, SrtMode::Caller).unwrap();
        assert_eq!(dest.endpoints(3).len(), 1);
    }

    #[test]
    fn listener_fans_out_to_consecutive_ports() {
        let dest = SrtDestination::parse("203.0.113.7", 9003, 200, SrtMode::Listener).unwrap();
        let urls = dest.endpoints(3);
        assert_eq!(urls.len(), 3);
        assert!(urls[0].starts_with("srt://0.0.0.0:9003?mode=listener"));
        assert!(urls[1].starts_with("srt://0.0.0.0:9004?mode=listener"));
        assert!(urls[2].starts_with("srt://0.0.0.0:9005?mode=listener"));
    }

    #[test]
    fn listener_clamps_max_callers() {
        let dest = SrtDestination::parse("", 9003, 200, SrtMode::Listener).unwrap();
        assert_eq!(dest.endpoints(0).len(), 1);
        assert_eq!(dest.endpoints(9).len(), 3);
    }

    #[test]
    fn listener_drops_ports_past_range() {
        let dest = SrtDestination::parse("", 65535, 200, SrtMode::Listener).unwrap();
        // Only 65535 fits; 65536+ are dropped.
        assert_eq!(dest.endpoints(3).len(), 1);
    }
}
