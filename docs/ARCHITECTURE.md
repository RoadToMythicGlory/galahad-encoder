# Architecture

Galahad Encoder is split into a capability-driven UI and a Rust media runtime.
The UI collects intent; the Rust side resolves that intent into a validated,
machine-specific streaming plan.

## Runtime Shape

```text
React/Tauri UI
    |
    | tauri commands
    v
AppState
    |
    | validates config, capabilities, identity
    v
Pipeline supervisor
    |
    +-- SRT window path: WGC -> raw BGRA -> FFmpeg stdin
    +-- SRT device path: DirectShow -> FFmpeg
    +-- ST 2110 path: DirectShow -> GStreamer RTP essence
```

The supervisor owns all long-running process and thread lifecycles. Start,
stop, restart, quality switch, and pushed config all converge there so media
processes do not leak across UI actions or network failures.

## Capture And Audio

- Window capture uses Windows Graphics Capture for modern game/app capture
  instead of desktop duplication or `gdigrab`.
- Device capture uses DirectShow for cameras, capture cards, and virtual video
  devices.
- Audio is planned as independent sources: application/game audio, Discord,
  microphone, and fallback system mix when process isolation is unavailable.
- Mixed PCM is sent to FFmpeg through localhost TCP because that is reliable on
  Windows and avoids inherited file descriptor edge cases.

## Encoding And Transport

The SRT path keeps FFmpeg focused on encode, mux, and transmit:

- Raw window frames enter FFmpeg as fully specified BGRA on stdin.
- Device capture lets FFmpeg open the DirectShow source directly.
- Hardware encoders are preferred in this order: NVENC, QSV, AMF.
- Software encoding is gated because it can overload player machines.
- Output is MPEG-TS over SRT with explicit latency and reconnect-friendly
  settings.

The broadcast path uses GStreamer for ST 2110-style RTP essence because it has
native payloaders and SDP-friendly primitives for RFC 4175 video and L24 audio.

## Control Plane

The optional WebSocket control channel sends:

- stable player identity and per-launch session identity
- 1 Hz telemetry snapshots
- operator commands such as start, stop, restart, switch quality, log upload,
  and pushed config
- command acknowledgements with success/failure details

This makes the encoder useful in multi-player or event environments where an
operator needs to triage clients remotely.

## Reliability Model

The pipeline uses a reconnect state machine with capped exponential backoff.
Recoverable failures such as FFmpeg exit, network loss, or source restart move
the UI into `reconnecting`; explicit stop returns it to `idle`.

Pure planning code is unit tested separately from OS and process boundaries:
SRT URLs, FFmpeg arguments, GStreamer/ST 2110 SDP, encoder selection, command
parsing, capability parsing, and stream-state transitions.
