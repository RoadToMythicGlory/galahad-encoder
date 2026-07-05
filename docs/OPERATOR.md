# Galahad Encoder — Operator Guide

Galahad Encoder is the **player-side** counterpart to the OBS matrix receiver. The
client connects as an SRT **caller**; the server owns which input each port maps
to. Players never see slots or input IDs.

## Port → input mapping (server-side)

Configure the matrix the usual way (see `../../docs/MEDIA.md`). For direct SRT
listeners:

| Listener port | Matrix input |
|---------------|--------------|
| 9001 | input_01 |
| 9002 | input_02 |
| 9003 | input_03 |

Hand each player just their destination, e.g. `Connect to: 1.2.3.4:9003`.

Start the receiver:

```powershell
excalibur-matrix serve --port 8800 --media configs/media.srt.example.yaml --simulate
```

The player's stream is H.264 + AAC over MPEG-TS, which the matrix FFmpeg preview
tap and OBS ingest already handle.

## Control Channel (optional but recommended)

Set each client's `controlChannelUrl` (in its config, or via the UI/Push Config)
to your control-plane WebSocket endpoint, e.g.:

```
ws://1.2.3.4:8800/ws/encoder
```

Once connected, the client sends a `Hello`, then ~1 Hz **telemetry**:

```json
{
  "type": "telemetry",
  "playerId": "player-123",
  "sessionId": "…",
  "displayName": "Idan",
  "source": "cs2.exe — Counter-Strike 2",
  "streaming": true,
  "state": "streaming",
  "fps": 30,
  "bitrate": 6160,
  "droppedFrames": 0,
  "encoder": "h264_nvenc",
  "captureBackend": "wgc",
  "lastError": null
}
```

`playerId` is stable per player; `sessionId` is unique per app launch, so
disconnect / reconnect / PC restart appear as distinct sessions.

### Commands you can send

```json
{ "action": "start_stream" }
{ "action": "stop_stream" }
{ "action": "restart_stream" }
{ "action": "switch_quality", "preset": "720p30" }
{ "action": "request_log_upload" }
{ "action": "push_config", "config": { "presetId": "1080p30", "destinationPort": 9003 } }
```

The client acks each command:

```json
{ "type": "command_ack", "action": "switch_quality", "ok": true, "detail": null }
```

This lets you triage and fix players remotely without TeamViewer.

## HEVC (experimental)

HEVC is **off by default**. Before enabling it for an event, validate the full
receiver path under multi-player load (SRT ingest, FFmpeg preview generation, OBS
ingest/program path, scene switching) and watch server CPU — a software-decode
fallback can overload the receiver with many feeds.
