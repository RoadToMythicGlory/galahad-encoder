# Contributing

This repository is public for visibility, but Galahad Encoder is a production
internal tool built for Sport 5 Israel workflows. It is not currently open for
outside contributions, direct edits, or community maintenance.

If you are inspecting the project, the best entry points are:

- `README.md` for the product and technical overview
- `docs/ARCHITECTURE.md` for design decisions
- `src-tauri/src/ffmpeg.rs` for low-latency FFmpeg planning
- `src-tauri/src/gst2110.rs` for ST 2110/GStreamer planning
- `src-tauri/src/pipeline.rs` for process supervision and reconnect behavior

Issues and pull requests may be disabled or left unanswered because this is not
a community-maintained project.
