//! Local HLS preview of the encoded output feed.
//!
//! The encoder's real output is SRT MPEG-TS (or ST 2110 RTP), neither of which a
//! WebView `<video>` element can play. To let an operator watch what is actually
//! going out, the SRT FFmpeg process tees its already-encoded packets into a
//! rolling HLS playlist on disk (no re-encode), and this module serves that
//! playlist over a tiny localhost HTTP server the preview window loads with
//! hls.js.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// Relative playlist filename FFmpeg writes into the preview directory. Kept
/// relative (no drive letter / backslashes) so it needs no escaping in the
/// FFmpeg `tee` output spec; the child process runs with its working directory
/// set to the preview directory.
pub const PLAYLIST_NAME: &str = "preview.m3u8";

/// Snapshot the preview window polls to decide what to show.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStatus {
    /// True when an HLS playlist is being produced and can be loaded now.
    pub available: bool,
    /// `http://127.0.0.1:<port>/preview.m3u8` when available.
    pub url: Option<String>,
    /// Human-readable explanation when not available (idle / unsupported).
    pub reason: Option<String>,
}

struct Runtime {
    /// A stream that writes the HLS preview is active.
    active: bool,
    /// Explanation shown when preview is not available.
    note: Option<String>,
}

/// Owns the preview directory, the localhost HLS server, and preview state.
pub struct PreviewManager {
    dir: PathBuf,
    server_port: Mutex<Option<u16>>,
    runtime: Mutex<Runtime>,
}

impl PreviewManager {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            server_port: Mutex::new(None),
            runtime: Mutex::new(Runtime {
                active: false,
                note: None,
            }),
        }
    }

    /// Prepare HLS preview for an SRT session: (re)create + clear the directory
    /// and make sure the HTTP server is running. Returns the directory the
    /// FFmpeg child must use as its working directory. Does not itself flip the
    /// preview to "active" — the session lifecycle does that via `set_active`.
    pub fn prepare_srt(&self) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        self.clear_dir();
        self.ensure_server()?;
        Ok(self.dir.clone())
    }

    /// Flip the preview between the active (streaming) and idle states. When
    /// active the "waiting" note is cleared so `status` returns the live URL.
    pub fn set_active(&self, active: bool) {
        let mut rt = self.runtime.lock().unwrap();
        rt.active = active;
        if active {
            rt.note = None;
        }
    }

    /// Mark preview unavailable for a transport that cannot be previewed in-app
    /// yet (e.g. ST 2110), with an explanation for the window.
    pub fn begin_unsupported(&self, note: impl Into<String>) {
        let mut rt = self.runtime.lock().unwrap();
        rt.active = false;
        rt.note = Some(note.into());
    }

    /// A stream stopped; preview goes back to the idle "waiting" state.
    pub fn end(&self) {
        let mut rt = self.runtime.lock().unwrap();
        rt.active = false;
        rt.note = None;
    }

    pub fn status(&self) -> PreviewStatus {
        let rt = self.runtime.lock().unwrap();
        let port = *self.server_port.lock().unwrap();
        match (rt.active, port) {
            (true, Some(port)) => PreviewStatus {
                available: true,
                url: Some(format!("http://127.0.0.1:{port}/{PLAYLIST_NAME}")),
                reason: None,
            },
            _ => PreviewStatus {
                available: false,
                url: None,
                reason: Some(
                    rt.note
                        .clone()
                        .unwrap_or_else(|| "Start an SRT stream to preview the encoded output.".into()),
                ),
            },
        }
    }

    /// Remove stale playlist / segment files so a new session starts clean.
    fn clear_dir(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let remove = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("ts") || e.eq_ignore_ascii_case("m3u8"))
                    .unwrap_or(false);
                if remove {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    /// Start the static file server once; subsequent calls are no-ops.
    fn ensure_server(&self) -> std::io::Result<()> {
        let mut slot = self.server_port.lock().unwrap();
        if slot.is_some() {
            return Ok(());
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let dir = self.dir.clone();
        std::thread::Builder::new()
            .name("preview-http".into())
            .spawn(move || serve(listener, dir))
            .ok();
        *slot = Some(port);
        log::info!("preview HLS server on http://127.0.0.1:{port}/{PLAYLIST_NAME}");
        Ok(())
    }
}

/// Accept loop for the preview HTTP server. One thread per connection keeps the
/// implementation trivial; hls.js issues only a handful of small requests.
fn serve(listener: TcpListener, dir: PathBuf) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, &dir);
                });
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(mut stream: TcpStream, dir: &Path) -> std::io::Result<()> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buf[..n]);
    match parse_request_target(&request).and_then(resolve_file) {
        Some((name, content_type)) => match std::fs::read(dir.join(name)) {
            Ok(bytes) => write_response(&mut stream, "200 OK", content_type, &bytes),
            Err(_) => write_not_found(&mut stream),
        },
        None => write_not_found(&mut stream),
    }
}

/// Extract the requested path from the HTTP request line, e.g.
/// `GET /preview.m3u8?_=123 HTTP/1.1` -> `preview.m3u8`.
fn parse_request_target(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let target = target.split('?').next().unwrap_or(target);
    Some(target.trim_start_matches('/').to_string())
}

/// Validate the requested filename and map it to a content type. Only the flat
/// playlist and `.ts` segments in the preview directory are ever served; any
/// path separator or traversal is rejected.
fn resolve_file(name: String) -> Option<(String, &'static str)> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let content_type = if name.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if name.ends_with(".ts") {
        "video/mp2t"
    } else {
        return None;
    };
    Some((name, content_type))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-cache, no-store, must-revalidate\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\r\n",
        len = body.len(),
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_not_found(stream: &mut TcpStream) -> std::io::Result<()> {
    write_response(stream, "404 Not Found", "text/plain", b"not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_target_and_strips_query() {
        assert_eq!(
            parse_request_target("GET /preview.m3u8?_=1 HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some("preview.m3u8".to_string())
        );
    }

    #[test]
    fn rejects_non_get() {
        assert_eq!(parse_request_target("POST /preview.m3u8 HTTP/1.1"), None);
    }

    #[test]
    fn resolves_playlist_and_segments() {
        assert_eq!(
            resolve_file("preview.m3u8".into()),
            Some(("preview.m3u8".into(), "application/vnd.apple.mpegurl"))
        );
        assert_eq!(
            resolve_file("preview3.ts".into()),
            Some(("preview3.ts".into(), "video/mp2t"))
        );
    }

    #[test]
    fn rejects_traversal_and_other_files() {
        assert_eq!(resolve_file("../secret.ts".into()), None);
        assert_eq!(resolve_file("sub/dir.ts".into()), None);
        assert_eq!(resolve_file("config.json".into()), None);
    }
}
