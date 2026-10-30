//! Shared file locations. The service runs as LocalSystem and the app as the
//! signed-in user, so they meet in %ProgramData%\Moni (writable by Users).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const SERVICE_NAME: &str = "MoniService";

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MONI_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("Moni")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("moni")
    }
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// Rewritten by the service every sample; the app polls it.
pub fn status_path() -> PathBuf {
    data_dir().join("status.json")
}

/// Full LibreHardwareMonitor sensor list, refreshed every few seconds.
pub fn sensors_path() -> PathBuf {
    data_dir().join("sensors.json")
}

/// While this file is fresh the service releases the COM port (firmware flashing).
pub fn pause_path() -> PathBuf {
    data_dir().join("pause-usb")
}

/// The app drops this to ask the service to reboot the clock over USB.
pub fn restart_clock_path() -> PathBuf {
    data_dir().join("restart-clock")
}

/// The app drops this (holding a fan control id) to spin that header to 100 %
/// for a few seconds, so the user sees which fans are on it.
pub fn identify_fan_path() -> PathBuf {
    data_dir().join("identify-fan")
}

/// Consume a request file that holds text: its text when under 30 s old.
pub fn take_request_text(path: &std::path::Path) -> Option<String> {
    let text = fs::read_to_string(path).ok();
    take_request(path).then_some(text?).map(|t| t.trim().to_string())
}

/// Consume a request file: true when it existed and is under 30 s old.
pub fn take_request(path: &std::path::Path) -> bool {
    let fresh = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|at| SystemTime::now().duration_since(at).unwrap_or_default() < Duration::from_secs(30))
        .unwrap_or(false);
    fs::remove_file(path).is_ok() && fresh
}

pub fn log_path() -> PathBuf {
    data_dir().join("service.log")
}

pub const PAUSE_MAX_AGE: Duration = Duration::from_secs(10 * 60);

pub fn usb_paused() -> bool {
    fs::metadata(pause_path())
        .and_then(|meta| meta.modified())
        .map(|at| SystemTime::now().duration_since(at).unwrap_or_default() < PAUSE_MAX_AGE)
        .unwrap_or(false)
}

/// Write to a sibling temp file then rename, so readers never see half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
    }
    // Windows rename fails while a reader holds the target open; retry briefly.
    let mut last = Ok(());
    for _ in 0..5 {
        last = fs::rename(&temp, path);
        if last.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = fs::remove_file(&temp);
    last
}

/// Append one timestamped line to service.log, rolling it over at 512 KB.
pub fn log(message: &str) {
    let path = log_path();
    if fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{now} {message}");
    }
    eprintln!("{message}");
}
