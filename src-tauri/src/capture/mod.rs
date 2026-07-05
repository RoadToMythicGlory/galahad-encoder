//! Capture backend abstraction.
//!
//! Windows Graphics Capture (WGC) is the primary backend, chosen over `gdigrab`
//! because it reliably captures GPU-composited games in borderless / fullscreen
//! windowed mode. The trait keeps room for a future lower-copy D3D11 path without
//! touching the pipeline.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::error::Result;

#[cfg(windows)]
pub mod wgc;

/// What to capture and at what cadence.
#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    /// Target window (HWND id from `window_enum`).
    pub window_id: isize,
    /// Capture polling cadence. The encoder paces output; this just bounds the
    /// rate at which we pull frames from the pool.
    pub fps: u32,
}

/// A tightly packed BGRA frame (no row padding) ready for FFmpeg's rawvideo input.
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, BGRA, top-down.
    pub data: Vec<u8>,
}

/// A running capture. Dropping it (or setting the shared stop flag) ends capture.
pub struct CaptureSession {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Dimensions of the frames this session emits (locked at start).
    pub width: u32,
    pub height: u32,
}

impl CaptureSession {
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start capturing the configured window, pushing frames to `sink`.
#[cfg(windows)]
pub fn start(config: CaptureConfig, sink: Sender<FrameBuffer>) -> Result<CaptureSession> {
    wgc::start(config, sink)
}

#[cfg(not(windows))]
pub fn start(_config: CaptureConfig, _sink: Sender<FrameBuffer>) -> Result<CaptureSession> {
    Err(crate::error::EncoderError::Capture(
        "window capture is only supported on Windows".into(),
    ))
}
