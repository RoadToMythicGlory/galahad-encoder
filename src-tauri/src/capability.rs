//! Startup capability detection.
//!
//! The UI is built from what the machine can actually do, instead of scattering
//! special cases for RTX / GTX / Intel / AMD / Windows 10 / 11 across the app.
//! Detection is best-effort: unknown/unsupported is represented explicitly rather
//! than assumed, and the runtime re-validates (e.g. audio activation) on start.

use std::process::Command;

use serde::Serialize;

use crate::audio::MicrophoneInfo;
use crate::encoder::{available_backends, EncoderBackend};
use crate::process_enum::{discord_processes, ProcessInfo};
use crate::video_device::{list_video_devices, VideoDeviceInfo};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    pub name: String,
    /// Best-effort OS build; `None` when it cannot be determined reliably.
    pub build: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// "wgc" when Windows Graphics Capture is the active backend, else "none".
    pub capture: String,
    /// FFmpeg encoder names available locally (filtered to ones we use).
    pub encoders: Vec<String>,
    /// Structured encoder backends (codec + vendor + hardware flag).
    pub encoder_backends: Vec<EncoderBackend>,
    /// Per-process audio loopback availability (best-effort; runtime re-checks).
    pub process_audio: bool,
    /// Whether at least one Discord process is currently running.
    pub discord_audio: bool,
    pub discord_processes: Vec<ProcessInfo>,
    pub microphones: Vec<MicrophoneInfo>,
    /// Physical capture devices (cameras / capture cards) for broadcast input.
    pub video_devices: Vec<VideoDeviceInfo>,
    /// GStreamer is present, enabling the ST 2110 output path.
    pub gstreamer_available: bool,
    pub gstreamer_path: String,
    /// ST 2110 output can be attempted: GStreamer present + at least one device.
    pub st2110_ready: bool,
    pub ffmpeg_available: bool,
    pub ffmpeg_path: String,
    pub os: OsInfo,
}

/// Known encoder names we care about, probed against the local FFmpeg build.
const KNOWN_ENCODERS: &[&str] = &[
    "h264_nvenc",
    "h264_qsv",
    "h264_amf",
    "libx264",
    "hevc_nvenc",
    "hevc_qsv",
    "hevc_amf",
    "libx265",
];

/// Parse the output of `ffmpeg -encoders` for the encoder names we support.
fn parse_encoders(stdout: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in stdout.lines() {
        // Lines look like: " V....D h264_nvenc           NVIDIA NVENC H.264 ..."
        let trimmed = line.trim_start();
        for name in KNOWN_ENCODERS {
            if trimmed
                .split_whitespace()
                .nth(1)
                .map(|tok| tok == *name)
                .unwrap_or(false)
            {
                found.push(name.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn probe_encoders(ffmpeg: &str) -> (bool, Vec<String>) {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            (true, parse_encoders(&stdout))
        }
        Err(_) => (false, Vec::new()),
    }
}

#[cfg(windows)]
fn detect_os() -> OsInfo {
    // CurrentBuildNumber is the most reliable build signal without a manifest
    // dependency; read it straight from the registry via `reg query`.
    let build = Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "/v",
            "CurrentBuildNumber",
        ])
        .output()
        .ok()
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            text.split_whitespace()
                .last()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    OsInfo {
        name: "Windows".into(),
        build,
    }
}

#[cfg(not(windows))]
fn detect_os() -> OsInfo {
    OsInfo {
        name: std::env::consts::OS.to_string(),
        build: None,
    }
}

/// Per-process loopback needs Windows 10 build 20348+ (Server 2022 / Win11 era
/// API surface; the AudioClient activation path also works on Win10 2004+ for
/// many drivers). We treat >= 19041 as "likely supported" and let the runtime
/// downgrade with a clear warning if activation actually fails.
fn detect_process_audio(os: &OsInfo) -> bool {
    if !cfg!(windows) {
        return false;
    }
    match os.build.as_ref().and_then(|b| b.parse::<u32>().ok()) {
        Some(build) => build >= 19041,
        None => true, // unknown: optimistic, runtime re-validates
    }
}

pub fn detect() -> Capabilities {
    let ffmpeg_path = crate::paths::locate_ffmpeg();
    let ffmpeg_str = ffmpeg_path.to_string_lossy().to_string();
    let (ffmpeg_available, encoders) = probe_encoders(&ffmpeg_str);
    let encoder_backends = available_backends(&encoders);

    let os = detect_os();
    let process_audio = detect_process_audio(&os);

    let discord = discord_processes();
    let microphones = crate::audio::list_microphones();

    let video_devices = if ffmpeg_available {
        list_video_devices(&ffmpeg_str)
    } else {
        Vec::new()
    };

    let gstreamer = crate::paths::locate_gstreamer();
    let gstreamer_available = gstreamer.is_some();
    let gstreamer_path = gstreamer
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let st2110_ready = gstreamer_available && !video_devices.is_empty();

    let capture = if cfg!(windows) { "wgc" } else { "none" }.to_string();

    Capabilities {
        capture,
        encoders,
        encoder_backends,
        process_audio,
        discord_audio: !discord.is_empty(),
        discord_processes: discord,
        microphones,
        video_devices,
        gstreamer_available,
        gstreamer_path,
        st2110_ready,
        ffmpeg_available,
        ffmpeg_path: ffmpeg_str,
        os,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_encoder_listing() {
        let sample = "\
Encoders:
 V..... = Video
 ------
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder
 V....D libx264              libx264 H.264 / AVC
 V....D hevc_nvenc           NVIDIA NVENC hevc encoder
 A....D aac                  AAC (Advanced Audio Coding)
";
        let found = parse_encoders(sample);
        assert!(found.contains(&"h264_nvenc".to_string()));
        assert!(found.contains(&"libx264".to_string()));
        assert!(found.contains(&"hevc_nvenc".to_string()));
        // aac is not in our known-video set.
        assert!(!found.contains(&"aac".to_string()));
    }

    #[test]
    fn process_audio_requires_modern_build() {
        let old = OsInfo {
            name: "Windows".into(),
            build: Some("17763".into()),
        };
        let modern = OsInfo {
            name: "Windows".into(),
            build: Some("22631".into()),
        };
        if cfg!(windows) {
            assert!(!detect_process_audio(&old));
            assert!(detect_process_audio(&modern));
        }
    }
}
