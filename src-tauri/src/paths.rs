//! Locating the bundled FFmpeg binary and the local log directory.

use std::path::PathBuf;

/// Resolve the FFmpeg executable.
///
/// Order of preference:
///   1. A `binaries/ffmpeg.exe` shipped next to the Galahad executable (the
///      installer places it there via Tauri resources).
///   2. `ffmpeg` on PATH (developer machines / fallback).
pub fn locate_ffmpeg() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidates = [
                dir.join("binaries").join(ffmpeg_filename()),
                dir.join(ffmpeg_filename()),
                // Tauri may unpack resources under `resources/`.
                dir.join("resources").join("binaries").join(ffmpeg_filename()),
            ];
            for candidate in candidates {
                if candidate.exists() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from("ffmpeg")
}

fn ffmpeg_filename() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn gstreamer_filename() -> &'static str {
    if cfg!(windows) {
        "gst-launch-1.0.exe"
    } else {
        "gst-launch-1.0"
    }
}

/// Resolve the `gst-launch-1.0` executable used for ST 2110 output.
///
/// GStreamer is not bundled (its ST 2110 / PTP stack is large and often
/// installed system-wide), so we look for a standard install:
///   1. `GSTREAMER_1_0_ROOT_MSVC_X86_64` / `GSTREAMER_1_0_ROOT_X86_64` env `bin`.
///   2. The default MSVC install path under Program Files.
///   3. `gst-launch-1.0` on PATH.
///
/// Returns `None` when no candidate exists so capability detection can report
/// ST 2110 as unavailable instead of failing at stream start.
pub fn locate_gstreamer() -> Option<PathBuf> {
    let name = gstreamer_filename();

    for var in [
        "GSTREAMER_1_0_ROOT_MSVC_X86_64",
        "GSTREAMER_1_0_ROOT_X86_64",
        "GSTREAMER_1_0_ROOT_MINGW_X86_64",
    ] {
        if let Ok(root) = std::env::var(var) {
            let candidate = PathBuf::from(root).join("bin").join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    if cfg!(windows) {
        for base in [
            r"C:\gstreamer\1.0\msvc_x86_64\bin",
            r"C:\Program Files\gstreamer\1.0\msvc_x86_64\bin",
        ] {
            let candidate = PathBuf::from(base).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    which_on_path(name)
}

/// Best-effort PATH lookup for an executable (no external crates).
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Directory for rolling local logs: `%APPDATA%/Galahad Encoder/logs`.
pub fn log_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join("Galahad Encoder").join("logs"))
}
