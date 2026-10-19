//! Moni desktop app: tray icon, settings, live status and firmware updates.
//! The telemetry itself comes from moni-service, which runs from boot; this
//! app only edits %ProgramData%\Moni\config.json and reads status.json.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod firmware;

use moni_core::config::Config;
use moni_core::engine::Status;
use moni_core::hardware::Row;
use moni_core::paths;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

#[derive(Serialize)]
struct StatusView {
    status: Option<Status>,
    /// Seconds since the service last wrote status.json.
    age: Option<u64>,
    /// "running", "stopped", "not installed", ...
    service: String,
}

#[tauri::command]
fn get_config() -> Config {
    Config::load(&paths::config_path())
}

#[tauri::command]
fn save_config(config: Config) -> Result<Config, String> {
    let config = config.sanitized();
    config
        .save(&paths::config_path())
        .map_err(|e| format!("Could not save settings ({e}). Reinstall Moni so the service can prepare its folder."))?;
    Ok(config)
}

#[tauri::command]
fn get_status() -> StatusView {
    let status: Option<Status> = std::fs::read(paths::status_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let age = status.as_ref().map(|s| now.saturating_sub(s.updated));
    StatusView { status, age, service: service_state() }
}

#[tauri::command]
fn get_sensors() -> Vec<Row> {
    std::fs::read(paths::sensors_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let launcher = app.autolaunch();
    if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())?;
    Ok(launcher.is_enabled().unwrap_or(enabled))
}

/// Ask the service (it owns the COM port) to reboot the clock over USB.
#[tauri::command]
fn restart_clock() -> Result<(), String> {
    paths::write_atomic(&paths::restart_clock_path(), b"restart")
        .map_err(|e| format!("Could not ask the service ({e}). Reinstall Moni so it can prepare its folder."))
}

/// Restarting a LocalSystem service needs admin: ask through UAC.
#[tauri::command]
fn restart_service(app: AppHandle) -> Result<(), String> {
    let exe = install_dir(&app)?.join("moni-service.exe");
    elevate(&exe.to_string_lossy(), "restart")
}

#[tauri::command]
fn open_data_folder() -> Result<(), String> {
    let dir = paths::data_dir();
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(windows))]
    let opener = "xdg-open";
    std::process::Command::new(opener).arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn install_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path().resource_dir().map_err(|e| e.to_string())
}

#[cfg(windows)]
fn elevate(exe: &str, args: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -WindowStyle Hidden -Wait",
        exe.replace('\'', "''"),
        args.replace('\'', "''")
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("Restart was cancelled or failed.".into()) }
}

#[cfg(not(windows))]
fn elevate(_exe: &str, _args: &str) -> Result<(), String> {
    Err("Only available on Windows".into())
}

#[cfg(windows)]
fn service_state() -> String {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
        return "unknown".into();
    };
    let Ok(service) = manager.open_service(paths::SERVICE_NAME, ServiceAccess::QUERY_STATUS) else {
        return "not installed".into();
    };
    match service.query_status().map(|s| s.current_state) {
        Ok(ServiceState::Running) => "running",
        Ok(ServiceState::Stopped) => "stopped",
        Ok(ServiceState::StartPending) => "starting",
        Ok(ServiceState::StopPending) => "stopping",
        Ok(_) => "paused",
        Err(_) => "unknown",
    }
    .into()
}

#[cfg(not(windows))]
fn service_state() -> String {
    "not installed".into()
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_status,
            get_sensors,
            get_autostart,
            set_autostart,
            restart_service,
            restart_clock,
            open_data_folder,
            firmware::flash_firmware,
            firmware::firmware_info,
        ])
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open Moni", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit (clock keeps running)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut tray = TrayIconBuilder::with_id("moni")
                .tooltip("Moni clock")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // Started at login with --minimized: stay in the tray.
            if !std::env::args().any(|a| a == "--minimized") {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it; the tray icon stays.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Moni");
}
