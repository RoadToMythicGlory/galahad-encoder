//! Persistent client configuration + player / session identity.
//!
//! `playerId` is stable across runs (identifies the human). `sessionId` is fresh
//! per app launch (identifies one connection lifecycle), so the operator can tell
//! "Idan reconnected / restarted PC" apart in the control plane. Neither is tied
//! to a matrix input or slot — routing is server-owned.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::presets::DEFAULT_PRESET_ID;
use crate::srt::{SrtMode, DEFAULT_LATENCY_MS};

/// Maximum callers an SRT listener may serve at once.
pub const MAX_SRT_CALLERS: u8 = 3;

fn default_max_callers() -> u8 {
    1
}

/// What a single mixer channel captures.
///
/// * `System`      - the default render endpoint loopback (whole desktop mix).
/// * `Microphone`  - a specific capture endpoint (by `device_id`, else default).
/// * `Application` - a single process's audio via Windows process loopback
///                   (captures the app tree rooted at `process_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioSourceKind {
    System,
    Microphone,
    Application,
}

impl Default for AudioSourceKind {
    fn default() -> Self {
        AudioSourceKind::System
    }
}

fn default_true() -> bool {
    true
}

fn default_gain() -> f32 {
    1.0
}

/// One channel in the audio mixer. Several may be active at once and are summed
/// (per-channel gain applied, then clamped) into the single broadcast track.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSource {
    /// Stable id for the UI (also used as a React key). Not interpreted natively.
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: AudioSourceKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub muted: bool,
    /// 0.0 - 2.0 linear gain. 1.0 is unity.
    #[serde(default = "default_gain")]
    pub gain: f32,
    /// For `Microphone`: the capture endpoint id (empty / None = default mic).
    #[serde(default)]
    pub device_id: Option<String>,
    /// For `Application`: the target process id whose audio to capture.
    #[serde(default)]
    pub process_id: Option<u32>,
    /// Human label shown in the UI (device / app name).
    #[serde(default)]
    pub label: Option<String>,
}

impl AudioSource {
    /// Whether this channel contributes to the mix (enabled and not muted).
    pub fn is_live(&self) -> bool {
        self.enabled && !self.muted
    }
}

/// The mixer configuration: an ordered list of channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    #[serde(default)]
    pub sources: Vec<AudioSource>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        // Seed with the desktop mix so program audio is captured out of the box;
        // users add microphones / per-app channels from the UI.
        Self {
            sources: vec![AudioSource {
                id: "system".into(),
                kind: AudioSourceKind::System,
                enabled: true,
                muted: false,
                gain: 1.0,
                device_id: None,
                process_id: None,
                label: Some("Desktop audio".into()),
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderConfig {
    /// Allow the CPU software fallback. Off by default for broadcast safety.
    pub allow_software: bool,
    /// Expose / use the experimental HEVC backend.
    pub experimental_hevc: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            allow_software: false,
            experimental_hevc: false,
        }
    }
}

/// Where the video comes from. `Window` is the legacy game-window capture path
/// (WGC); `Device` is a physical camera / capture card exposed to Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoSourceKind {
    Window,
    Device,
}

impl Default for VideoSourceKind {
    fn default() -> Self {
        VideoSourceKind::Window
    }
}

/// Generalized video source selection. Replaces the window-only model while
/// staying backward compatible: a `Window` source keeps using the persisted
/// title / process name so it re-resolves across launches (the HWND changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSourceConfig {
    #[serde(rename = "type", default)]
    pub kind: VideoSourceKind,
    /// For `Device`: the capture device's stable name (DirectShow friendly
    /// name, which both FFmpeg `-f dshow` and GStreamer `dshowvideosrc` accept).
    #[serde(default)]
    pub device_name: Option<String>,
    /// For `Window` (legacy): title + process, re-resolved on next launch.
    #[serde(default)]
    pub window_title: Option<String>,
    #[serde(default)]
    pub process_name: Option<String>,
}

impl Default for VideoSourceConfig {
    fn default() -> Self {
        Self {
            kind: VideoSourceKind::Window,
            device_name: None,
            window_title: None,
            process_name: None,
        }
    }
}

/// Output transport family. `Srt` is the legacy compressed uplink; `St2110` is
/// uncompressed SMPTE ST 2110 over RTP for in-facility broadcast IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind {
    Srt,
    St2110,
}

impl Default for TransportKind {
    fn default() -> Self {
        TransportKind::Srt
    }
}

/// SMPTE ST 2110 output configuration. Video (2110-20) is mandatory; audio
/// (2110-30) is optional. Destinations are typically multicast group addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct St2110Config {
    /// 2110-20 video RTP destination (multicast group or unicast host).
    pub video_dest_ip: String,
    pub video_dest_port: u16,
    /// 2110-30 audio RTP destination. Ignored when `audio_enabled` is false.
    pub audio_dest_ip: String,
    pub audio_dest_port: u16,
    /// Local NIC address to send from (empty = OS default route). Broadcast IP
    /// deployments usually pin this to the media NIC.
    pub interface_ip: String,
    /// Dynamic RTP payload type for the video essence (96-127).
    pub payload_type: u8,
    /// PTP (ST 2059) clock domain the media network runs on.
    pub ptp_domain: u8,
    /// Emit a 2110-30 PCM audio essence alongside video.
    pub audio_enabled: bool,
}

impl Default for St2110Config {
    fn default() -> Self {
        Self {
            video_dest_ip: "239.20.20.20".into(),
            video_dest_port: 20000,
            audio_dest_ip: "239.20.20.30".into(),
            audio_dest_port: 20030,
            interface_ip: String::new(),
            payload_type: 96,
            ptp_domain: 0,
            audio_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfig {
    /// Stable player identity (persisted).
    pub player_id: String,
    pub display_name: String,

    /// Destination the operator handed the player (caller mode), or the
    /// advisory address the player shares with the caller (listener mode).
    pub destination_host: String,
    pub destination_port: u32,
    pub latency_ms: u32,

    /// Whether this encoder calls out (default) or listens for a puller.
    #[serde(default)]
    pub srt_mode: SrtMode,

    /// In listener mode, how many callers may pull at once (1-3). Each caller
    /// connects to its own consecutive port starting at `destination_port`.
    #[serde(default = "default_max_callers")]
    pub srt_max_callers: u8,

    /// Quality / broadcast profile id (see `presets`).
    pub preset_id: String,

    /// Output transport family (SRT legacy vs ST 2110 broadcast IP).
    #[serde(default)]
    pub transport: TransportKind,

    /// SMPTE ST 2110 output settings (used when `transport == St2110`).
    #[serde(default)]
    pub st2110: St2110Config,

    /// Generalized video source (window capture or physical device).
    #[serde(default)]
    pub video_source: VideoSourceConfig,

    /// Selected capture source (window). Retained for backward compatibility and
    /// mirrored from / into `video_source` so older configs keep re-resolving.
    #[serde(default)]
    pub source_window_title: Option<String>,
    #[serde(default)]
    pub source_process_name: Option<String>,

    pub audio: AudioConfig,
    pub encoder: EncoderConfig,

    /// Control-plane Control Channel endpoint, e.g. `ws://1.2.3.4:8800/ws/encoder`.
    pub control_channel_url: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            player_id: format!("player-{}", &Uuid::new_v4().to_string()[..8]),
            display_name: String::new(),
            destination_host: String::new(),
            destination_port: 0,
            latency_ms: DEFAULT_LATENCY_MS,
            srt_mode: SrtMode::default(),
            srt_max_callers: default_max_callers(),
            preset_id: DEFAULT_PRESET_ID.to_string(),
            transport: TransportKind::default(),
            st2110: St2110Config::default(),
            video_source: VideoSourceConfig::default(),
            source_window_title: None,
            source_process_name: None,
            audio: AudioConfig::default(),
            encoder: EncoderConfig::default(),
            control_channel_url: None,
        }
    }
}

impl ClientConfig {
    /// Default on-disk location: `%APPDATA%/Galahad Encoder/config.json`.
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::config_dir().ok_or_else(|| {
            crate::error::EncoderError::Config("could not resolve config dir".into())
        })?;
        Ok(base.join("Galahad Encoder").join("config.json"))
    }

    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                log::warn!("config parse failed ({err}); using defaults");
                ClientConfig::default()
            }),
            Err(_) => ClientConfig::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

/// Identity emitted on the Control Channel. `session_id` is generated once per
/// process launch and never persisted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub player_id: String,
    pub session_id: String,
    pub display_name: String,
}

impl Identity {
    pub fn new(config: &ClientConfig) -> Self {
        Self {
            player_id: config.player_id.clone(),
            session_id: Uuid::new_v4().to_string(),
            display_name: config.display_name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_generate_player_id() {
        let cfg = ClientConfig::default();
        assert!(cfg.player_id.starts_with("player-"));
        assert_eq!(cfg.preset_id, DEFAULT_PRESET_ID);
    }

    #[test]
    fn round_trips_through_json() {
        let mut cfg = ClientConfig::default();
        cfg.display_name = "Idan".into();
        cfg.destination_host = "1.2.3.4".into();
        cfg.destination_port = 9003;
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ClientConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.display_name, "Idan");
        assert_eq!(back.destination_port, 9003);
    }

    #[test]
    fn round_trips_broadcast_source_and_transport() {
        let mut cfg = ClientConfig::default();
        cfg.transport = TransportKind::St2110;
        cfg.video_source = VideoSourceConfig {
            kind: VideoSourceKind::Device,
            device_name: Some("Blackmagic WDM Capture".into()),
            window_title: None,
            process_name: None,
        };
        cfg.st2110.video_dest_ip = "239.1.2.3".into();
        cfg.st2110.video_dest_port = 30000;
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ClientConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.transport, TransportKind::St2110);
        assert_eq!(back.video_source.kind, VideoSourceKind::Device);
        assert_eq!(
            back.video_source.device_name.as_deref(),
            Some("Blackmagic WDM Capture")
        );
        assert_eq!(back.st2110.video_dest_port, 30000);
    }

    #[test]
    fn legacy_config_without_broadcast_fields_defaults() {
        // A config persisted before broadcast support must still load.
        let legacy = r#"{
            "playerId": "player-old",
            "displayName": "Old",
            "destinationHost": "1.2.3.4",
            "destinationPort": 9003,
            "latencyMs": 200,
            "presetId": "1080p60",
            "sourceWindowTitle": null,
            "sourceProcessName": null,
            "audio": {
                "game": {"enabled": true, "muted": false, "gain": 1.0},
                "discord": {"enabled": true, "muted": false, "gain": 1.0},
                "microphone": {"enabled": true, "muted": false, "gain": 1.0},
                "microphoneDeviceId": null,
                "discordProcessId": null
            },
            "encoder": {"allowSoftware": false, "experimentalHevc": false},
            "controlChannelUrl": null
        }"#;
        let cfg: ClientConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(cfg.transport, TransportKind::Srt);
        assert_eq!(cfg.video_source.kind, VideoSourceKind::Window);
        assert_eq!(cfg.st2110.payload_type, 96);
    }

    #[test]
    fn identity_session_is_unique_per_call() {
        let cfg = ClientConfig::default();
        let a = Identity::new(&cfg);
        let b = Identity::new(&cfg);
        assert_eq!(a.player_id, b.player_id);
        assert_ne!(a.session_id, b.session_id);
    }
}
