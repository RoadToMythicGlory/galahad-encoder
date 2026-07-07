//! Streaming pipeline supervisor.
//!
//! Wires capture + audio mixing + FFmpeg into one managed session and drives the
//! always-on reconnect state machine. Frames flow capture -> FFmpeg stdin; mixed
//! PCM flows audio-engine -> localhost TCP -> FFmpeg. The supervisor owns process
//! lifecycle so Start/Stop/Restart never leak FFmpeg or capture threads.

use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use serde::Serialize;

use crate::audio::{self, AudioEngine, AudioPlan};
use crate::capture::{self, CaptureConfig};
use crate::encoder::EncoderBackend;
use crate::error::{EncoderError, Result};
use crate::ffmpeg::FfmpegPlan;
use crate::gst2110::Gst2110Plan;
use crate::srt::SrtDestination;
use crate::stream_state::{StreamEvent, StreamMachine, StreamState};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The output transport for a session. SRT captures a window with WGC and
/// encodes with FFmpeg; ST 2110 captures a device directly with GStreamer and
/// sends uncompressed RTP.
#[derive(Debug, Clone)]
pub enum OutputPlan {
    /// Compressed uplink from a game window: WGC window capture -> FFmpeg -> SRT.
    SrtWindow {
        window_id: isize,
        backend: EncoderBackend,
        destination: SrtDestination,
        /// Listener fan-out: max simultaneous callers (1-3).
        max_callers: u8,
        audio_plan: AudioPlan,
    },
    /// Compressed uplink from a camera / capture card: FFmpeg opens the
    /// DirectShow device directly (no WGC), encodes, and sends SRT. Audio (if
    /// any channels are configured) is mixed natively and muxed alongside.
    SrtDevice {
        device_name: String,
        backend: EncoderBackend,
        destination: SrtDestination,
        /// Listener fan-out: max simultaneous callers (1-3).
        max_callers: u8,
        audio_plan: AudioPlan,
    },
    /// Broadcast IP: GStreamer device capture -> ST 2110 RTP.
    St2110 {
        gst: Gst2110Plan,
        gstreamer_path: PathBuf,
        /// Session description receivers use to subscribe; written to the log
        /// dir and echoed to the log on start.
        sdp: String,
    },
}

/// Local HLS preview target for an SRT session. FFmpeg tees its already-encoded
/// packets into `playlist` (a relative filename) while running with its working
/// directory set to `dir`, so no path escaping is needed in the tee spec.
#[derive(Debug, Clone)]
pub struct PreviewSession {
    pub dir: PathBuf,
    pub playlist: String,
}

/// Fully resolved configuration for one streaming session.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub source_label: String,
    pub fps: u32,
    pub out_width: u32,
    pub out_height: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    pub output: OutputPlan,
    /// When set, an SRT session also writes a local HLS preview.
    pub preview: Option<PreviewSession>,
}

/// Snapshot of pipeline status for the UI and Control Channel telemetry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub state: StreamState,
    pub source: Option<String>,
    pub fps: u32,
    pub bitrate: u32,
    pub dropped_frames: u64,
    pub encoder: Option<String>,
    pub capture_backend: Option<String>,
    pub last_error: Option<String>,
    pub audio_warnings: Vec<String>,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self {
            state: StreamState::Idle,
            source: None,
            fps: 0,
            bitrate: 0,
            dropped_frames: 0,
            encoder: None,
            capture_backend: None,
            last_error: None,
            audio_warnings: Vec::new(),
        }
    }
}

struct Inner {
    stop: AtomicBool,
    restart: AtomicBool,
    desired: Mutex<PipelineConfig>,
    status: Mutex<StreamStatus>,
    ffmpeg_path: PathBuf,
    /// Live program + per-channel audio levels, shared with the UI.
    audio_levels: audio::LevelsHandle,
}

/// Owns the supervisor thread for an active stream.
pub struct Pipeline {
    inner: Arc<Inner>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Start streaming with the given config. The supervisor runs until `stop`.
    pub fn start(
        config: PipelineConfig,
        ffmpeg_path: PathBuf,
        audio_levels: audio::LevelsHandle,
    ) -> Self {
        let inner = Arc::new(Inner {
            stop: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            desired: Mutex::new(config),
            status: Mutex::new(StreamStatus::default()),
            ffmpeg_path,
            audio_levels,
        });
        let thread_inner = inner.clone();
        let thread = std::thread::Builder::new()
            .name("pipeline-supervisor".into())
            .spawn(move || supervise(thread_inner))
            .ok();
        Pipeline { inner, thread }
    }

    pub fn status(&self) -> StreamStatus {
        self.inner.status.lock().unwrap().clone()
    }

    pub fn restart(&self) {
        self.inner.restart.store(true, Ordering::SeqCst);
    }

    /// Update the desired config (e.g. quality switch) and trigger a restart.
    pub fn update_config(&self, config: PipelineConfig) {
        *self.inner.desired.lock().unwrap() = config;
        self.inner.restart.store(true, Ordering::SeqCst);
    }

    pub fn stop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        let mut status = self.inner.status.lock().unwrap();
        status.state = StreamState::Idle;
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bind a localhost TCP socket FFmpeg will connect to for the mixed PCM audio.
/// Returns `(None, "")` when audio is disabled.
fn bind_audio_socket(enabled: bool) -> Result<(Option<TcpListener>, String)> {
    if !enabled {
        return Ok((None, String::new()));
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| EncoderError::Pipeline(format!("audio socket bind failed: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| EncoderError::Pipeline(format!("audio socket addr failed: {e}")))?
        .port();
    Ok((Some(listener), format!("tcp://127.0.0.1:{port}")))
}

/// Start the audio engine and, if a socket was bound, the thread that feeds
/// mixed PCM to FFmpeg once it connects. Shared by the window and device paths.
fn start_audio_engine(
    inner: &Arc<Inner>,
    audio_plan: &AudioPlan,
    audio_listener: Option<TcpListener>,
    session_alive: &Arc<AtomicBool>,
) -> Result<AudioEngine> {
    let (pcm_tx, pcm_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let engine = audio::start(audio_plan.clone(), pcm_tx, inner.audio_levels.clone())?;
    inner.status.lock().unwrap().audio_warnings = engine.warnings.clone();

    if let Some(listener) = audio_listener {
        listener
            .set_nonblocking(true)
            .map_err(|e| EncoderError::Pipeline(format!("audio socket nonblock: {e}")))?;
        let alive = session_alive.clone();
        let stop_flag = inner.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if stop_flag.stop.load(Ordering::SeqCst) || Instant::now() > deadline {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nodelay(true);
                        while alive.load(Ordering::SeqCst) {
                            match pcm_rx.recv_timeout(Duration::from_millis(500)) {
                                Ok(bytes) => {
                                    if stream.write_all(&bytes).is_err() {
                                        alive.store(false, Ordering::SeqCst);
                                        return;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                                Err(_) => return,
                            }
                        }
                        return;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => return,
                }
            }
        });
    }
    Ok(engine)
}

/// Relay a child process's stderr into the log. The dshow "real-time buffer …
/// frame dropped" warning fires continuously whenever the camera is producing
/// faster than FFmpeg drains it — which is the normal state while an SRT
/// listener waits for a caller. Left unfiltered it floods the log (hundreds of
/// lines/second) and buries everything useful, so we collapse those specific
/// warnings into a single summary line every few seconds and pass everything
/// else through verbatim.
fn spawn_stderr_relay(stderr: ChildStderr, tool: &'static str) {
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stderr);
        let mut dropped: u64 = 0;
        let mut last_summary: Option<Instant> = None;
        for line in reader.lines().map_while(|l| l.ok()) {
            let is_drop = line.contains("real-time buffer") && line.contains("frame dropped");
            let is_drop_repeat = dropped > 0 && line.contains("Last message repeated");
            if is_drop || is_drop_repeat {
                dropped += 1;
                let due = last_summary
                    .map(|t| t.elapsed() >= Duration::from_secs(5))
                    .unwrap_or(true);
                if due {
                    log::warn!(
                        "{tool}: camera frames dropping (no consumer draining yet — \
                         normal while the SRT listener waits for a caller)"
                    );
                    last_summary = Some(Instant::now());
                }
                continue;
            }
            log::warn!("{tool}: {line}");
        }
    });
}

fn set_state(inner: &Inner, state: StreamState) {
    inner.status.lock().unwrap().state = state;
}

fn set_error(inner: &Inner, error: Option<String>) {
    inner.status.lock().unwrap().last_error = error;
}

fn supervise(inner: Arc<Inner>) {
    let mut machine = StreamMachine::default();
    machine.apply(StreamEvent::Start);
    set_state(&inner, StreamState::Starting);

    while !inner.stop.load(Ordering::SeqCst) {
        inner.restart.store(false, Ordering::SeqCst);
        let config = inner.desired.lock().unwrap().clone();

        // Reset meters for the new session; the audio engine repopulates them if
        // this session carries audio (ST 2110 / video-only leaves them empty).
        *inner.audio_levels.lock().unwrap() = audio::AudioLevels::default();

        match run_session(&inner, &config, &mut machine) {
            Ok(SessionEnd::UserStop) => break,
            Ok(SessionEnd::Restart) => {
                // Quality switch / explicit restart: loop straight back in.
                set_state(&inner, StreamState::Starting);
                machine.apply(StreamEvent::Start);
                continue;
            }
            Ok(SessionEnd::Ended) | Err(_) => {
                if inner.stop.load(Ordering::SeqCst) {
                    break;
                }
                machine.apply(StreamEvent::RecoverableError);
                set_state(&inner, StreamState::Reconnecting);
                machine.apply(StreamEvent::RetryTick);
                let delay = machine.current_backoff();
                // Sleep in small slices so Stop is responsive.
                let mut waited = Duration::ZERO;
                while waited < delay && !inner.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                    waited += Duration::from_millis(100);
                }
            }
        }
    }

    machine.apply(StreamEvent::Stop);
    set_state(&inner, StreamState::Idle);
}

enum SessionEnd {
    /// User pressed Stop.
    UserStop,
    /// Explicit restart / quality switch requested.
    Restart,
    /// Session ended on its own (process exit, write failure).
    Ended,
}

fn run_session(
    inner: &Arc<Inner>,
    config: &PipelineConfig,
    machine: &mut StreamMachine,
) -> Result<SessionEnd> {
    match &config.output {
        OutputPlan::SrtWindow {
            window_id,
            backend,
            destination,
            max_callers,
            audio_plan,
        } => run_srt_session(
            inner,
            config,
            machine,
            *window_id,
            *backend,
            destination,
            *max_callers,
            audio_plan,
        ),
        OutputPlan::SrtDevice {
            device_name,
            backend,
            destination,
            max_callers,
            audio_plan,
        } => run_srt_device_session(
            inner,
            config,
            machine,
            device_name,
            *backend,
            destination,
            *max_callers,
            audio_plan,
        ),
        OutputPlan::St2110 {
            gst,
            gstreamer_path,
            sdp,
        } => run_st2110_session(inner, config, machine, gst, gstreamer_path, sdp),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_srt_session(
    inner: &Arc<Inner>,
    config: &PipelineConfig,
    machine: &mut StreamMachine,
    window_id: isize,
    backend: EncoderBackend,
    destination: &SrtDestination,
    max_callers: u8,
    audio_plan: &AudioPlan,
) -> Result<SessionEnd> {
    // --- Capture ---
    let (frame_tx, frame_rx) = bounded::<capture::FrameBuffer>(4);
    let mut capture_session = capture::start(
        CaptureConfig {
            window_id,
            fps: config.fps,
        },
        frame_tx,
    )?;
    let cap_w = capture_session.width;
    let cap_h = capture_session.height;

    {
        let mut status = inner.status.lock().unwrap();
        status.source = Some(config.source_label.clone());
        status.capture_backend = Some("wgc".into());
        status.encoder = Some(backend.ffmpeg_name.to_string());
        status.fps = config.fps;
        status.bitrate = config.video_kbps + if audio_plan.any_enabled() { config.audio_kbps } else { 0 };
        status.dropped_frames = 0;
        status.last_error = None;
    }

    // --- Audio TCP transport ---
    let audio_enabled = audio_plan.any_enabled();
    let (audio_listener, audio_url) = bind_audio_socket(audio_enabled)?;

    // --- FFmpeg ---
    let plan = FfmpegPlan {
        video_input: crate::ffmpeg::VideoInput::RawPipe,
        capture_width: cap_w,
        capture_height: cap_h,
        out_width: config.out_width,
        out_height: config.out_height,
        fps: config.fps,
        video_kbps: config.video_kbps,
        backend,
        destination: destination.clone(),
        max_callers,
        audio_enabled,
        audio_input: audio_url,
        audio_sample_rate: audio::SAMPLE_RATE,
        audio_channels: audio::CHANNELS,
        audio_kbps: config.audio_kbps,
        preview: config
            .preview
            .as_ref()
            .map(|p| crate::ffmpeg::PreviewSink::new(p.playlist.clone())),
    };
    let args = plan.build_args();
    log::info!("ffmpeg {}", args.join(" "));

    let mut command = Command::new(&inner.ffmpeg_path);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // HLS preview segments are written relative to the working directory.
    if let Some(preview) = &config.preview {
        command.current_dir(&preview.dir);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child: Child = command
        .spawn()
        .map_err(|e| EncoderError::Pipeline(format!("failed to launch ffmpeg: {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EncoderError::Pipeline("ffmpeg stdin unavailable".into()))?;

    // Drain ffmpeg stderr to the log so capture/encode warnings are visible.
    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_relay(stderr, "ffmpeg");
    }

    let session_alive = Arc::new(AtomicBool::new(true));
    let frames_written = Arc::new(AtomicU64::new(0));

    // --- Audio engine + writer ---
    let mut audio_engine = None;
    if audio_enabled {
        audio_engine = Some(start_audio_engine(
            inner,
            audio_plan,
            audio_listener,
            &session_alive,
        )?);
    }

    // --- Video writer: capture frames -> ffmpeg stdin ---
    let writer_alive = session_alive.clone();
    let writer_frames = frames_written.clone();
    let writer = std::thread::Builder::new()
        .name("video-writer".into())
        .spawn(move || {
            while writer_alive.load(Ordering::SeqCst) {
                match frame_rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(frame) => {
                        if stdin.write_all(&frame.data).is_err() {
                            writer_alive.store(false, Ordering::SeqCst);
                            break;
                        }
                        writer_frames.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
        })
        .ok();

    // Surface "streaming" in the UI immediately, but do NOT yet credit this
    // session as a healthy bring-up. `StreamEvent::Up` resets the reconnect
    // backoff counter, so emitting it the instant FFmpeg *spawns* (rather than
    // once it has actually stayed up) defeats the exponential backoff entirely:
    // an SRT peer that connects then drops after ~1-2s -- e.g. the matrix
    // listener briefly re-listening -- would reset the counter every cycle and
    // hammer reconnects forever at ~2s intervals instead of backing off. We only
    // count the session as healthy after it survives `STABLE_UPTIME`.
    set_state(inner, StreamState::Streaming);
    const STABLE_UPTIME: Duration = Duration::from_secs(5);
    let mut credited_up = false;

    // Connection tracking. FFmpeg only starts draining frames from stdin once
    // the SRT peer is established (a listener blocks in write_header until a
    // caller connects), so a rising `frames_written` is a reliable "peer is
    // live and pulling" signal. We log transitions so the operator can see in
    // the log whether anyone actually connected.
    let is_listener = matches!(destination.mode, crate::srt::SrtMode::Listener);
    let peer_port = destination.port;
    let peer_host = destination.host.clone();
    if is_listener {
        log::info!(
            "SRT listener on port {peer_port}: waiting for a caller to connect \u{2026}"
        );
    } else {
        log::info!("SRT caller: connecting to {peer_host}:{peer_port} \u{2026}");
    }
    let mut peer_connected = false;
    let mut last_written = frames_written.load(Ordering::Relaxed);
    let mut last_conn_check = Instant::now();
    const CONN_CHECK: Duration = Duration::from_secs(2);

    // --- Monitor loop ---
    let started = Instant::now();
    let end = loop {
        if inner.stop.load(Ordering::SeqCst) {
            break SessionEnd::UserStop;
        }
        if inner.restart.load(Ordering::SeqCst) {
            break SessionEnd::Restart;
        }
        if !credited_up && started.elapsed() >= STABLE_UPTIME {
            // Stayed up long enough to be a real connection: reset backoff.
            machine.apply(StreamEvent::Up);
            credited_up = true;
        }
        if !session_alive.load(Ordering::SeqCst) {
            set_error(inner, Some("stream writer disconnected".into()));
            break SessionEnd::Ended;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                set_error(inner, Some(format!("ffmpeg exited: {exit}")));
                break SessionEnd::Ended;
            }
            Ok(None) => {}
            Err(e) => {
                set_error(inner, Some(format!("ffmpeg wait error: {e}")));
                break SessionEnd::Ended;
            }
        }

        // Telemetry: approximate dropped frames from expected vs written.
        let elapsed = started.elapsed().as_secs_f64();
        let expected = (elapsed * config.fps as f64) as u64;
        let written = frames_written.load(Ordering::Relaxed);
        let dropped = expected.saturating_sub(written);
        inner.status.lock().unwrap().dropped_frames = dropped;

        // Detect peer connect / disconnect from frame flow and log transitions.
        if last_conn_check.elapsed() >= CONN_CHECK {
            let delta = written.saturating_sub(last_written);
            // At least ~half a second of frames must have flowed to count as live.
            let flowing = delta as f64 >= (config.fps as f64 * 0.5).max(1.0);
            if flowing != peer_connected {
                peer_connected = flowing;
                if flowing {
                    if is_listener {
                        log::info!(
                            "SRT caller connected on port {peer_port}: streaming \
                             ({written} frames sent)"
                        );
                    } else {
                        log::info!(
                            "SRT connected to {peer_host}:{peer_port}: streaming \
                             ({written} frames sent)"
                        );
                    }
                    set_error(inner, None);
                } else if is_listener {
                    log::warn!(
                        "SRT listener on port {peer_port}: no caller pulling \
                         (waiting / disconnected)"
                    );
                } else {
                    log::warn!(
                        "SRT caller to {peer_host}:{peer_port}: stream stalled \
                         (peer not accepting)"
                    );
                }
            }
            last_written = written;
            last_conn_check = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(200));
    };

    // --- Teardown (ordered, no orphans) ---
    session_alive.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    capture_session.stop();
    if let Some(mut engine) = audio_engine.take() {
        engine.stop();
    }
    if let Some(writer) = writer {
        let _ = writer.join();
    }

    Ok(end)
}

/// Run an SRT session that captures a camera / capture card directly.
///
/// Unlike the window path there is no WGC capture and no stdin pipe: FFmpeg
/// opens the DirectShow device itself, so the supervisor only spawns and
/// monitors the process. Because FFmpeg reads its own frames we can't count
/// them the way the pipe path does, so connection state is inferred from the
/// process staying alive (an SRT listener blocks in `write_header` until a
/// caller connects, but the device keeps running regardless).
#[allow(clippy::too_many_arguments)]
fn run_srt_device_session(
    inner: &Arc<Inner>,
    config: &PipelineConfig,
    machine: &mut StreamMachine,
    device_name: &str,
    backend: EncoderBackend,
    destination: &SrtDestination,
    max_callers: u8,
    audio_plan: &AudioPlan,
) -> Result<SessionEnd> {
    {
        let mut status = inner.status.lock().unwrap();
        status.source = Some(config.source_label.clone());
        status.capture_backend = Some("dshow".into());
        status.encoder = Some(backend.ffmpeg_name.to_string());
        status.fps = config.fps;
        status.bitrate =
            config.video_kbps + if audio_plan.any_enabled() { config.audio_kbps } else { 0 };
        status.dropped_frames = 0;
        status.last_error = None;
        status.audio_warnings = Vec::new();
    }

    // --- Audio TCP transport (mixed PCM), if any channels are configured ---
    let audio_enabled = audio_plan.any_enabled();
    let (audio_listener, audio_url) = bind_audio_socket(audio_enabled)?;

    // Capture at the profile geometry so no scaling is needed (highest quality
    // straight off the card).
    let plan = FfmpegPlan {
        video_input: crate::ffmpeg::VideoInput::Dshow {
            device: device_name.to_string(),
        },
        capture_width: config.out_width,
        capture_height: config.out_height,
        out_width: config.out_width,
        out_height: config.out_height,
        fps: config.fps,
        video_kbps: config.video_kbps,
        backend,
        destination: destination.clone(),
        max_callers,
        audio_enabled,
        audio_input: audio_url,
        audio_sample_rate: audio::SAMPLE_RATE,
        audio_channels: audio::CHANNELS,
        audio_kbps: config.audio_kbps,
        preview: config
            .preview
            .as_ref()
            .map(|p| crate::ffmpeg::PreviewSink::new(p.playlist.clone())),
    };
    let args = plan.build_args();
    log::info!("ffmpeg {}", args.join(" "));

    let mut command = Command::new(&inner.ffmpeg_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // HLS preview segments are written relative to the working directory.
    if let Some(preview) = &config.preview {
        command.current_dir(&preview.dir);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child: Child = command
        .spawn()
        .map_err(|e| EncoderError::Pipeline(format!("failed to launch ffmpeg: {e}")))?;

    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_relay(stderr, "ffmpeg");
    }

    // --- Audio engine + writer ---
    let session_alive = Arc::new(AtomicBool::new(true));
    let mut audio_engine = None;
    if audio_enabled {
        audio_engine = Some(start_audio_engine(
            inner,
            audio_plan,
            audio_listener,
            &session_alive,
        )?);
    }

    let is_listener = matches!(destination.mode, crate::srt::SrtMode::Listener);
    if is_listener {
        log::info!(
            "SRT listener on port {}: capturing '{device_name}', waiting for a caller \u{2026}",
            destination.port
        );
    } else {
        log::info!(
            "SRT caller: capturing '{device_name}', connecting to {}:{} \u{2026}",
            destination.host,
            destination.port
        );
    }

    set_state(inner, StreamState::Streaming);
    const STABLE_UPTIME: Duration = Duration::from_secs(5);
    let mut credited_up = false;

    let started = Instant::now();
    let end = loop {
        if inner.stop.load(Ordering::SeqCst) {
            break SessionEnd::UserStop;
        }
        if inner.restart.load(Ordering::SeqCst) {
            break SessionEnd::Restart;
        }
        if !credited_up && started.elapsed() >= STABLE_UPTIME {
            machine.apply(StreamEvent::Up);
            credited_up = true;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                set_error(inner, Some(format!("ffmpeg exited: {exit}")));
                break SessionEnd::Ended;
            }
            Ok(None) => {}
            Err(e) => {
                set_error(inner, Some(format!("ffmpeg wait error: {e}")));
                break SessionEnd::Ended;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    session_alive.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(mut engine) = audio_engine.take() {
        engine.stop();
    }

    Ok(end)
}

/// Run an ST 2110 session: GStreamer captures the device directly and sends
/// uncompressed RTP. Unlike the SRT path there is no WGC capture or FFmpeg;
/// GStreamer owns both source and sink, so the supervisor only spawns and
/// monitors the process (and publishes the SDP for receivers).
fn run_st2110_session(
    inner: &Arc<Inner>,
    config: &PipelineConfig,
    machine: &mut StreamMachine,
    gst: &Gst2110Plan,
    gstreamer_path: &PathBuf,
    sdp: &str,
) -> Result<SessionEnd> {
    publish_sdp(config, sdp);

    {
        let mut status = inner.status.lock().unwrap();
        status.source = Some(config.source_label.clone());
        status.capture_backend = Some("st2110".into());
        status.encoder = Some("st2110-20 raw".into());
        status.fps = config.fps;
        status.bitrate = config.video_kbps;
        status.dropped_frames = 0;
        status.last_error = None;
        status.audio_warnings = Vec::new();
    }

    let args = gst.build_pipeline_args();
    log::info!("gst-launch-1.0 {}", args.join(" "));

    let mut command = Command::new(gstreamer_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child: Child = command
        .spawn()
        .map_err(|e| EncoderError::Pipeline(format!("failed to launch gst-launch-1.0: {e}")))?;

    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_relay(stderr, "gstreamer");
    }

    set_state(inner, StreamState::Streaming);
    const STABLE_UPTIME: Duration = Duration::from_secs(5);
    let mut credited_up = false;

    let started = Instant::now();
    let end = loop {
        if inner.stop.load(Ordering::SeqCst) {
            break SessionEnd::UserStop;
        }
        if inner.restart.load(Ordering::SeqCst) {
            break SessionEnd::Restart;
        }
        if !credited_up && started.elapsed() >= STABLE_UPTIME {
            machine.apply(StreamEvent::Up);
            credited_up = true;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                set_error(inner, Some(format!("gstreamer exited: {exit}")));
                break SessionEnd::Ended;
            }
            Ok(None) => {}
            Err(e) => {
                set_error(inner, Some(format!("gstreamer wait error: {e}")));
                break SessionEnd::Ended;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let _ = child.kill();
    let _ = child.wait();

    Ok(end)
}

/// Write the SDP next to the logs and echo it so operators can hand it to
/// receivers (a 2110 receiver subscribes using this session description).
fn publish_sdp(config: &PipelineConfig, sdp: &str) {
    if let Some(dir) = crate::paths::log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("st2110.sdp");
        if let Err(e) = std::fs::write(&path, sdp) {
            log::warn!("failed to write SDP to {}: {e}", path.display());
        } else {
            log::info!("ST 2110 SDP written to {}", path.display());
        }
    }
    log::info!("ST 2110 session '{}' SDP:\n{}", config.source_label, sdp);
}
