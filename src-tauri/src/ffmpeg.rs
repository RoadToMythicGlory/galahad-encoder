//! FFmpeg argument construction for the streaming pipeline.
//!
//! Galahad owns capture (Windows Graphics Capture) and audio mixing natively,
//! then hands FFmpeg two raw streams:
//!   * raw BGRA video on `pipe:0` (stdin)
//!   * interleaved s16le PCM audio over a localhost TCP socket
//!
//! TCP is used for audio (instead of `pipe:3`) because passing an extra inherited
//! file descriptor to a child process is awkward on Windows, whereas FFmpeg can
//! reliably connect to `tcp://127.0.0.1:<port>` as a client.
//!
//! FFmpeg's only job is encode + scale + MPEG-TS mux + SRT caller output. Keeping
//! the arg builder pure makes it unit-testable.

use crate::encoder::{Codec, EncoderBackend, Vendor};
use crate::srt::SrtDestination;

/// Where FFmpeg reads video from.
#[derive(Debug, Clone)]
pub enum VideoInput {
    /// Raw BGRA frames fed on stdin by our WGC window capture.
    RawPipe,
    /// A physical DirectShow capture device (camera / capture card) that FFmpeg
    /// opens directly with `-f dshow -i video=<name>`.
    Dshow { device: String },
}

/// Everything needed to assemble an FFmpeg invocation.
#[derive(Debug, Clone)]
pub struct FfmpegPlan {
    /// How video enters FFmpeg (stdin pipe vs. direct device capture).
    pub video_input: VideoInput,
    /// Raw frame geometry coming from capture (the window's client size, or the
    /// requested device capture size).
    pub capture_width: u32,
    pub capture_height: u32,
    /// Encoded output geometry (from the quality preset).
    pub out_width: u32,
    pub out_height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub backend: EncoderBackend,
    pub destination: SrtDestination,
    /// Listener fan-out: how many callers may pull at once (1-3). Ignored in
    /// caller mode (always a single output).
    pub max_callers: u8,
    /// When false, no audio input is added (capture-only fallback).
    pub audio_enabled: bool,
    /// FFmpeg input URL for PCM audio, e.g. `tcp://127.0.0.1:5060`.
    pub audio_input: String,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
    pub audio_kbps: u32,
}

impl FfmpegPlan {
    fn needs_scaling(&self) -> bool {
        self.capture_width != self.out_width || self.capture_height != self.out_height
    }

    /// Low-latency rate-control + preset args specific to the encoder family.
    fn video_codec_args(&self) -> Vec<String> {
        let mut args = vec!["-c:v".into(), self.backend.ffmpeg_name.to_string()];
        let bitrate = format!("{}k", self.video_kbps);

        match self.backend.vendor {
            Vendor::Nvidia => args.extend([
                "-preset".into(),
                "p3".into(),
                "-tune".into(),
                "ll".into(),
                "-rc".into(),
                "cbr".into(),
            ]),
            Vendor::Intel => args.extend([
                "-preset".into(),
                "veryfast".into(),
                "-low_power".into(),
                "0".into(),
            ]),
            Vendor::Amd => args.extend([
                "-usage".into(),
                "lowlatency".into(),
                "-rc".into(),
                "cbr".into(),
            ]),
            Vendor::Software => {
                let tune = match self.backend.codec {
                    Codec::H264 | Codec::Hevc => "zerolatency",
                };
                args.extend([
                    "-preset".into(),
                    "veryfast".into(),
                    "-tune".into(),
                    tune.into(),
                ]);
            }
        }

        args.extend([
            "-b:v".into(),
            bitrate.clone(),
            "-maxrate".into(),
            bitrate.clone(),
            "-bufsize".into(),
            bitrate,
        ]);

        // GOP ~2s, no B-frames for latency.
        let gop = (self.fps * 2).max(1).to_string();
        args.extend([
            "-g".into(),
            gop,
            "-bf".into(),
            "0".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ]);
        args
    }

    /// Build the full argument vector (excluding the `ffmpeg` program itself).
    pub fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();

        args.extend([
            "-hide_banner".into(),
            "-loglevel".into(),
            "warning".into(),
        ]);

        // --- Video input ---
        match &self.video_input {
            VideoInput::RawPipe => {
                // Raw BGRA frames on stdin (from our WGC window capture). No
                // analysis is needed (format is fully specified), so nobuffer
                // keeps latency low.
                args.extend([
                    "-fflags".into(),
                    "nobuffer".into(),
                    "-f".into(),
                    "rawvideo".into(),
                    "-pix_fmt".into(),
                    "bgra".into(),
                    "-video_size".into(),
                    format!("{}x{}", self.capture_width, self.capture_height),
                    "-framerate".into(),
                    self.fps.to_string(),
                    "-i".into(),
                    "pipe:0".into(),
                ]);
            }
            VideoInput::Dshow { device } => {
                // Direct camera / capture-card capture. We deliberately do NOT
                // pin -video_size / -framerate: many devices (webcams, virtual
                // cameras, some cards) reject an exact mode with "Could not set
                // video options". Instead we take the device's native format and
                // conform it to the profile with output filters below.
                //
                // Devices need real input analysis: many webcams deliver MJPEG,
                // whose pixel format can only be determined by decoding a frame.
                // We must NOT use -fflags nobuffer here (it suppresses that
                // analysis and yields "unspecified pixel format" / dead input).
                // Explicit analyze/probe budgets bound how long the listener waits
                // to bind while still giving MJPEG enough to be detected.
                args.extend([
                    "-f".into(),
                    "dshow".into(),
                    "-rtbufsize".into(),
                    "64M".into(),
                    "-analyzeduration".into(),
                    "5000000".into(),
                    "-probesize".into(),
                    "5000000".into(),
                    "-i".into(),
                    format!("video={device}"),
                ]);
            }
        }

        // --- Audio input: mixed PCM over TCP ---
        if self.audio_enabled {
            args.extend([
                "-f".into(),
                "s16le".into(),
                "-ar".into(),
                self.audio_sample_rate.to_string(),
                "-ac".into(),
                self.audio_channels.to_string(),
                "-i".into(),
                self.audio_input.clone(),
            ]);
        }

        // --- Conform video to the profile geometry / rate ---
        match &self.video_input {
            VideoInput::Dshow { .. } => {
                // Device native format is unknown, so always conform to the
                // profile. Crucially, drop to the target fps *before* scaling:
                // webcams / virtual cams often push 30-60 fps, and scaling every
                // one of those frames (only to discard most at output) burns CPU
                // past real-time and overflows the dshow capture buffer. The
                // `fps` filter decimates first so we only scale frames we keep.
                // `bilinear` is much cheaper than `bicubic` with negligible
                // quality loss when only mildly rescaling for a live uplink.
                args.extend([
                    "-vf".into(),
                    format!(
                        "fps={},scale={}:{}:flags=bilinear",
                        self.fps, self.out_width, self.out_height
                    ),
                ]);
            }
            VideoInput::RawPipe => {
                if self.needs_scaling() {
                    args.extend([
                        "-vf".into(),
                        format!("scale={}:{}:flags=bicubic", self.out_width, self.out_height),
                    ]);
                }
            }
        }

        // --- Video encode ---
        args.extend(self.video_codec_args());

        // --- Audio encode ---
        if self.audio_enabled {
            args.extend([
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{}k", self.audio_kbps),
                "-ar".into(),
                self.audio_sample_rate.to_string(),
            ]);
        }

        // --- Output: MPEG-TS over SRT ---
        let endpoints = self.destination.endpoints(self.max_callers);
        if endpoints.len() <= 1 {
            let url = endpoints
                .into_iter()
                .next()
                .unwrap_or_else(|| self.destination.to_url());
            args.extend([
                "-f".into(),
                "mpegts".into(),
                "-flush_packets".into(),
                "1".into(),
                url,
            ]);
        } else {
            // Fan out to multiple SRT listener ports via the tee muxer so up to
            // 3 callers can each pull from their own port. `onfail=ignore` keeps
            // the session alive if a slot never gets a caller. tee needs
            // explicit stream maps.
            args.extend(["-map".into(), "0:v:0".into()]);
            if self.audio_enabled {
                args.extend(["-map".into(), "1:a:0".into()]);
            }
            let tee = endpoints
                .iter()
                .map(|url| format!("[f=mpegts:onfail=ignore]{url}"))
                .collect::<Vec<_>>()
                .join("|");
            args.extend(["-flush_packets".into(), "1".into(), "-f".into(), "tee".into(), tee]);
        }

        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Codec, EncoderBackend, Vendor};

    fn nvenc() -> EncoderBackend {
        EncoderBackend {
            codec: Codec::H264,
            ffmpeg_name: "h264_nvenc",
            vendor: Vendor::Nvidia,
            hardware: true,
        }
    }

    fn plan() -> FfmpegPlan {
        FfmpegPlan {
            video_input: VideoInput::RawPipe,
            capture_width: 2560,
            capture_height: 1440,
            out_width: 1920,
            out_height: 1080,
            fps: 30,
            video_kbps: 6000,
            backend: nvenc(),
            destination: SrtDestination::parse("1.2.3.4", 9003, 200, crate::srt::SrtMode::Caller)
                .unwrap(),
            max_callers: 1,
            audio_enabled: true,
            audio_input: "tcp://127.0.0.1:5060".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            audio_kbps: 160,
        }
    }

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn includes_raw_video_input_at_capture_size() {
        let s = joined(&plan().build_args());
        assert!(s.contains("-f rawvideo"));
        assert!(s.contains("-pix_fmt bgra"));
        assert!(s.contains("-video_size 2560x1440"));
        assert!(s.contains("-i pipe:0"));
    }

    #[test]
    fn dshow_input_captures_from_named_device() {
        let mut p = plan();
        p.video_input = VideoInput::Dshow {
            device: "Blackmagic WDM Capture".into(),
        };
        p.capture_width = 1920;
        p.capture_height = 1080;
        let s = joined(&p.build_args());
        assert!(s.contains("-f dshow"));
        assert!(s.contains("-i video=Blackmagic WDM Capture"));
        assert!(!s.contains("-i pipe:0"));
        // Device format is not pinned on the input; we conform on the output.
        assert!(!s.contains("-video_size"));
        // fps decimation happens before the scale so we don't waste CPU
        // scaling frames we'll drop.
        assert!(s.contains("fps=30,scale=1920:1080"));
    }

    #[test]
    fn scales_to_preset_when_sizes_differ() {
        let s = joined(&plan().build_args());
        assert!(s.contains("scale=1920:1080"));
    }

    #[test]
    fn no_scale_filter_when_sizes_match() {
        let mut p = plan();
        p.capture_width = 1920;
        p.capture_height = 1080;
        let s = joined(&p.build_args());
        assert!(!s.contains("scale="));
    }

    #[test]
    fn includes_audio_tcp_input_when_enabled() {
        let s = joined(&plan().build_args());
        assert!(s.contains("-i tcp://127.0.0.1:5060"));
        assert!(s.contains("-c:a aac"));
        assert!(s.contains("-b:a 160k"));
    }

    #[test]
    fn omits_audio_when_disabled() {
        let mut p = plan();
        p.audio_enabled = false;
        let s = joined(&p.build_args());
        assert!(!s.contains("tcp://"));
        assert!(!s.contains("-c:a"));
    }

    #[test]
    fn selects_nvenc_lowlatency_args() {
        let s = joined(&plan().build_args());
        assert!(s.contains("-c:v h264_nvenc"));
        assert!(s.contains("-tune ll"));
        assert!(s.contains("-b:v 6000k"));
    }

    #[test]
    fn software_uses_zerolatency() {
        let mut p = plan();
        p.backend = EncoderBackend {
            codec: Codec::H264,
            ffmpeg_name: "libx264",
            vendor: Vendor::Software,
            hardware: false,
        };
        let s = joined(&p.build_args());
        assert!(s.contains("-c:v libx264"));
        assert!(s.contains("-tune zerolatency"));
    }

    #[test]
    fn ends_with_srt_caller_url() {
        let args = plan().build_args();
        assert!(args.last().unwrap().starts_with("srt://1.2.3.4:9003?mode=caller"));
    }

    #[test]
    fn listener_multi_caller_uses_tee_fanout() {
        let mut p = plan();
        p.destination =
            SrtDestination::parse("203.0.113.7", 9003, 200, crate::srt::SrtMode::Listener)
                .unwrap();
        p.max_callers = 3;
        let s = joined(&p.build_args());
        assert!(s.contains("-f tee"));
        assert!(s.contains("[f=mpegts:onfail=ignore]srt://0.0.0.0:9003?mode=listener"));
        assert!(s.contains("[f=mpegts:onfail=ignore]srt://0.0.0.0:9004?mode=listener"));
        assert!(s.contains("[f=mpegts:onfail=ignore]srt://0.0.0.0:9005?mode=listener"));
        // Explicit maps are required by tee.
        assert!(s.contains("-map 0:v:0"));
        assert!(s.contains("-map 1:a:0"));
    }

    #[test]
    fn listener_single_caller_uses_plain_output() {
        let mut p = plan();
        p.destination =
            SrtDestination::parse("203.0.113.7", 9003, 200, crate::srt::SrtMode::Listener)
                .unwrap();
        p.max_callers = 1;
        let s = joined(&p.build_args());
        assert!(!s.contains("-f tee"));
        assert!(s.contains("srt://0.0.0.0:9003?mode=listener"));
    }
}
