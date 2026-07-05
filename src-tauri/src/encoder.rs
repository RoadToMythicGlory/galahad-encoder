//! Encoder backend selection.
//!
//! `EncoderBackend` exists from day one (per the plan): H.264 is the default,
//! HEVC is experimental Phase 2. Hardware vendor selection (NVENC / QSV / AMF) is
//! isolated here behind a capability probe rather than scattered across the app.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn as_str(&self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }
}

/// A concrete FFmpeg encoder name + the hardware family it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderBackend {
    pub codec: Codec,
    /// FFmpeg encoder, e.g. `h264_nvenc`, `hevc_qsv`, `libx264`.
    pub ffmpeg_name: &'static str,
    pub vendor: Vendor,
    /// Software encoders are a last resort; they can overload the machine.
    pub hardware: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Intel,
    Amd,
    Software,
}

/// Preference order for H.264, best (lowest server + client cost) first.
const H264_PREFERENCE: &[EncoderBackend] = &[
    EncoderBackend {
        codec: Codec::H264,
        ffmpeg_name: "h264_nvenc",
        vendor: Vendor::Nvidia,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::H264,
        ffmpeg_name: "h264_qsv",
        vendor: Vendor::Intel,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::H264,
        ffmpeg_name: "h264_amf",
        vendor: Vendor::Amd,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::H264,
        ffmpeg_name: "libx264",
        vendor: Vendor::Software,
        hardware: false,
    },
];

/// Preference order for HEVC (experimental).
const HEVC_PREFERENCE: &[EncoderBackend] = &[
    EncoderBackend {
        codec: Codec::Hevc,
        ffmpeg_name: "hevc_nvenc",
        vendor: Vendor::Nvidia,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::Hevc,
        ffmpeg_name: "hevc_qsv",
        vendor: Vendor::Intel,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::Hevc,
        ffmpeg_name: "hevc_amf",
        vendor: Vendor::Amd,
        hardware: true,
    },
    EncoderBackend {
        codec: Codec::Hevc,
        ffmpeg_name: "libx265",
        vendor: Vendor::Software,
        hardware: false,
    },
];

fn preference(codec: Codec) -> &'static [EncoderBackend] {
    match codec {
        Codec::H264 => H264_PREFERENCE,
        Codec::Hevc => HEVC_PREFERENCE,
    }
}

/// Select the best available encoder for a codec given the set of FFmpeg encoder
/// names the local FFmpeg build reports as available.
///
/// `allow_software` gates the CPU fallback. For normal broadcast use we keep it
/// off so a machine without a hardware encoder fails loudly instead of melting.
pub fn select(
    codec: Codec,
    available: &[String],
    allow_software: bool,
) -> Option<EncoderBackend> {
    preference(codec).iter().copied().find(|backend| {
        if !backend.hardware && !allow_software {
            return false;
        }
        available.iter().any(|name| name == backend.ffmpeg_name)
    })
}

/// All encoders (across both codecs) that are available locally. Used to drive
/// the capability payload and the UI's preset gating.
pub fn available_backends(available: &[String]) -> Vec<EncoderBackend> {
    H264_PREFERENCE
        .iter()
        .chain(HEVC_PREFERENCE.iter())
        .copied()
        .filter(|b| available.iter().any(|name| name == b.ffmpeg_name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefers_nvenc_for_h264() {
        let avail = names(&["h264_qsv", "h264_nvenc", "libx264"]);
        let chosen = select(Codec::H264, &avail, false).unwrap();
        assert_eq!(chosen.ffmpeg_name, "h264_nvenc");
        assert_eq!(chosen.vendor, Vendor::Nvidia);
    }

    #[test]
    fn falls_back_to_qsv_then_amf() {
        let avail = names(&["h264_amf", "h264_qsv"]);
        let chosen = select(Codec::H264, &avail, false).unwrap();
        assert_eq!(chosen.ffmpeg_name, "h264_qsv");
    }

    #[test]
    fn software_excluded_unless_allowed() {
        let avail = names(&["libx264"]);
        assert!(select(Codec::H264, &avail, false).is_none());
        let chosen = select(Codec::H264, &avail, true).unwrap();
        assert_eq!(chosen.ffmpeg_name, "libx264");
        assert!(!chosen.hardware);
    }

    #[test]
    fn hevc_selection_is_independent() {
        let avail = names(&["h264_nvenc", "hevc_qsv"]);
        assert_eq!(
            select(Codec::Hevc, &avail, false).unwrap().ffmpeg_name,
            "hevc_qsv"
        );
    }

    #[test]
    fn available_backends_lists_all_present() {
        let avail = names(&["h264_nvenc", "hevc_nvenc", "libx264"]);
        let backends = available_backends(&avail);
        assert_eq!(backends.len(), 3);
    }
}
