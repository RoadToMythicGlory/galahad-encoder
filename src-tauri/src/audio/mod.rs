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

/// What the engine should mix. Per-process gain/mute is folded into these two
/// effective sources by the caller (see `pipeline`).
#[derive(Debug, Clone)]
pub struct AudioPlan {
    /// Capture the system render endpoint (covers game + Discord today).
    pub system_loopback: bool,
    pub system_gain: f32,
    /// Capture the selected microphone.
    pub microphone: bool,
    pub microphone_device_id: Option<String>,
    pub microphone_gain: f32,
}

impl AudioPlan {
    pub fn any_enabled(&self) -> bool {
        self.system_loopback || self.microphone
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
pub fn start(plan: AudioPlan, sink: Sender<Vec<u8>>) -> Result<AudioEngine> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    let mut warnings = Vec::new();
    let mut sources: Vec<(SampleRing, f32)> = Vec::new();

    #[cfg(windows)]
    {
        if plan.system_loopback {
            let ring = new_ring();
            match wasapi::start_render_loopback(ring.clone(), stop.clone()) {
                Ok(handle) => {
                    handles.push(handle);
                    sources.push((ring, plan.system_gain));
                }
                Err(e) => warnings.push(format!("system audio capture failed: {e}")),
            }
        }
        if plan.microphone {
            let ring = new_ring();
            match wasapi::start_microphone(
                plan.microphone_device_id.clone(),
                ring.clone(),
                stop.clone(),
            ) {
                Ok(handle) => {
                    handles.push(handle);
                    sources.push((ring, plan.microphone_gain));
                }
                Err(e) => warnings.push(format!("microphone capture failed: {e}")),
            }
        }
        warnings.push(
            "per-process isolation unavailable; mixing system audio (game + Discord) + mic".into(),
        );
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
        .spawn(move || mixer_loop(sources, sink, mix_stop))
        .map_err(|e| crate::error::EncoderError::Audio(format!("mixer thread: {e}")))?;
    handles.push(handle);

    Ok(AudioEngine {
        stop,
        handles,
        warnings,
    })
}

fn mixer_loop(sources: Vec<(SampleRing, f32)>, sink: Sender<Vec<u8>>, stop: Arc<AtomicBool>) {
    let frames_per_tick = (SAMPLE_RATE as u64 * TICK_MS / 1000) as usize;
    let samples_per_tick = frames_per_tick * CHANNELS as usize;
    let tick = Duration::from_millis(TICK_MS);

    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let mut mix = vec![0f32; samples_per_tick];

        for (ring, gain) in &sources {
            if let Ok(mut q) = ring.lock() {
                for slot in mix.iter_mut() {
                    if let Some(sample) = q.pop_front() {
                        *slot += sample * *gain;
                    }
                }
            }
        }

        let mut bytes = Vec::with_capacity(samples_per_tick * 2);
        for s in &mix {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }

        if sink.send(bytes).is_err() {
            break; // pipeline closed the audio writer
        }

        if let Some(remaining) = tick.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
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
        let plan = AudioPlan {
            system_loopback: false,
            system_gain: 1.0,
            microphone: false,
            microphone_device_id: None,
            microphone_gain: 1.0,
        };
        assert!(!plan.any_enabled());
    }
}
