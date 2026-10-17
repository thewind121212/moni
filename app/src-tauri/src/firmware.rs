//! Flash the bundled clock firmware over USB with espflash (no external
//! tools). The service owns the COM port, so it is asked to let go first via
//! the pause file.

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::{Chip, ProgressCallbacks};
use moni_core::config::Config;
use moni_core::engine::Status;
use moni_core::paths;
use moni_core::transport::resolve_port;
use serde::Serialize;
use serialport::{SerialPortType, UsbPortInfo};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Serialize)]
pub struct FirmwareInfo {
    path: String,
    size: u64,
    available: bool,
}

fn bundled_image(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().resource_dir().map_err(|e| e.to_string())?.join("firmware").join("moni-si-hai.bin"))
}

#[tauri::command]
pub fn firmware_info(app: AppHandle) -> FirmwareInfo {
    let path = bundled_image(&app).unwrap_or_default();
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

/// Reports progress to the UI in 10% steps.
struct Progress<'a> {
    app: &'a AppHandle,
    total: usize,
    last: usize,
}

impl ProgressCallbacks for Progress<'_> {
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total.max(1);
        self.last = 0;
        emit(self.app, &format!("Writing {total} bytes at {addr:#x}..."));
    }
    fn update(&mut self, current: usize) {
        let percent = current * 100 / self.total;
        if percent >= self.last + 10 {
            self.last = percent - percent % 10;
            emit(self.app, &format!("Writing... {}%", self.last));
        }
    }
    fn verifying(&mut self) {
        emit(self.app, "Verifying...");
    }
    fn finish(&mut self, skipped: bool) {
        emit(self.app, if skipped { "Already up to date (skipped)." } else { "Written." });
    }
}

#[tauri::command]
pub async fn flash_firmware(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || flash(&app))
        .await
        .map_err(|e| e.to_string())?
}

fn usb_info(port: &str) -> UsbPortInfo {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.port_name.eq_ignore_ascii_case(port))
        .and_then(|p| match p.port_type {
            SerialPortType::UsbPort(info) => Some(info),
            _ => None,
        })
        .unwrap_or(UsbPortInfo { vid: 0, pid: 0, serial_number: None, manufacturer: None, product: None })
}

fn write_image(app: &AppHandle, port: &str, image: &[u8], baud: u32) -> Result<(), String> {
    // espflash connects at 115200, then switches to `baud` for the transfer.
    let serial = serialport::new(port, 115_200)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(|e| format!("cannot open {port}: {e}"))?;
    let connection = Connection::new(
        serial,
        usb_info(port),
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        115_200,
    );
    emit(app, &format!("Connecting to the clock on {port}..."));
    let mut flasher = Flasher::connect(connection, true, true, false, Some(Chip::Esp32), Some(baud))
        .map_err(|e| format!("could not enter the bootloader: {e}"))?;
    let mut progress = Progress { app, total: 1, last: 0 };
    flasher
        .write_bin_to_flash(0x0, image, &mut progress)
        .map_err(|e| format!("write failed: {e}"))
}

fn flash(app: &AppHandle) -> Result<String, String> {
    let path = bundled_image(app)?;
    let image = std::fs::read(&path).map_err(|e| format!("firmware image missing from this install: {e}"))?;
    // A full image starts with the bootloader at 0x1000; refuse a bare app
    // image, which would overwrite the bootloader at 0x0.
    if image.len() < 0x10001 || image[0x1000] != 0xE9 || image[0x8000..0x8002] != [0xAA, 0x50] {
        return Err("The bundled firmware is not a full flash image; refusing to write it.".into());
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
    for baud in [460_800, 115_200] {
        match write_image(app, &port, &image, baud) {
            Ok(()) => {
                emit(app, "Done. The clock restarts with the new firmware.");
                return Ok(format!("Firmware updated on {port}."));
            }
            Err(error) => {
                last_error = error;
                if baud != 115_200 {
                    emit(app, &format!("{last_error}; retrying slower..."));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
    Err(last_error)
}
