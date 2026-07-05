//! WASAPI capture (microphone + system render loopback).
//!
//! Each capturer runs on its own thread, normalises the endpoint's native format
//! to stereo f32, resamples to 48 kHz, and pushes into a shared ring the mixer
//! drains. Polling (rather than event callbacks) keeps the COM surface small.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;

use super::{push_samples, MicrophoneInfo, SampleRing, SAMPLE_RATE};
use crate::error::{EncoderError, Result};

/// 200 ms shared buffer (REFERENCE_TIME, 100 ns units).
const BUFFER_DURATION: i64 = 2_000_000;

struct ComGuard;
impl ComGuard {
    fn init() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        ComGuard
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| EncoderError::Audio(format!("device enumerator failed: {e}")))
    }
}

pub fn list_capture_devices() -> Result<Vec<MicrophoneInfo>> {
    let _com = ComGuard::init();
    let enumerator = enumerator()?;
    let mut devices = Vec::new();
    unsafe {
        let collection = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(|e| EncoderError::Audio(format!("enum endpoints failed: {e}")))?;
        let count = collection
            .GetCount()
            .map_err(|e| EncoderError::Audio(format!("endpoint count failed: {e}")))?;
        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                continue;
            };
            let id = match device.GetId() {
                Ok(pwstr) => {
                    let s = pwstr.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(pwstr.0 as *const c_void));
                    s
                }
                Err(_) => continue,
            };
            let name = friendly_name(&device).unwrap_or_else(|| "Microphone".to_string());
            devices.push(MicrophoneInfo { id, name });
        }
    }
    Ok(devices)
}

unsafe fn friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let prop = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
    let pwstr = PropVariantToStringAlloc(&prop).ok()?;
    let name = pwstr.to_string().ok();
    CoTaskMemFree(Some(pwstr.0 as *const c_void));
    name
}

pub fn start_render_loopback(ring: SampleRing, stop: Arc<AtomicBool>) -> Result<JoinHandle<()>> {
    spawn_capture(CaptureTarget::RenderLoopback, ring, stop)
}

pub fn start_microphone(
    device_id: Option<String>,
    ring: SampleRing,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    spawn_capture(CaptureTarget::Microphone(device_id), ring, stop)
}

enum CaptureTarget {
    RenderLoopback,
    Microphone(Option<String>),
}

fn spawn_capture(
    target: CaptureTarget,
    ring: SampleRing,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("wasapi-capture".into())
        .spawn(move || {
            if let Err(e) = capture_thread(target, ring, stop) {
                log::warn!("audio capture thread ended: {e}");
            }
        })
        .map_err(|e| EncoderError::Audio(format!("spawn capture thread: {e}")))
}

fn capture_thread(
    target: CaptureTarget,
    ring: SampleRing,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let _com = ComGuard::init();
    let enumerator = enumerator()?;

    let (device, loopback) = unsafe {
        match target {
            CaptureTarget::RenderLoopback => (
                enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(|e| EncoderError::Audio(format!("default render endpoint: {e}")))?,
                true,
            ),
            CaptureTarget::Microphone(Some(id)) => {
                let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                (
                    enumerator
                        .GetDevice(PCWSTR(wide.as_ptr()))
                        .map_err(|e| EncoderError::Audio(format!("mic device by id: {e}")))?,
                    false,
                )
            }
            CaptureTarget::Microphone(None) => (
                enumerator
                    .GetDefaultAudioEndpoint(eCapture, eConsole)
                    .map_err(|e| EncoderError::Audio(format!("default mic endpoint: {e}")))?,
                false,
            ),
        }
    };

    unsafe {
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| EncoderError::Audio(format!("activate audio client: {e}")))?;

        let format_ptr = client
            .GetMixFormat()
            .map_err(|e| EncoderError::Audio(format!("get mix format: {e}")))?;
        let format = *format_ptr;
        let native_rate = format.nSamplesPerSec;
        let channels = format.nChannels;
        let bits = format.wBitsPerSample;

        let stream_flags = if loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };

        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                BUFFER_DURATION,
                0,
                format_ptr,
                None,
            )
            .map_err(|e| EncoderError::Audio(format!("initialize audio client: {e}")))?;

        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| EncoderError::Audio(format!("get capture service: {e}")))?;

        client
            .Start()
            .map_err(|e| EncoderError::Audio(format!("start audio client: {e}")))?;

        let mut resampler = Resampler::new(native_rate, SAMPLE_RATE);
        let poll = Duration::from_millis(5);

        while !stop.load(Ordering::SeqCst) {
            let mut packet = capture
                .GetNextPacketSize()
                .map_err(|e| EncoderError::Audio(format!("next packet size: {e}")))?;

            while packet > 0 {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| EncoderError::Audio(format!("get buffer: {e}")))?;

                if frames > 0 {
                    let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                    let stereo_native = if silent || data.is_null() {
                        vec![0f32; frames as usize * 2]
                    } else {
                        let byte_len = frames as usize
                            * channels as usize
                            * (bits as usize / 8);
                        let slice = std::slice::from_raw_parts(data, byte_len);
                        to_stereo_f32(slice, frames as usize, channels, bits)
                    };
                    let mut out = Vec::new();
                    resampler.process(&stereo_native, &mut out);
                    push_samples(&ring, &out);
                }

                capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| EncoderError::Audio(format!("release buffer: {e}")))?;

                packet = capture
                    .GetNextPacketSize()
                    .map_err(|e| EncoderError::Audio(format!("next packet size: {e}")))?;
            }

            std::thread::sleep(poll);
        }

        let _ = client.Stop();
        CoTaskMemFree(Some(format_ptr as *const c_void));
    }

    Ok(())
}

/// Convert an interleaved native-format frame block to interleaved stereo f32.
fn to_stereo_f32(data: &[u8], frames: usize, channels: u16, bits: u16) -> Vec<f32> {
    let bytes_per_sample = (bits / 8) as usize;
    let frame_bytes = bytes_per_sample * channels as usize;
    let mut out = Vec::with_capacity(frames * 2);

    let read = |base: usize, ch: usize| -> f32 {
        let off = base + ch * bytes_per_sample;
        if off + bytes_per_sample > data.len() {
            return 0.0;
        }
        match bits {
            32 => {
                // WASAPI shared mix format is IEEE float32.
                let b = [data[off], data[off + 1], data[off + 2], data[off + 3]];
                f32::from_le_bytes(b)
            }
            16 => {
                let b = [data[off], data[off + 1]];
                i16::from_le_bytes(b) as f32 / 32768.0
            }
            _ => 0.0,
        }
    };

    for f in 0..frames {
        let base = f * frame_bytes;
        let l = read(base, 0);
        let r = if channels >= 2 { read(base, 1) } else { l };
        out.push(l);
        out.push(r);
    }
    out
}

/// Linear stereo resampler with fractional carry across packets.
struct Resampler {
    /// output_rate / input_rate
    ratio: f64,
    pos: f64,
}

impl Resampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        let ratio = output_rate as f64 / input_rate.max(1) as f64;
        Self { ratio, pos: 0.0 }
    }

    fn process(&mut self, input_stereo: &[f32], out: &mut Vec<f32>) {
        let n = input_stereo.len() / 2;
        if n == 0 {
            return;
        }
        let step = 1.0 / self.ratio;
        while self.pos < n as f64 {
            let idx = self.pos.floor() as usize;
            let frac = (self.pos - idx as f64) as f32;
            let (l0, r0) = (input_stereo[idx * 2], input_stereo[idx * 2 + 1]);
            let (l1, r1) = if idx + 1 < n {
                (input_stereo[(idx + 1) * 2], input_stereo[(idx + 1) * 2 + 1])
            } else {
                (l0, r0)
            };
            out.push(l0 + (l1 - l0) * frac);
            out.push(r0 + (r1 - r0) * frac);
            self.pos += step;
        }
        self.pos -= n as f64;
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_rates_match() {
        let mut r = Resampler::new(48000, 48000);
        let input = vec![0.0, 0.0, 1.0, 1.0, 0.5, 0.5];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn upsamples_44100_to_48000_increases_samples() {
        let mut r = Resampler::new(44100, 48000);
        let input = vec![0.1f32; 441 * 2];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        // ~480 stereo frames out of ~441 in.
        assert!(out.len() / 2 >= 470 && out.len() / 2 <= 490);
    }

    #[test]
    fn stereo_conversion_duplicates_mono() {
        // one mono 16-bit sample at full scale.
        let data = (i16::MAX).to_le_bytes().to_vec();
        let out = to_stereo_f32(&data, 1, 1, 16);
        assert_eq!(out.len(), 2);
        assert!((out[0] - out[1]).abs() < 1e-6);
        assert!(out[0] > 0.9);
    }
}
