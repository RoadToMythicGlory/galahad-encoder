//! Tauri command surface + shared application state.
//!
//! The same start/stop/restart/switch primitives back both the UI commands and
//! the Control Channel dispatcher, so an operator can drive the client remotely.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::audio::AudioPlan;
use crate::capability::{self, Capabilities};
use crate::config::{ClientConfig, Identity, TransportKind};
use crate::control_channel::{ClientMessage, CommandAck, ServerCommand};
use crate::control_runtime::ControlHandle;
use crate::encoder::{self, Codec};
use crate::error::{EncoderError, Result};
use crate::gst2110::{self, EssenceTarget, Gst2110Audio, Gst2110Plan};
use crate::logger::LogBuffer;
use crate::pipeline::{OutputPlan, Pipeline, PipelineConfig, StreamStatus};
use crate::presets::{self, QualityPreset};
use crate::process_enum::ProcessInfo;
use crate::srt::SrtDestination;
use crate::video_device::VideoDeviceInfo;
use crate::window_enum::{self, WindowInfo};

pub struct AppStateInner {
    pub config: Mutex<ClientConfig>,
    pub config_path: PathBuf,
    pub caps: Mutex<Capabilities>,
    pub pipeline: Mutex<Option<Pipeline>>,
    pub identity: Identity,
    pub ffmpeg_path: PathBuf,
    pub logs: LogBuffer,
    pub control: Mutex<Option<ControlHandle>>,
    /// Window the player picked (id + label), set from the UI before start.
    pub selected_window: Mutex<Option<(isize, String)>>,
}

#[derive(Clone)]
pub struct AppState(pub Arc<AppStateInner>);

impl AppStateInner {
    fn build_pipeline_config(&self) -> Result<PipelineConfig> {
        let config = self.config.lock().unwrap().clone();
        let caps = self.caps.lock().unwrap().clone();
        let preset = presets::resolve_or_default(&config.preset_id);

        match config.transport {
            TransportKind::Srt => self.build_srt_config(&config, &caps, preset),
            TransportKind::St2110 => self.build_st2110_config(&config, &caps, preset),
        }
    }

    /// SRT path: FFmpeg encode + SRT. Video comes either from a camera / capture
    /// card (DirectShow) or, for the legacy game path, a captured window (WGC).
    fn build_srt_config(
        &self,
        config: &ClientConfig,
        caps: &Capabilities,
        preset: &QualityPreset,
    ) -> Result<PipelineConfig> {
        let destination = SrtDestination::parse(
            &config.destination_host,
            config.destination_port,
            config.latency_ms,
            config.srt_mode,
        )?;

        // Codec preference: experimental HEVC only if explicitly enabled, else H264.
        let preferred = if config.encoder.experimental_hevc {
            Codec::Hevc
        } else {
            Codec::H264
        };
        let backend = encoder::select(preferred, &caps.encoders, config.encoder.allow_software)
            .or_else(|| {
                encoder::select(Codec::H264, &caps.encoders, config.encoder.allow_software)
            })
            .ok_or_else(|| {
                EncoderError::Encoder(
                    "no compatible hardware encoder found. Update GPU drivers or enable the \
                     software fallback in settings."
                        .into(),
                )
            })?;

        let video_kbps = match backend.codec {
            Codec::H264 => preset.h264_kbps,
            Codec::Hevc => preset.hevc_kbps,
        };

        // Multi-caller fan-out only applies when we listen; a caller always has
        // a single output.
        let max_callers = match config.srt_mode {
            crate::srt::SrtMode::Listener => {
                config.srt_max_callers.clamp(1, crate::config::MAX_SRT_CALLERS)
            }
            crate::srt::SrtMode::Caller => 1,
        };

        // Choose the video source: a physical capture device is the broadcast
        // default; a window is the legacy game-capture path.
        let device_name = config
            .video_source
            .device_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let use_device = matches!(
            config.video_source.kind,
            crate::config::VideoSourceKind::Device
        ) && device_name.is_some();

        let (source_label, output, audio_kbps) = if use_device {
            let device_name = device_name.unwrap().to_string();
            (
                format!("{device_name} ({})", preset.label),
                OutputPlan::SrtDevice {
                    device_name,
                    backend,
                    destination,
                    max_callers,
                },
                0,
            )
        } else {
            let (window_id, label) = self
                .selected_window
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| {
                    EncoderError::Config(
                        "no capture source selected. Pick a camera / capture card (or a window)."
                            .into(),
                    )
                })?;
            let audio_plan = build_audio_plan(config);
            (
                label,
                OutputPlan::SrtWindow {
                    window_id,
                    backend,
                    destination,
                    max_callers,
                    audio_plan,
                },
                160,
            )
        };

        Ok(PipelineConfig {
            source_label,
            fps: preset.frame_rate,
            out_width: preset.width,
            out_height: preset.height,
            video_kbps,
            audio_kbps,
            output,
        })
    }

    /// Broadcast IP path: GStreamer device capture -> ST 2110 RTP.
    fn build_st2110_config(
        &self,
        config: &ClientConfig,
        caps: &Capabilities,
        preset: &QualityPreset,
    ) -> Result<PipelineConfig> {
        if !caps.gstreamer_available {
            return Err(EncoderError::Config(
                "ST 2110 output requires GStreamer, which was not found. Install GStreamer \
                 (with the RTP / rawvideo plugins) or switch the transport to SRT."
                    .into(),
            ));
        }
        let gstreamer_path = PathBuf::from(&caps.gstreamer_path);

        let device_name = config
            .video_source
            .device_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                EncoderError::Config(
                    "no capture device selected. Pick a camera or capture card for ST 2110 output."
                        .into(),
                )
            })?;

        let st = &config.st2110;
        let video = EssenceTarget::parse(&st.video_dest_ip, st.video_dest_port)?;

        let audio = if st.audio_enabled {
            let target = EssenceTarget::parse(&st.audio_dest_ip, st.audio_dest_port)?;
            // Audio uses the next dynamic payload type after video.
            let audio_pt = st.payload_type.checked_add(1).unwrap_or(97);
            Some(Gst2110Audio {
                target,
                payload_type: audio_pt,
                sample_rate: gst2110::AUDIO_SAMPLE_RATE,
                channels: gst2110::AUDIO_CHANNELS,
            })
        } else {
            None
        };

        let interface = if st.interface_ip.trim().is_empty() {
            None
        } else {
            Some(st.interface_ip.trim().to_string())
        };

        let gst = Gst2110Plan::from_profile(
            device_name,
            preset,
            video,
            st.payload_type,
            st.ptp_domain,
            interface,
            audio,
        )?;

        let sdp = gst.to_sdp(&st.interface_ip);

        Ok(PipelineConfig {
            source_label: format!("{device_name} ({})", preset.label),
            fps: preset.frame_rate,
            out_width: preset.width,
            out_height: preset.height,
            // Report the uncompressed ST 2110-20 rate (Mbit/s -> kbps) so the UI
            // reflects the real wire bandwidth this profile needs.
            video_kbps: preset.st2110_mbps.saturating_mul(1000),
            audio_kbps: 0,
            output: OutputPlan::St2110 {
                gst,
                gstreamer_path,
                sdp,
            },
        })
    }

    pub fn start(&self) -> Result<()> {
        let config = self.build_pipeline_config()?;
        let pipeline = Pipeline::start(config, self.ffmpeg_path.clone());
        let mut slot = self.pipeline.lock().unwrap();
        if let Some(mut old) = slot.take() {
            old.stop();
        }
        *slot = Some(pipeline);
        log::info!("stream started");
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(mut pipeline) = self.pipeline.lock().unwrap().take() {
            pipeline.stop();
            log::info!("stream stopped");
        }
    }

    pub fn restart(&self) -> Result<()> {
        let needs_new = {
            let slot = self.pipeline.lock().unwrap();
            slot.as_ref().map(|p| p.restart()).is_none()
        };
        if needs_new {
            self.start()
        } else {
            log::info!("stream restart requested");
            Ok(())
        }
    }

    pub fn switch_quality(&self, preset_id: &str) -> Result<()> {
        if presets::find(preset_id).is_none() {
            return Err(EncoderError::Config(format!("unknown preset '{preset_id}'")));
        }
        self.config.lock().unwrap().preset_id = preset_id.to_string();
        self.persist()?;

        let new_config = self.build_pipeline_config()?;
        let slot = self.pipeline.lock().unwrap();
        match slot.as_ref() {
            Some(pipeline) => pipeline.update_config(new_config),
            None => {} // not streaming; preset is saved for next start
        }
        log::info!("quality switched to {preset_id}");
        Ok(())
    }

    pub fn status(&self) -> StreamStatus {
        self.pipeline
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.status())
            .unwrap_or_default()
    }

    pub fn persist(&self) -> Result<()> {
        let config = self.config.lock().unwrap().clone();
        config.save(&self.config_path)
    }
}

fn build_audio_plan(config: &ClientConfig) -> AudioPlan {
    let game_on = config.audio.game.enabled && !config.audio.game.muted;
    let discord_on = config.audio.discord.enabled && !config.audio.discord.muted;
    let mic_on = config.audio.microphone.enabled && !config.audio.microphone.muted;

    // Without per-process isolation, game + Discord share the system render
    // loopback; combine their gains.
    let mut gains = Vec::new();
    if game_on {
        gains.push(config.audio.game.gain);
    }
    if discord_on {
        gains.push(config.audio.discord.gain);
    }
    let system_gain = if gains.is_empty() {
        1.0
    } else {
        gains.iter().sum::<f32>() / gains.len() as f32
    };

    AudioPlan {
        system_loopback: game_on || discord_on,
        system_gain,
        microphone: mic_on,
        microphone_device_id: config.audio.microphone_device_id.clone(),
        microphone_gain: config.audio.microphone.gain,
    }
}

/// Build a telemetry message from current status + identity.
pub fn telemetry_message(inner: &AppStateInner) -> ClientMessage {
    let status = inner.status();
    let streaming = matches!(
        status.state,
        crate::stream_state::StreamState::Streaming | crate::stream_state::StreamState::Degraded
    );
    ClientMessage::Telemetry(crate::control_channel::Telemetry {
        player_id: inner.identity.player_id.clone(),
        session_id: inner.identity.session_id.clone(),
        display_name: inner.identity.display_name.clone(),
        source: status.source,
        streaming,
        state: status.state,
        fps: status.fps,
        bitrate: status.bitrate,
        dropped_frames: status.dropped_frames,
        encoder: status.encoder,
        capture_backend: status.capture_backend,
        last_error: status.last_error,
    })
}

/// Dispatch an operator command and emit an ack on the Control Channel.
pub fn dispatch_command(inner: &Arc<AppStateInner>, cmd: ServerCommand) {
    let action = cmd.action_name().to_string();
    let result: Result<Option<String>> = match cmd {
        ServerCommand::StartStream => inner.start().map(|_| None),
        ServerCommand::StopStream => {
            inner.stop();
            Ok(None)
        }
        ServerCommand::RestartStream => inner.restart().map(|_| None),
        ServerCommand::SwitchQuality { preset } => inner.switch_quality(&preset).map(|_| None),
        ServerCommand::RequestLogUpload => {
            if let Some(handle) = inner.control.lock().unwrap().as_ref() {
                handle.send(ClientMessage::LogUpload {
                    session_id: inner.identity.session_id.clone(),
                    lines: inner.logs.snapshot(),
                });
            }
            Ok(Some("log upload sent".into()))
        }
        ServerCommand::PushConfig { config } => apply_pushed_config(inner, config),
    };

    let ack = match result {
        Ok(detail) => CommandAck {
            action,
            ok: true,
            detail,
        },
        Err(e) => CommandAck {
            action,
            ok: false,
            detail: Some(e.to_string()),
        },
    };
    if let Some(handle) = inner.control.lock().unwrap().as_ref() {
        handle.send(ClientMessage::CommandAck(ack));
    }
}

fn apply_pushed_config(inner: &Arc<AppStateInner>, value: serde_json::Value) -> Result<Option<String>> {
    // Merge only known, safe fields from the pushed config.
    {
        let mut config = inner.config.lock().unwrap();
        if let Some(preset) = value.get("presetId").and_then(|v| v.as_str()) {
            if presets::find(preset).is_some() {
                config.preset_id = preset.to_string();
            }
        }
        if let Some(host) = value.get("destinationHost").and_then(|v| v.as_str()) {
            config.destination_host = host.to_string();
        }
        if let Some(port) = value.get("destinationPort").and_then(|v| v.as_u64()) {
            config.destination_port = port as u32;
        }
        if let Some(latency) = value.get("latencyMs").and_then(|v| v.as_u64()) {
            config.latency_ms = latency as u32;
        }
    }
    inner.persist()?;
    Ok(Some("config applied".into()))
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_capabilities(state: tauri::State<AppState>) -> Capabilities {
    state.0.caps.lock().unwrap().clone()
}

#[tauri::command]
pub fn refresh_capabilities(state: tauri::State<AppState>) -> Capabilities {
    let caps = capability::detect();
    *state.0.caps.lock().unwrap() = caps.clone();
    caps
}

#[tauri::command]
pub fn list_windows() -> Vec<WindowInfo> {
    window_enum::list_windows()
}

#[tauri::command]
pub fn list_microphones() -> Vec<crate::audio::MicrophoneInfo> {
    crate::audio::list_microphones()
}

#[tauri::command]
pub fn list_video_devices(state: tauri::State<AppState>) -> Vec<VideoDeviceInfo> {
    let ffmpeg = state.0.ffmpeg_path.to_string_lossy().to_string();
    crate::video_device::list_video_devices(&ffmpeg)
}

/// Best-effort local + public IP discovery for pre-filling the listener address.
#[tauri::command]
pub fn detect_ip() -> crate::net_info::IpInfo {
    crate::net_info::detect()
}

#[tauri::command]
pub fn discord_processes() -> Vec<ProcessInfo> {
    crate::process_enum::discord_processes()
}

#[tauri::command]
pub fn get_config(state: tauri::State<AppState>) -> ClientConfig {
    state.0.config.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_config(state: tauri::State<AppState>, config: ClientConfig) -> Result<()> {
    *state.0.config.lock().unwrap() = config;
    state.0.persist()
}

#[tauri::command]
pub fn get_identity(state: tauri::State<AppState>) -> Identity {
    state.0.identity.clone()
}

#[tauri::command]
pub fn select_window(state: tauri::State<AppState>, window_id: i64, label: String) {
    *state.0.selected_window.lock().unwrap() = Some((window_id as isize, label));
}

#[tauri::command]
pub fn get_status(state: tauri::State<AppState>) -> StreamStatus {
    state.0.status()
}

#[tauri::command]
pub fn start_stream(state: tauri::State<AppState>) -> Result<()> {
    state.0.start()
}

#[tauri::command]
pub fn stop_stream(state: tauri::State<AppState>) {
    state.0.stop();
}

#[tauri::command]
pub fn restart_stream(state: tauri::State<AppState>) -> Result<()> {
    state.0.restart()
}

#[tauri::command]
pub fn switch_quality(state: tauri::State<AppState>, preset: String) -> Result<()> {
    state.0.switch_quality(&preset)
}

#[tauri::command]
pub fn get_logs(state: tauri::State<AppState>) -> Vec<String> {
    state.0.logs.snapshot()
}
