//! Audio capture + mixing.
//!
//! The plan's target mix is selected-app audio + Discord + microphone. True
//! per-process isolation needs Windows process-loopback activation, which is
//! gated behind capability detection. When it is unavailable we fall back (per
//! the plan) to a system render-endpoint loopback that contains the game and
//! Discord, plus the microphone, and surface a clear warning.
//!
//! Every source is normalised to stereo f32 @ 48 kHz internally; the mixer pulls
//! a fixed slice per tick, applies gain, sums, clamps, converts to s16le, and
//! pushes the result to the pipeline (which relays it to FFmpeg over TCP).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use serde::Serialize;

use crate::error::Result;

#[cfg(windows)]
mod wasapi;

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u32 = 2;
const TICK_MS: u64 = 10;
/// Max buffered audio per source (~1s) before oldest samples are dropped.
const RING_CAP: usize = (SAMPLE_RATE * CHANNELS) as usize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneInfo {
    pub id: String,
    pub name: String,
}

/// Live level of one mixer channel (post-gain), 0.0..1.0 linear.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelLevel {
    pub label: String,
    /// Peak-hold level with decay (snappy, catches transients).
    pub peak: f32,
    /// Smoothed RMS (loudness-ish, steadier).
    pub rms: f32,
}

/// Live audio metering shared with the UI: the program (master L/R) plus each
/// mixer channel. Updated by the mixer thread; read by `get_audio_levels`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioLevels {
    /// True while the mixer is running (a stream with audio is live).
    pub active: bool,
    pub program_peak_l: f32,
    pub program_peak_r: f32,
    pub program_rms_l: f32,
    pub program_rms_r: f32,
    pub channels: Vec<ChannelLevel>,
}

/// Shared handle for live audio levels.
pub type LevelsHandle = Arc<Mutex<AudioLevels>>;

pub fn new_levels() -> LevelsHandle {
    Arc::new(Mutex::new(AudioLevels::default()))
}

/// Interleaved stereo f32 @ 48 kHz ring buffer shared with a capturer thread.
pub type SampleRing = Arc<Mutex<VecDeque<f32>>>;

pub fn new_ring() -> SampleRing {
    Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP)))
}

/// Push samples into a ring, dropping oldest data past the cap.
pub fn push_samples(ring: &SampleRing, samples: &[f32]) {
    if let Ok(mut q) = ring.lock() {
        for &s in samples {
            if q.len() >= RING_CAP {
                q.pop_front();
            }
            q.push_back(s);
        }
    }
}

/// A single capture target for the engine.
#[derive(Debug, Clone)]
pub enum AudioCapture {
    /// Default render endpoint loopback (whole desktop mix).
    SystemLoopback,
    /// A capture endpoint by id (None = default mic).
    Microphone { device_id: Option<String> },
    /// A process's audio via Windows process loopback (app tree at this pid).
    Application { process_id: u32 },
}

/// One resolved mixer channel: what to capture and at what gain.
#[derive(Debug, Clone)]
pub struct AudioPlanSource {
    pub capture: AudioCapture,
    pub gain: f32,
    /// Label for warnings / logs.
    pub label: String,
}

/// What the engine should capture and mix. Built from the config's live
/// channels by the caller (see `commands::build_audio_plan`).
#[derive(Debug, Clone, Default)]
pub struct AudioPlan {
    pub sources: Vec<AudioPlanSource>,
}

impl AudioPlan {
    pub fn any_enabled(&self) -> bool {
        !self.sources.is_empty()
    }
}

/// A running audio engine. Drop or `stop()` to end capture + mixing.
pub struct AudioEngine {
    stop: Arc<AtomicBool>,
    handles: Vec<std::thread::JoinHandle<()>>,
    pub warnings: Vec<String>,
}

impl AudioEngine {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// List available microphone (capture) endpoints.
pub fn list_microphones() -> Vec<MicrophoneInfo> {
    #[cfg(windows)]
    {
        wasapi::list_capture_devices().unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Start the audio engine, mixing enabled sources into `sink` as s16le bytes.
/// Live program + per-channel levels are published into `levels`.
pub fn start(plan: AudioPlan, sink: Sender<Vec<u8>>, levels: LevelsHandle) -> Result<AudioEngine> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    let mut warnings = Vec::new();
    let mut sources: Vec<(SampleRing, f32, String)> = Vec::new();

    #[cfg(windows)]
    {
        for src in &plan.sources {
            let ring = new_ring();
            let started = match &src.capture {
                AudioCapture::SystemLoopback => {
                    wasapi::start_render_loopback(ring.clone(), stop.clone())
                }
                AudioCapture::Microphone { device_id } => {
                    wasapi::start_microphone(device_id.clone(), ring.clone(), stop.clone())
                }
                AudioCapture::Application { process_id } => {
                    wasapi::start_process_loopback(*process_id, ring.clone(), stop.clone())
                }
            };
            match started {
                Ok(handle) => {
                    log::info!(
                        "audio: started capture '{}' (gain {:.2})",
                        src.label,
                        src.gain
                    );
                    handles.push(handle);
                    sources.push((ring, src.gain, src.label.clone()));
                }
                Err(e) => {
                    log::warn!("audio: '{}' capture failed: {e}", src.label);
                    warnings.push(format!("'{}' capture failed: {e}", src.label));
                }
            }
        }
        if sources.is_empty() && !plan.sources.is_empty() {
            log::warn!("audio: no channel could be started; streaming video only");
            warnings.push("no audio channel could be started; streaming video only".into());
        }
    }

    #[cfg(not(windows))]
    {
        let _ = &plan;
        warnings.push("audio capture is only supported on Windows".into());
    }

    // Mixer thread.
    let mix_stop = stop.clone();
    let handle = std::thread::Builder::new()
        .name("audio-mixer".into())
        .spawn(move || mixer_loop(sources, sink, mix_stop, levels))
        .map_err(|e| crate::error::EncoderError::Audio(format!("mixer thread: {e}")))?;
    handles.push(handle);

    Ok(AudioEngine {
        stop,
        handles,
        warnings,
    })
}

fn mixer_loop(
    sources: Vec<(SampleRing, f32, String)>,
    sink: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    levels: LevelsHandle,
) {
    let frames_per_tick = (SAMPLE_RATE as u64 * TICK_MS / 1000) as usize;
    let samples_per_tick = frames_per_tick * CHANNELS as usize;
    let tick = Duration::from_millis(TICK_MS);

    // Peak-hold decay + RMS smoothing per ~10ms tick, tuned to look like a
    // hardware meter (snappy attack, gentle release).
    const PEAK_DECAY: f32 = 0.86;
    const RMS_SMOOTH: f32 = 0.6;

    // Seed the shared meter with the channel labels so the UI can lay out rows
    // immediately.
    {
        let mut lv = levels.lock().unwrap();
        *lv = AudioLevels {
            active: true,
            channels: sources
                .iter()
                .map(|(_, _, label)| ChannelLevel {
                    label: label.clone(),
                    peak: 0.0,
                    rms: 0.0,
                })
                .collect(),
            ..Default::default()
        };
    }

    // Per-second log accumulators (kept for diagnostics in the log view).
    let mut log_source_peak = vec![0f32; sources.len()];
    let mut log_mix_peak = 0f32;
    let mut meter_since = Instant::now();

    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let mut mix = vec![0f32; samples_per_tick];

        let mut src_peak = vec![0f32; sources.len()];
        let mut src_sq = vec![0f64; sources.len()];
        let mut src_cnt = vec![0u32; sources.len()];

        for (i, (ring, gain, _)) in sources.iter().enumerate() {
            if let Ok(mut q) = ring.lock() {
                for slot in mix.iter_mut() {
                    if let Some(sample) = q.pop_front() {
                        let s = sample * *gain;
                        *slot += s;
                        let a = s.abs();
                        if a > src_peak[i] {
                            src_peak[i] = a;
                        }
                        src_sq[i] += (s as f64) * (s as f64);
                        src_cnt[i] += 1;
                    }
                }
            }
        }

        // Program (master) L/R metering while converting to s16le.
        let mut prog_peak = [0f32; 2];
        let mut prog_sq = [0f64; 2];
        let mut prog_cnt = [0u32; 2];
        let mut bytes = Vec::with_capacity(samples_per_tick * 2);
        for (idx, s) in mix.iter().enumerate() {
            let clamped = s.clamp(-1.0, 1.0);
            let a = clamped.abs();
            let ch = idx & 1; // 0 = L, 1 = R (interleaved stereo)
            if a > prog_peak[ch] {
                prog_peak[ch] = a;
            }
            prog_sq[ch] += (clamped as f64) * (clamped as f64);
            prog_cnt[ch] += 1;
            if a > log_mix_peak {
                log_mix_peak = a;
            }
            let v = (clamped * 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }

        if sink.send(bytes).is_err() {
            break; // pipeline closed the audio writer
        }

        // Publish live levels (peak-hold decay + smoothed RMS).
        {
            let rms = |sq: f64, n: u32| if n > 0 { (sq / n as f64).sqrt() as f32 } else { 0.0 };
            let mut lv = levels.lock().unwrap();
            if lv.channels.len() != sources.len() {
                lv.channels = sources
                    .iter()
                    .map(|(_, _, label)| ChannelLevel {
                        label: label.clone(),
                        peak: 0.0,
                        rms: 0.0,
                    })
                    .collect();
            }
            for (i, ch) in lv.channels.iter_mut().enumerate() {
                ch.peak = src_peak[i].max(ch.peak * PEAK_DECAY);
                ch.rms = RMS_SMOOTH * ch.rms + (1.0 - RMS_SMOOTH) * rms(src_sq[i], src_cnt[i]);
                if src_peak[i] > log_source_peak[i] {
                    log_source_peak[i] = src_peak[i];
                }
            }
            lv.program_peak_l = prog_peak[0].max(lv.program_peak_l * PEAK_DECAY);
            lv.program_peak_r = prog_peak[1].max(lv.program_peak_r * PEAK_DECAY);
            lv.program_rms_l = RMS_SMOOTH * lv.program_rms_l + (1.0 - RMS_SMOOTH) * rms(prog_sq[0], prog_cnt[0]);
            lv.program_rms_r = RMS_SMOOTH * lv.program_rms_r + (1.0 - RMS_SMOOTH) * rms(prog_sq[1], prog_cnt[1]);
        }

        if meter_since.elapsed() >= Duration::from_secs(1) {
            let per: Vec<String> = sources
                .iter()
                .enumerate()
                .map(|(i, (_, _, label))| format!("{label}={:.0}%", log_source_peak[i] * 100.0))
                .collect();
            log::info!(
                "audio level: mix={:.0}% [{}]",
                log_mix_peak * 100.0,
                per.join(", ")
            );
            for p in log_source_peak.iter_mut() {
                *p = 0.0;
            }
            log_mix_peak = 0.0;
            meter_since = Instant::now();
        }

        if let Some(remaining) = tick.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    // Zero the meter on exit so the UI doesn't freeze on the last value.
    *levels.lock().unwrap() = AudioLevels::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_drops_oldest_past_cap() {
        let ring = new_ring();
        let big = vec![1.0f32; RING_CAP + 100];
        push_samples(&ring, &big);
        let q = ring.lock().unwrap();
        assert_eq!(q.len(), RING_CAP);
    }

    #[test]
    fn plan_any_enabled() {
        let empty = AudioPlan::default();
        assert!(!empty.any_enabled());

        let one = AudioPlan {
            sources: vec![AudioPlanSource {
                capture: AudioCapture::SystemLoopback,
                gain: 1.0,
                label: "Desktop audio".into(),
            }],
        };
        assert!(one.any_enabled());
    }
}
