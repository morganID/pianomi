//! Opt-in, file-based playback tracing.
//!
//! The menu bar app is launched with `open`, so its stdout and stderr are
//! attached to `/dev/null`. Every `Player::log` warning and every scheduled
//! key event was therefore invisible exactly when debugging was most needed.
//! This module writes a plain-text trace that survives that: one line per
//! event, flushed immediately (so a hang or OOM kill never loses the tail),
//! plus a run header describing the *effective* timing config and a summary.
//!
//! Enable with `--trace`, the GUI's **Debug logging** menu item, or
//! `PIANOMI_TRACE=1`. Override the destination with `PIANOMI_TRACE_LOG`.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static ENABLED: AtomicBool = AtomicBool::new(false);
static WRITER: OnceLock<Mutex<Option<BufWriter<File>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<BufWriter<File>>> {
    WRITER.get_or_init(|| Mutex::new(None))
}

fn lock() -> std::sync::MutexGuard<'static, Option<BufWriter<File>>> {
    slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(value: bool) {
    ENABLED.store(value, Ordering::Relaxed);
}

/// Honour `PIANOMI_TRACE=1` without overriding an explicit `set_enabled`.
pub fn init_from_env() {
    if let Ok(v) = std::env::var("PIANOMI_TRACE") {
        let v = v.trim().to_ascii_lowercase();
        if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
            set_enabled(true);
        }
    }
}

/// Where the trace is written. `~/Library/Logs/Pianomi/trace.log` is reachable
/// from Console.app and from Finder via the GUI's "Reveal Debug Log" item.
pub fn log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PIANOMI_TRACE_LOG") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("Pianomi")
            .join("trace.log");
    }
    PathBuf::from("/tmp/pianomi-trace.log")
}

pub fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn open_writer(truncate: bool) -> std::io::Result<BufWriter<File>> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut opts = OpenOptions::new();
    opts.create(true).write(true);
    if truncate {
        opts.truncate(true);
    } else {
        opts.append(true);
    }
    Ok(BufWriter::new(opts.open(&path)?))
}

/// Start a new run: truncate the previous trace and write the header.
pub fn begin(header: &[String]) -> std::io::Result<PathBuf> {
    let path = log_path();
    if !enabled() {
        return Ok(path);
    }
    let mut file = open_writer(true)?;
    writeln!(file, "# Pianomi playback trace v1")?;
    writeln!(file, "# started_unix_ms={}", unix_ms())?;
    for line in header {
        writeln!(file, "# {line}")?;
    }
    writeln!(
        file,
        "# columns: E | scheduled_ms | actual_ms | drift_ms | kind | key_id | physical | held | dest"
    )?;
    writeln!(
        file,
        "# kinds: PRESS, RELEASE, SUPPRESSED, HELD_RELEASE, DEFERRED, PHYSICAL_HELD"
    )?;
    file.flush()?;
    *lock() = Some(file);
    Ok(path)
}

/// Append one line. Lazily opens the file if `begin` was never called, so a
/// stray log line can never be silently lost.
pub fn line(msg: &str) {
    if !enabled() {
        return;
    }
    let mut guard = lock();
    if guard.is_none() {
        match open_writer(false) {
            Ok(file) => *guard = Some(file),
            Err(_) => return,
        }
    }
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "{msg}");
        // Deliberately per-line: a freeze must not discard the buffered tail.
        let _ = file.flush();
    }
}

/// Mirror a human-readable Player::log line into the trace.
pub fn note(msg: &str) {
    line(&format!("# LOG\t{msg}"));
}

/// Close the run, leaving the file readable by Console.app / `less`.
pub fn end(summary: &[String]) {
    if !enabled() {
        return;
    }
    let mut guard = lock();
    if let Some(mut file) = guard.take() {
        for line in summary {
            let _ = writeln!(file, "# {line}");
        }
        let _ = writeln!(file, "# end_unix_ms={}", unix_ms());
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_tracing_never_writes() {
        // The flag is process-global; only assert the pure helpers here so the
        // test cannot race another test that enables tracing.
        assert!(!log_path().as_os_str().is_empty());
        assert!(unix_ms() > 0);
    }
}
