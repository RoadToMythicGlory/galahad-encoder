//! Physical video capture device enumeration.
//!
//! Broadcast sources are cameras and capture cards exposed to Windows, not game
//! windows. We enumerate them through the bundled FFmpeg's DirectShow backend
//! (`ffmpeg -f dshow -list_devices true -i dummy`) and key devices by their
//! DirectShow *friendly name*. That name is the common identifier accepted by
//! both capture paths this app uses:
//!   * FFmpeg   -> `-f dshow -i video=<name>`
//!   * GStreamer -> `dshowvideosrc device-name=<name>`
//!
//! Keeping a single device identity across both encoders avoids a translation
//! layer between the SRT (FFmpeg) and ST 2110 (GStreamer) pipelines.

use std::process::Command;

use serde::Serialize;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDeviceInfo {
    /// Stable identifier = DirectShow friendly name.
    pub id: String,
    pub name: String,
}

/// Parse the stderr of `ffmpeg -f dshow -list_devices true -i dummy`.
///
/// Two output layouts exist across FFmpeg versions:
///
/// **FFmpeg < 7** groups devices under section headers:
/// ```text
/// [dshow @ ...] DirectShow video devices (some may be both video and audio)
/// [dshow @ ...]  "Logitech BRIO"
/// [dshow @ ...]     Alternative name "@device_pnp_\\?\usb#..."
/// [dshow @ ...] DirectShow audio devices
/// [dshow @ ...]  "Microphone (Realtek)"
/// ```
///
/// **FFmpeg >= 7** dropped the section headers and instead tags each device
/// inline with its media type:
/// ```text
/// [in#0 @ ...] "Integrated Webcam" (video)
/// [in#0 @ ...]   Alternative name "@device_pnp_\\?\usb#..."
/// [in#0 @ ...] "LSVCam" (none)
/// [in#0 @ ...] "Microphone Array (...)" (audio)
/// ```
///
/// We keep only devices that expose a video pin: the inline `(video)` tag when
/// present, otherwise the video section for the older layout. `(audio)` and
/// `(none)` devices are skipped.
pub fn parse_dshow_devices(stderr: &str) -> Vec<VideoDeviceInfo> {
    let mut devices = Vec::new();
    let mut in_video_section = false;

    for line in stderr.lines() {
        let content = strip_dshow_prefix(line);
        let trimmed = content.trim();
        let lower = trimmed.to_lowercase();

        // Old-format section headers (ignored by FFmpeg >= 7, which omits them).
        if lower.contains("directshow video devices") {
            in_video_section = true;
            continue;
        }
        if lower.contains("directshow audio devices") {
            in_video_section = false;
            continue;
        }
        // Skip the "Alternative name \"...\"" lines; keep only friendly names.
        if lower.starts_with("alternative name") {
            continue;
        }

        let name = match extract_quoted(trimmed) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };

        // Prefer the FFmpeg >= 7 inline media tag; fall back to the section for
        // the older grouped layout when no tag is present.
        let is_video = if lower.ends_with("(video)") {
            true
        } else if lower.ends_with("(audio)") || lower.ends_with("(none)") {
            false
        } else {
            in_video_section
        };

        if is_video && !devices.iter().any(|d: &VideoDeviceInfo| d.name == name) {
            devices.push(VideoDeviceInfo {
                id: name.clone(),
                name,
            });
        }
    }

    devices
}

/// Strip a leading `[dshow @ 0x...] ` log prefix if present.
fn strip_dshow_prefix(line: &str) -> &str {
    match line.find(']') {
        Some(idx) if line.trim_start().starts_with('[') => &line[idx + 1..],
        _ => line,
    }
}

/// Return the text inside the first pair of double quotes, if any.
fn extract_quoted(text: &str) -> Option<String> {
    let start = text.find('"')?;
    let rest = &text[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Enumerate video capture devices using the given FFmpeg executable.
pub fn list_video_devices(ffmpeg: &str) -> Vec<VideoDeviceInfo> {
    if !cfg!(windows) {
        return Vec::new();
    }

    let mut command = Command::new(ffmpeg);
    command.args([
        "-hide_banner",
        "-f",
        "dshow",
        "-list_devices",
        "true",
        "-i",
        "dummy",
    ]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    match command.output() {
        // FFmpeg exits non-zero for the `dummy` input; the listing is on stderr
        // regardless, so we parse stderr unconditionally.
        Ok(out) => parse_dshow_devices(&String::from_utf8_lossy(&out.stderr)),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[dshow @ 000001] DirectShow video devices (some may be both video and audio devices)
[dshow @ 000001]  "Logitech BRIO"
[dshow @ 000001]     Alternative name "@device_pnp_\\?\usb#vid_046d"
[dshow @ 000001]  "Blackmagic WDM Capture"
[dshow @ 000001]     Alternative name "@device_pnp_\\?\pci#ven_1cfa"
[dshow @ 000001] DirectShow audio devices
[dshow @ 000001]  "Microphone (Realtek High Definition Audio)"
[dshow @ 000001]     Alternative name "@device_cm_{guid}"
"#;

    #[test]
    fn parses_only_video_friendly_names() {
        let devices = parse_dshow_devices(SAMPLE);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "Logitech BRIO");
        assert_eq!(devices[1].name, "Blackmagic WDM Capture");
    }

    #[test]
    fn ignores_alternative_names_and_audio_section() {
        let devices = parse_dshow_devices(SAMPLE);
        assert!(devices.iter().all(|d| !d.name.starts_with('@')));
        assert!(devices
            .iter()
            .all(|d| !d.name.to_lowercase().contains("microphone")));
    }

    #[test]
    fn id_matches_friendly_name() {
        let devices = parse_dshow_devices(SAMPLE);
        assert!(devices.iter().all(|d| d.id == d.name));
    }

    #[test]
    fn dedupes_repeated_devices() {
        let doubled = format!("{SAMPLE}{SAMPLE}");
        let devices = parse_dshow_devices(&doubled);
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn empty_on_no_devices() {
        let listing = "[dshow @ 1] DirectShow video devices\n[dshow @ 1] DirectShow audio devices\n";
        assert!(parse_dshow_devices(listing).is_empty());
    }

    // FFmpeg >= 7 layout: no section headers, inline (video)/(audio)/(none) tags.
    const SAMPLE_V7: &str = r#"[in#0 @ 0001] "Integrated Webcam" (video)
[in#0 @ 0001]   Alternative name "@device_pnp_\\?\usb#vid_0c45"
[in#0 @ 0001] "LSVCam" (none)
[in#0 @ 0001]   Alternative name "@device_sw_{860BB310}\LSVCam"
[in#0 @ 0001] "OBS Virtual Camera" (video)
[in#0 @ 0001]   Alternative name "@device_sw_{860BB310}\{A3FCE0F5}"
[in#0 @ 0001] "DouWan Camera5 (1920x1080)" (video)
[in#0 @ 0001]   Alternative name "@device_sw_{860BB310}\{A3FCE0EE}"
[in#0 @ 0001] "Microphone Array (Intel Smart Sound)" (audio)
[in#0 @ 0001]   Alternative name "@device_cm_{33D9A762}\wave_{A8AF644C}"
"#;

    #[test]
    fn parses_ffmpeg7_inline_tagged_video_devices() {
        let devices = parse_dshow_devices(SAMPLE_V7);
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Integrated Webcam",
                "OBS Virtual Camera",
                "DouWan Camera5 (1920x1080)"
            ]
        );
    }

    #[test]
    fn ffmpeg7_skips_audio_and_none_devices() {
        let devices = parse_dshow_devices(SAMPLE_V7);
        assert!(devices.iter().all(|d| d.name != "LSVCam"));
        assert!(devices
            .iter()
            .all(|d| !d.name.to_lowercase().contains("microphone")));
    }

    #[test]
    fn handles_lines_without_log_prefix() {
        let listing = "DirectShow video devices\n \"Camera One\"\nDirectShow audio devices\n";
        let devices = parse_dshow_devices(listing);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Camera One");
    }
}
