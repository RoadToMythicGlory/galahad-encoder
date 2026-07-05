//! SMPTE ST 2110 output via GStreamer.
//!
//! ST 2110 is fundamentally different from the SRT path: it carries
//! *uncompressed* essence over RTP/UDP (usually multicast) with PTP timing,
//! so it is not "just another FFmpeg output URL". We drive it with GStreamer,
//! which has mature RFC 4175 (2110-20 video) and L24 (2110-30 audio) payloaders
//! plus a PTP clock, and which can source a capture device directly.
//!
//! This module is a *pure* plan + argument + SDP builder (no process spawning),
//! so it is fully unit testable. `pipeline.rs` owns the actual process lifecycle.

use std::net::Ipv4Addr;
use std::str::FromStr;

use crate::error::{EncoderError, Result};
use crate::presets::QualityPreset;

/// RTP clock rate for video RTP (90 kHz, fixed by RFC 4175 / ST 2110-20).
const VIDEO_CLOCK_RATE: u32 = 90000;
/// L24 audio essence: 48 kHz, 24-bit, stereo (ST 2110-30, class defaults).
pub const AUDIO_SAMPLE_RATE: u32 = 48000;
pub const AUDIO_CHANNELS: u32 = 2;
/// RTP MTU tuned so RFC 4175 lines pack into standard 1500-byte frames.
const RTP_MTU: u32 = 1420;

/// A validated ST 2110 RTP essence destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EssenceTarget {
    pub ip: Ipv4Addr,
    pub port: u16,
    pub multicast: bool,
}

impl EssenceTarget {
    pub fn parse(ip: &str, port: u16) -> Result<Self> {
        let ip = ip.trim();
        let addr = Ipv4Addr::from_str(ip).map_err(|_| {
            EncoderError::Config(format!("ST 2110 destination '{ip}' is not a valid IPv4 address"))
        })?;
        if port == 0 {
            return Err(EncoderError::Config(
                "ST 2110 destination port must be 1-65535".into(),
            ));
        }
        Ok(Self {
            ip: addr,
            port,
            multicast: addr.is_multicast(),
        })
    }
}

/// Audio essence (ST 2110-30) plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gst2110Audio {
    pub target: EssenceTarget,
    pub payload_type: u8,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Everything needed to assemble an ST 2110 GStreamer invocation + SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gst2110Plan {
    /// DirectShow friendly name of the capture device.
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub interlaced: bool,
    pub video: EssenceTarget,
    pub video_payload_type: u8,
    /// PTP (ST 2059) domain the media network runs on.
    pub ptp_domain: u8,
    /// Local NIC identifier for multicast egress (empty = OS default).
    pub interface: Option<String>,
    pub audio: Option<Gst2110Audio>,
}

impl Gst2110Plan {
    /// Resolve a plan from a broadcast profile + validated destinations.
    pub fn from_profile(
        device_name: &str,
        preset: &QualityPreset,
        video: EssenceTarget,
        video_payload_type: u8,
        ptp_domain: u8,
        interface: Option<String>,
        audio: Option<Gst2110Audio>,
    ) -> Result<Self> {
        if device_name.trim().is_empty() {
            return Err(EncoderError::Config(
                "no capture device selected for ST 2110 output".into(),
            ));
        }
        if !(96..=127).contains(&video_payload_type) {
            return Err(EncoderError::Config(format!(
                "ST 2110 payload type {video_payload_type} must be a dynamic type (96-127)"
            )));
        }
        Ok(Self {
            device_name: device_name.trim().to_string(),
            width: preset.width,
            height: preset.height,
            frame_rate: preset.frame_rate,
            interlaced: preset.scan.is_interlaced(),
            video,
            video_payload_type,
            ptp_domain,
            interface,
            audio,
        })
    }

    fn interlace_mode(&self) -> &'static str {
        if self.interlaced {
            "interleaved"
        } else {
            "progressive"
        }
    }

    fn udpsink_extra(&self) -> Vec<String> {
        let mut args = vec![
            "auto-multicast=true".to_string(),
            format!("ttl-mc={}", 16),
        ];
        if let Some(iface) = self.interface.as_ref().filter(|s| !s.trim().is_empty()) {
            args.push(format!("multicast-iface={}", iface.trim()));
        }
        args
    }

    /// Build the `gst-launch-1.0` argument vector (excluding the program name).
    ///
    /// The pipeline sources the DirectShow device, converts to 10-bit 4:2:2,
    /// RFC 4175 payloads it (ST 2110-20), and sends it over UDP. When audio is
    /// enabled a parallel L24 (ST 2110-30) branch is appended.
    pub fn build_pipeline_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["-e".into(), "-v".into()];

        // --- Video essence (ST 2110-20) ---
        let raster_caps = format!(
            "video/x-raw,width={w},height={h},framerate={r}/1,interlace-mode={im}",
            w = self.width,
            h = self.height,
            r = self.frame_rate,
            im = self.interlace_mode(),
        );
        let payloaded_caps = format!(
            "application/x-rtp,media=video,clock-rate={cr},encoding-name=RAW,\
             sampling=YCbCr-4:2:2,depth=(string)10,width=(string){w},height=(string){h}",
            cr = VIDEO_CLOCK_RATE,
            w = self.width,
            h = self.height,
        );

        let mut chain = vec![
            format!("dshowvideosrc device-name=\"{}\"", self.device_name),
            "queue".into(),
            "videoconvert".into(),
            "video/x-raw,format=I422_10LE".into(),
            raster_caps,
            format!(
                "rtpvrawpay pt={} mtu={}",
                self.video_payload_type, RTP_MTU
            ),
            payloaded_caps,
            format!(
                "udpsink host={host} port={port} {extra}",
                host = self.video.ip,
                port = self.video.port,
                extra = self.udpsink_extra().join(" "),
            ),
        ];

        // --- Audio essence (ST 2110-30), optional parallel branch ---
        if let Some(audio) = &self.audio {
            chain.push("dshowaudiosrc".into());
            chain.push("queue".into());
            chain.push("audioconvert".into());
            chain.push("audioresample".into());
            chain.push(format!(
                "audio/x-raw,rate={rate},channels={ch},format=S24BE",
                rate = audio.sample_rate,
                ch = audio.channels,
            ));
            chain.push(format!(
                "rtpL24pay pt={} mtu={}",
                audio.payload_type, RTP_MTU
            ));
            chain.push(format!(
                "udpsink host={host} port={port} {extra}",
                host = audio.target.ip,
                port = audio.target.port,
                extra = self.udpsink_extra().join(" "),
            ));
        }

        // gst-launch expresses linked elements as `a ! b ! c`. Separate parallel
        // branches (video, audio) are independent top-level chains.
        let video_len = if self.audio.is_some() {
            chain.len() - 6
        } else {
            chain.len()
        };

        let mut launch: Vec<String> = Vec::new();
        // Video branch.
        push_linked(&mut launch, &chain[..video_len]);
        // Audio branch (independent).
        if self.audio.is_some() {
            launch.push("\n".into());
            push_linked(&mut launch, &chain[video_len..]);
        }

        args.extend(launch);
        args
    }

    /// Whether the media network clock is expressed as a PTP reference in the SDP.
    fn ptp_refclk(&self) -> String {
        // Grandmaster identity is unknown until PTP locks; SDP carries the domain
        // and a traceable clock, receivers match on the actual gmid at runtime.
        format!("ts-refclk:ptp=IEEE1588-2008:traceable:{}", self.ptp_domain)
    }

    /// Generate the session SDP receivers use to subscribe to this stream.
    ///
    /// Follows the ST 2110-20 SDP conventions (RFC 4175 media, `fmtp` with
    /// sampling / depth / colorimetry, `ts-refclk` PTP + `mediaclk:direct`).
    pub fn to_sdp(&self, source_ip: &str) -> String {
        let mut sdp = String::new();
        sdp.push_str("v=0\r\n");
        sdp.push_str(&format!(
            "o=- 0 0 IN IP4 {src}\r\n",
            src = sdp_source(source_ip)
        ));
        sdp.push_str("s=RHEncoder ST 2110 Stream\r\n");
        sdp.push_str("t=0 0\r\n");

        // --- Video media (2110-20) ---
        sdp.push_str(&format!(
            "m=video {port} RTP/AVP {pt}\r\n",
            port = self.video.port,
            pt = self.video_payload_type
        ));
        sdp.push_str(&format!(
            "c=IN IP4 {ip}/{ttl}\r\n",
            ip = self.video.ip,
            ttl = 64
        ));
        sdp.push_str(&format!(
            "a=rtpmap:{pt} raw/{cr}\r\n",
            pt = self.video_payload_type,
            cr = VIDEO_CLOCK_RATE
        ));
        sdp.push_str(&format!("a=fmtp:{pt} {params}\r\n",
            pt = self.video_payload_type,
            params = self.video_fmtp()
        ));
        sdp.push_str("a=mediaclk:direct=0\r\n");
        sdp.push_str(&format!("a={}\r\n", self.ptp_refclk()));

        // --- Audio media (2110-30) ---
        if let Some(audio) = &self.audio {
            sdp.push_str(&format!(
                "m=audio {port} RTP/AVP {pt}\r\n",
                port = audio.target.port,
                pt = audio.payload_type
            ));
            sdp.push_str(&format!("c=IN IP4 {ip}/{ttl}\r\n", ip = audio.target.ip, ttl = 64));
            sdp.push_str(&format!(
                "a=rtpmap:{pt} L24/{rate}/{ch}\r\n",
                pt = audio.payload_type,
                rate = audio.sample_rate,
                ch = audio.channels
            ));
            // 1 ms packet time is the ST 2110-30 default (48 samples @ 48 kHz).
            sdp.push_str("a=ptime:1\r\n");
            sdp.push_str("a=mediaclk:direct=0\r\n");
            sdp.push_str(&format!("a={}\r\n", self.ptp_refclk()));
        }

        sdp
    }

    fn video_fmtp(&self) -> String {
        let mut params = vec![
            "sampling=YCbCr-4:2:2".to_string(),
            format!("width={}", self.width),
            format!("height={}", self.height),
            format!("exactframerate={}", self.frame_rate),
            "depth=10".to_string(),
            "TCS=SDR".to_string(),
            "colorimetry=BT709".to_string(),
            "PM=2110GPM".to_string(),
            "SSN=ST2110-20:2017".to_string(),
            "TP=2110TPN".to_string(),
        ];
        if self.interlaced {
            params.push("interlace".to_string());
        }
        params.join("; ")
    }
}

fn sdp_source(source_ip: &str) -> String {
    let ip = source_ip.trim();
    if ip.is_empty() {
        "0.0.0.0".to_string()
    } else {
        ip.to_string()
    }
}

/// Append `elements` joined by the gst-launch link operator ` ! `.
fn push_linked(out: &mut Vec<String>, elements: &[String]) {
    for (i, el) in elements.iter().enumerate() {
        if i > 0 {
            out.push("!".into());
        }
        out.push(el.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets;

    fn video_target() -> EssenceTarget {
        EssenceTarget::parse("239.20.20.20", 20000).unwrap()
    }

    fn plan(profile: &str, audio: bool) -> Gst2110Plan {
        let preset = presets::resolve_or_default(profile);
        let audio = if audio {
            Some(Gst2110Audio {
                target: EssenceTarget::parse("239.20.20.30", 20030).unwrap(),
                payload_type: 97,
                sample_rate: AUDIO_SAMPLE_RATE,
                channels: AUDIO_CHANNELS,
            })
        } else {
            None
        };
        Gst2110Plan::from_profile(
            "Blackmagic WDM Capture",
            preset,
            video_target(),
            96,
            0,
            Some("Ethernet 2".into()),
            audio,
        )
        .unwrap()
    }

    #[test]
    fn rejects_invalid_destination_ip() {
        assert!(EssenceTarget::parse("not-an-ip", 20000).is_err());
        assert!(EssenceTarget::parse("239.20.20.20", 0).is_err());
    }

    #[test]
    fn detects_multicast_destination() {
        assert!(EssenceTarget::parse("239.1.1.1", 5000).unwrap().multicast);
        assert!(!EssenceTarget::parse("10.0.0.5", 5000).unwrap().multicast);
    }

    #[test]
    fn rejects_non_dynamic_payload_type() {
        let preset = presets::resolve_or_default("1080p60");
        let err = Gst2110Plan::from_profile(
            "cam",
            preset,
            video_target(),
            33,
            0,
            None,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_empty_device() {
        let preset = presets::resolve_or_default("1080p60");
        assert!(
            Gst2110Plan::from_profile("  ", preset, video_target(), 96, 0, None, None).is_err()
        );
    }

    #[test]
    fn pipeline_has_device_payloader_and_sink() {
        let args = plan("1080p60", false).build_pipeline_args();
        let joined = args.join(" ");
        assert!(joined.contains("dshowvideosrc device-name=\"Blackmagic WDM Capture\""));
        assert!(joined.contains("rtpvrawpay pt=96"));
        assert!(joined.contains("udpsink host=239.20.20.20 port=20000"));
        assert!(joined.contains("sampling=YCbCr-4:2:2"));
    }

    #[test]
    fn pipeline_marks_interlaced_profiles() {
        let joined = plan("1080i50", false).build_pipeline_args().join(" ");
        assert!(joined.contains("interlace-mode=interleaved"));
        let prog = plan("1080p60", false).build_pipeline_args().join(" ");
        assert!(prog.contains("interlace-mode=progressive"));
    }

    #[test]
    fn pipeline_adds_audio_branch_when_enabled() {
        let joined = plan("1080p60", true).build_pipeline_args().join(" ");
        assert!(joined.contains("dshowaudiosrc"));
        assert!(joined.contains("rtpL24pay pt=97"));
        assert!(joined.contains("udpsink host=239.20.20.30 port=20030"));
    }

    #[test]
    fn pipeline_passes_multicast_interface() {
        let joined = plan("1080p60", false).build_pipeline_args().join(" ");
        assert!(joined.contains("multicast-iface=Ethernet 2"));
    }

    #[test]
    fn sdp_video_media_has_rfc4175_params() {
        let sdp = plan("1080p60", false).to_sdp("10.0.0.9");
        assert!(sdp.contains("m=video 20000 RTP/AVP 96"));
        assert!(sdp.contains("c=IN IP4 239.20.20.20/64"));
        assert!(sdp.contains("a=rtpmap:96 raw/90000"));
        assert!(sdp.contains("sampling=YCbCr-4:2:2"));
        assert!(sdp.contains("exactframerate=60"));
        assert!(sdp.contains("depth=10"));
        assert!(sdp.contains("ts-refclk:ptp=IEEE1588-2008:traceable:0"));
    }

    #[test]
    fn sdp_marks_interlaced() {
        let sdp = plan("1080i60", false).to_sdp("10.0.0.9");
        assert!(sdp.contains("interlace"));
        assert!(sdp.contains("exactframerate=30"));
    }

    #[test]
    fn sdp_includes_audio_when_enabled() {
        let sdp = plan("1080p60", true).to_sdp("10.0.0.9");
        assert!(sdp.contains("m=audio 20030 RTP/AVP 97"));
        assert!(sdp.contains("a=rtpmap:97 L24/48000/2"));
        assert!(sdp.contains("a=ptime:1"));
    }

    #[test]
    fn sdp_defaults_source_when_blank() {
        let sdp = plan("1080p60", false).to_sdp("");
        assert!(sdp.contains("o=- 0 0 IN IP4 0.0.0.0"));
    }
}
