//! Minimal logger: writes to a rolling in-memory ring (for Control Channel log
//! uploads and the diagnostics panel) and appends to a local log file.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Metadata, Record};

const RING_CAP: usize = 1000;

#[derive(Clone)]
pub struct LogBuffer(pub Arc<Mutex<VecDeque<String>>>);

impl LogBuffer {
    pub fn snapshot(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

struct GalahadLogger {
    buffer: LogBuffer,
    file: Option<Mutex<std::fs::File>>,
}

impl log::Log for GalahadLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{}] {}: {}",
            now_stamp(),
            record.level(),
            record.args()
        );

        if let Ok(mut q) = self.buffer.0.lock() {
            if q.len() >= RING_CAP {
                q.pop_front();
            }
            q.push_back(line.clone());
        }

        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{line}");
            }
        }

        // Mirror to stderr in dev.
        eprintln!("{line}");
    }

    fn flush(&self) {}
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// Install the global logger. Returns the shared ring buffer handle.
pub fn init(log_path: Option<PathBuf>) -> LogBuffer {
    let buffer = LogBuffer(Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP))));

    let file = log_path.and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });

    let logger = GalahadLogger {
        buffer: buffer.clone(),
        file,
    };

    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    buffer
}
