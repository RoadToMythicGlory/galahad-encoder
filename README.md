# Galahad Encoder

[![CI](https://github.com/RoadToMythicGlory/galahad-encoder/actions/workflows/ci.yml/badge.svg)](https://github.com/RoadToMythicGlory/galahad-encoder/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-orange)
![Tauri](https://img.shields.io/badge/Tauri-2.x-24c8db)
![FFmpeg](https://img.shields.io/badge/FFmpeg-low--latency-green)
![SRT](https://img.shields.io/badge/Transport-SRT-blue)
![GStreamer](https://img.shields.io/badge/GStreamer-ST%202110-purple)

Galahad Encoder is a Windows desktop encoder for live game and broadcast workflows.
It captures a game window or video device, mixes audio sources, and streams a
low-latency feed to a receiver over SRT or SMPTE ST 2110.

This repository is a portfolio project focused on the same problems real video
streaming teams care about: capture stability, codec selection, low-latency
FFmpeg planning, reconnect behavior, operator control, and testable media
pipeline code.

## Highlights

- **Low-latency SRT path**: H.264/AAC over MPEG-TS with caller and listener modes,
  SRT latency controls, and optional listener fan-out.
- **Windows-native capture**: Windows Graphics Capture for game/app windows,
  plus DirectShow device capture for cameras and capture cards.
- **Hardware encoder selection**: NVENC, Intel QSV, AMD AMF, and software fallback
  are selected through a capability probe instead of UI guesswork.
- **GStreamer broadcast path**: ST 2110 planning for RFC 4175 video and L24 audio
  over RTP/UDP, including SDP generation for receivers.
- **Resilient supervisor**: FFmpeg/GStreamer process lifecycle, restart handling,
  and capped reconnect backoff are owned by one Rust pipeline supervisor.
- **Operator control channel**: WebSocket telemetry, command acknowledgements,
  quality switching, remote restart, config push, and log upload.
- **Capability-driven UI**: React/Tauri interface shows only the transports,
  codecs, devices, and audio modes the current machine can actually use.

## Pipeline

```text
Game window (WGC)       Raw BGRA frames
Camera / capture card   DirectShow device
        |                       |
        +----------+------------+
                   |
            Rust pipeline supervisor
                   |
        +----------+-----------+
        |                      |
   FFmpeg SRT path        GStreamer ST 2110 path
   H.264/HEVC + AAC       RFC 4175 video + L24 audio
   MPEG-TS over SRT       RTP/UDP + SDP
        |
 Control channel: telemetry, commands, logs, diagnostics
```

## Tech Stack

- **Desktop**: Tauri 2, React 18, TypeScript, Vite
- **Core**: Rust 2021, Windows APIs, Tokio, WebSocket control plane
- **Media**: FFmpeg, SRT, MPEG-TS, AAC, H.264, experimental HEVC
- **Broadcast**: GStreamer, SMPTE ST 2110 style RTP essence planning
- **Windows capture/audio**: Windows Graphics Capture, WASAPI, DirectShow

## Quick Start

### Prerequisites

- Windows 10 build 19041+ or Windows 11
- [Node.js](https://nodejs.org) 18+
- [Rust](https://rustup.rs) with the MSVC toolchain
- Visual Studio Build Tools
- Current GPU drivers for NVENC, QSV, AMF, or HEVC testing

### Install

```powershell
npm install

# Download and bundle FFmpeg. The binary is intentionally git-ignored.
pwsh ./scripts/fetch-ffmpeg.ps1
```

### Develop

```powershell
npm run tauri:dev
```

### Build Windows Installer

```powershell
npm run tauri:build
# Output: src-tauri/target/release/bundle/nsis/*.exe
```

## Test

The media planning code is deliberately pure where possible. Unit tests cover
SRT URL construction, preset selection, encoder selection, FFmpeg argument
building, reconnect state transitions, control-channel command parsing,
capability parsing, ST 2110 SDP generation, and audio resampling helpers.

```powershell
cd src-tauri
cargo test
```

Frontend type-check and bundle:

```powershell
npm run build
```

## Repository Map

| Path | Purpose |
|------|---------|
| `src/` | React/TypeScript capability-driven desktop UI |
| `src-tauri/src/capture/` | Windows Graphics Capture backend |
| `src-tauri/src/audio/` | WASAPI capture, device enumeration, and mixer logic |
| `src-tauri/src/pipeline.rs` | Streaming supervisor for capture, media processes, reconnects |
| `src-tauri/src/ffmpeg.rs` | Pure FFmpeg argument planner for low-latency SRT output |
| `src-tauri/src/gst2110.rs` | Pure GStreamer ST 2110 pipeline and SDP planner |
| `src-tauri/src/encoder.rs` | Codec/backend selection for NVENC, QSV, AMF, software |
| `src-tauri/src/control_channel.rs` | Telemetry, command parsing, acknowledgements |
| `docs/PLAYER.md` | Player setup and troubleshooting guide |
| `docs/OPERATOR.md` | Operator guide for routing, telemetry, and remote commands |
| `docs/ARCHITECTURE.md` | Architecture notes and engineering tradeoffs |

## Access

This repository is public for review and portfolio evaluation. It is not open
for external edits or direct pushes; GitHub keeps write access limited to the
repository owner unless collaborators are explicitly added.
