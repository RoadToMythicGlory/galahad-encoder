//! Galahad Encoder library entry point.
//!
//! Wires the capability scan, persistent config, identity, streaming pipeline and
//! bidirectional Control Channel into a Tauri application.

mod audio;
mod capability;
mod capture;
mod commands;
mod config;
mod control_channel;
mod control_runtime;
mod encoder;
mod error;
mod ffmpeg;
mod gst2110;
mod logger;
mod net_info;
mod paths;
mod pipeline;
mod presets;
mod preview;
mod process_enum;
mod srt;
mod stream_state;
mod video_device;
mod window_enum;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use commands::{AppState, AppStateInner};
use config::{ClientConfig, Identity};
use control_channel::ClientMessage;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Logger first so startup is captured.
    let log_path = paths::log_dir().map(|d| d.join("galahad.log"));
    let logs = logger::init(log_path);
    log::info!("Galahad Encoder starting");

    // Config + identity.
    let config_path = ClientConfig::default_path().unwrap_or_else(|_| {
        std::path::PathBuf::from("galahad-config.json")
    });
    let mut config = ClientConfig::load_or_default(&config_path);
    // Persist a freshly generated player id on first run.
    if config.player_id.is_empty() {
        config.player_id = format!("player-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    }
    let identity = Identity::new(&config);
    log::info!(
        "identity player_id={} session_id={}",
        identity.player_id,
        identity.session_id
    );

    // Capability scan drives the whole UI.
    let caps = capability::detect();
    log::info!(
        "capabilities: capture={} encoders={:?} processAudio={} discord={} mics={}",
        caps.capture,
        caps.encoders,
        caps.process_audio,
        caps.discord_audio,
        caps.microphones.len()
    );

    let ffmpeg_path = paths::locate_ffmpeg();
    let control_url = config.control_channel_url.clone();

    let inner = Arc::new(AppStateInner {
        config: Mutex::new(config),
        config_path,
        caps: Mutex::new(caps),
        pipeline: Mutex::new(None),
        identity: identity.clone(),
        ffmpeg_path,
        logs,
        control: Mutex::new(None),
        selected_window: Mutex::new(None),
        preview: preview::PreviewManager::new(paths::preview_dir()),
        audio_levels: audio::new_levels(),
    });

    let state = AppState(inner.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .setup(move |_app| {
            // Spawn the Control Channel + telemetry once the runtime is up.
            start_control_channel(inner.clone(), control_url);
            start_telemetry_timer(inner.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_capabilities,
            commands::refresh_capabilities,
            commands::list_windows,
            commands::list_microphones,
            commands::list_video_devices,
            commands::detect_ip,
            commands::discord_processes,
            commands::list_audio_processes,
            commands::get_config,
            commands::save_config,
            commands::get_identity,
            commands::select_window,
            commands::get_status,
            commands::start_stream,
            commands::stop_stream,
            commands::restart_stream,
            commands::switch_quality,
            commands::get_logs,
            commands::get_preview_status,
            commands::get_audio_levels,
            commands::open_preview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Galahad Encoder");
}

fn start_control_channel(inner: Arc<AppStateInner>, url: Option<String>) {
    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        log::info!("no control channel url configured; running standalone");
        return;
    };

    let hello = ClientMessage::Hello {
        player_id: inner.identity.player_id.clone(),
        session_id: inner.identity.session_id.clone(),
        display_name: inner.identity.display_name.clone(),
    };

    let dispatch_inner = inner.clone();
    let sink = Arc::new(move |cmd| {
        commands::dispatch_command(&dispatch_inner, cmd);
    });

    let handle = control_runtime::spawn(url, hello, sink);
    *inner.control.lock().unwrap() = Some(handle);
}

fn start_telemetry_timer(inner: Arc<AppStateInner>) {
    std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let handle = inner.control.lock().unwrap().clone();
            if let Some(handle) = handle {
                handle.send(commands::telemetry_message(&inner));
            }
        })
        .ok();
}
