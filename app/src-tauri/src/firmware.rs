//! Flash the bundled clock firmware with the bundled esptool.exe. The service
//! owns the COM port, so it is asked to let go first via the pause file.

use moni_core::config::Config;
use moni_core::engine::Status;
use moni_core::paths;
use moni_core::transport::resolve_port;
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Serialize)]
pub struct FirmwareInfo {
    path: String,
    size: u64,
    available: bool,
}

fn bundled(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    Ok(app.path().resource_dir().map_err(|e| e.to_string())?.join(relative))
}

#[tauri::command]
pub fn firmware_info(app: AppHandle) -> FirmwareInfo {
    let path = bundled(&app, "firmware/moni-si-hai.bin").unwrap_or_default();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    FirmwareInfo { path: path.to_string_lossy().into(), size, available: size > 0 }
}

/// Removes the pause file however flashing ends.
struct Pause;

impl Pause {
    fn start() -> Result<Pause, String> {
        paths::write_atomic(&paths::pause_path(), b"firmware update")
            .map_err(|e| format!("cannot pause the service: {e}"))?;
        Ok(Pause)
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(paths::pause_path());
    }
}

fn service_holds_port() -> bool {
    std::fs::read(paths::status_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Status>(&bytes).ok())
        .map(|s| s.usb_port.is_some() && !s.usb_paused)
        .unwrap_or(false)
}

fn emit(app: &AppHandle, line: &str) {
    let _ = app.emit("flash-log", line.to_string());
}

#[tauri::command]
pub async fn flash_firmware(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || flash(&app))
        .await
        .map_err(|e| e.to_string())?
}

fn flash(app: &AppHandle) -> Result<String, String> {
    let esptool = bundled(app, "tools/esptool.exe")?;
    let image = bundled(app, "firmware/moni-si-hai.bin")?;
    if !esptool.exists() || !image.exists() {
        return Err("esptool or the firmware image is missing from this install.".into());
    }

    let _pause = Pause::start()?;
    emit(app, "Asking the Moni service to release the USB port...");
    let deadline = Instant::now() + Duration::from_secs(6);
    while service_holds_port() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(500));

    let config = Config::load(&paths::config_path());
    let port = resolve_port(&config.serial_port, false)
        .ok_or("Clock not found. Plug it into this PC with a USB data cable and try again.")?;

    let mut last_error = String::new();
    for baud in ["460800", "115200"] {
        emit(app, &format!("Flashing {} to {port} at {baud} baud...", image.display()));
        let mut command = Command::new(&esptool);
        command
            .args(["--chip", "esp32", "--port", &port, "--baud", baud, "write-flash", "0x0"])
            .arg(&image)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn().map_err(|e| format!("cannot start esptool: {e}"))?;
        let mut stderr = child.stderr.take();
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(stderr) = stderr.as_mut() {
                let _ = stderr.read_to_string(&mut text);
            }
            text
        });
        // esptool redraws its progress bar with \r; forward each line.
        if let Some(mut stdout) = child.stdout.take() {
            let mut buffer = [0u8; 512];
            let mut line = Vec::new();
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                for &byte in &buffer[..read] {
                    if byte == b'\n' || byte == b'\r' {
                        if !line.is_empty() {
                            emit(app, String::from_utf8_lossy(&line).trim());
                            line.clear();
                        }
                    } else {
                        line.push(byte);
                    }
                }
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let stderr_text = errors.join().unwrap_or_default();
        if !stderr_text.trim().is_empty() {
            emit(app, stderr_text.trim());
        }
        if status.success() {
            emit(app, "Done. The clock restarts with the new firmware.");
            return Ok(format!("Firmware updated on {port}."));
        }
        last_error = format!("esptool failed ({status})");
        if baud != "115200" {
            emit(app, &format!("{last_error}; retrying slower..."));
        }
    }
    Err(last_error)
}
