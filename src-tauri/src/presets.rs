//! Broadcast quality profiles.
//!
//! Operators never tune raw bitrate ladders. They pick a named broadcast
//! profile; the client resolves it to resolution / scan mode / rate / bitrate.
//!
//! Profiles are modelled explicitly for broadcast: `scan` distinguishes
//! progressive from interlaced, `display_rate` is the rate as it appears in the
//! profile name (field rate for interlaced, frame rate for progressive), and
//! `frame_rate` is the number of actual frames per second delivered to the
//! encoder / ST 2110 payloader (for interlaced this is `display_rate / 2`).
//!
//! The H.264 / HEVC bitrates target compressed SRT delivery. `st2110_mbps` is a
//! feasibility estimate for uncompressed 4:2:2 10-bit ST 2110-20, used to warn
//! when a machine / network almost certainly cannot carry a profile.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanMode {
    Progressive,
    Interlaced,
}

impl ScanMode {
    pub fn is_interlaced(&self) -> bool {
        matches!(self, ScanMode::Interlaced)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityPreset {
    /// Stable identifier used in config + control-channel commands, e.g. `1080p60`.
    pub id: &'static str,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
    pub scan: ScanMode,
    /// Actual frames per second delivered to the encoder / payloader.
    pub frame_rate: u32,
    /// Rate as it appears in the profile name (field rate when interlaced).
    pub display_rate: u32,
    /// Target video bitrate in kbps for H.264 (compressed SRT delivery).
    pub h264_kbps: u32,
    /// Target video bitrate in kbps for HEVC (experimental, ~30% lower).
    pub hevc_kbps: u32,
    /// Approximate uncompressed 4:2:2 10-bit ST 2110-20 bandwidth in Mbit/s.
    pub st2110_mbps: u32,
}

pub const PRESETS: &[QualityPreset] = &[
    QualityPreset {
        id: "4Kp60",
        label: "4K p60",
        width: 3840,
        height: 2160,
        scan: ScanMode::Progressive,
        frame_rate: 60,
        display_rate: 60,
        h264_kbps: 60000,
        hevc_kbps: 42000,
        // 3840x2160 * 20bpp * 60 ~= 9.95 Gbit/s
        st2110_mbps: 9953,
    },
    QualityPreset {
        id: "4Kp50",
        label: "4K p50",
        width: 3840,
        height: 2160,
        scan: ScanMode::Progressive,
        frame_rate: 50,
        display_rate: 50,
        h264_kbps: 55000,
        hevc_kbps: 38000,
        st2110_mbps: 8294,
    },
    QualityPreset {
        id: "1080p60",
        label: "1080 p60",
        width: 1920,
        height: 1080,
        scan: ScanMode::Progressive,
        frame_rate: 60,
        display_rate: 60,
        h264_kbps: 12000,
        hevc_kbps: 8400,
        // 1920x1080 * 20bpp * 60 ~= 2.49 Gbit/s
        st2110_mbps: 2488,
    },
    QualityPreset {
        id: "1080p50",
        label: "1080 p50",
        width: 1920,
        height: 1080,
        scan: ScanMode::Progressive,
        frame_rate: 50,
        display_rate: 50,
        h264_kbps: 10000,
        hevc_kbps: 7000,
        st2110_mbps: 2074,
    },
    QualityPreset {
        id: "4Ki60",
        label: "4K i60",
        width: 3840,
        height: 2160,
        scan: ScanMode::Interlaced,
        frame_rate: 30,
        display_rate: 60,
        h264_kbps: 40000,
        hevc_kbps: 28000,
        st2110_mbps: 4977,
    },
    QualityPreset {
        id: "4Ki50",
        label: "4K i50",
        width: 3840,
        height: 2160,
        scan: ScanMode::Interlaced,
        frame_rate: 25,
        display_rate: 50,
        h264_kbps: 36000,
        hevc_kbps: 25000,
        st2110_mbps: 4147,
    },
    QualityPreset {
        id: "1080i60",
        label: "1080 i60",
        width: 1920,
        height: 1080,
        scan: ScanMode::Interlaced,
        frame_rate: 30,
        display_rate: 60,
        h264_kbps: 8000,
        hevc_kbps: 5600,
        st2110_mbps: 1244,
    },
    QualityPreset {
        id: "1080i50",
        label: "1080 i50",
        width: 1920,
        height: 1080,
        scan: ScanMode::Interlaced,
        frame_rate: 25,
        display_rate: 50,
        h264_kbps: 7000,
        hevc_kbps: 4900,
        st2110_mbps: 1037,
    },
];

/// Default profile id used on first run.
pub const DEFAULT_PRESET_ID: &str = "1080p60";

pub fn find(id: &str) -> Option<&'static QualityPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Resolve a profile id, falling back to the default if unknown.
pub fn resolve_or_default(id: &str) -> &'static QualityPreset {
    find(id).unwrap_or_else(|| find(DEFAULT_PRESET_ID).expect("default preset exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(len, ids.len(), "preset ids must be unique");
    }

    #[test]
    fn default_preset_exists() {
        assert!(find(DEFAULT_PRESET_ID).is_some());
    }

    #[test]
    fn unknown_preset_falls_back() {
        assert_eq!(resolve_or_default("nope").id, DEFAULT_PRESET_ID);
    }

    #[test]
    fn hevc_targets_are_lower_than_h264() {
        for p in PRESETS {
            assert!(p.hevc_kbps < p.h264_kbps, "{} hevc >= h264", p.id);
        }
    }

    #[test]
    fn all_requested_broadcast_profiles_present() {
        for id in [
            "4Kp60", "4Kp50", "1080p60", "1080p50", "4Ki60", "4Ki50", "1080i60", "1080i50",
        ] {
            assert!(find(id).is_some(), "missing broadcast profile {id}");
        }
    }

    #[test]
    fn interlaced_frame_rate_is_half_field_rate() {
        for p in PRESETS.iter().filter(|p| p.scan.is_interlaced()) {
            assert_eq!(
                p.frame_rate * 2,
                p.display_rate,
                "{} interlaced frame rate should be half the field rate",
                p.id
            );
        }
    }

    #[test]
    fn progressive_frame_rate_matches_display_rate() {
        for p in PRESETS.iter().filter(|p| !p.scan.is_interlaced()) {
            assert_eq!(p.frame_rate, p.display_rate, "{}", p.id);
        }
    }

    #[test]
    fn four_k_profiles_are_uhd_raster() {
        for p in PRESETS.iter().filter(|p| p.id.starts_with("4K")) {
            assert_eq!((p.width, p.height), (3840, 2160), "{}", p.id);
        }
    }
}
