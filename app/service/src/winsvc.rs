//! Windows Service Control Manager glue.

use moni_core::control::CONTROL_PORT;
use moni_core::engine::{self, Options};
use moni_core::paths::{self, SERVICE_NAME};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
    ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const DISPLAY_NAME: &str = "Moni Clock Telemetry";
const DESCRIPTION: &str = "Streams CPU/GPU/thermal telemetry to the Moni ESP32 clock over USB and WiFi.";

define_windows_service!(ffi_service_main, service_main);

pub fn dispatch() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| format!("service dispatcher: {e}"))
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        paths::log(&format!("service failed: {error}"));
    }
}

fn run_service() -> windows_service::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            signal.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state: ServiceState, controls: ServiceControlAccept| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(3),
        process_id: None,
    };
    status.set_service_status(report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ))?;

    prepare_data_dir();
    engine::run(stop, Options { preview: false, samples: 0, use_helper: true });

    status.set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

/// %ProgramData%\Moni, modifiable by Users so the app (not elevated) can save
/// settings. Only plain settings live there; nothing in it is executed.
fn prepare_data_dir() {
    let dir = paths::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/grant", "*S-1-5-32-545:(OI)(CI)M", "/T", "/Q"])
        .output();
}

fn install_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
}

/// Kill processes whose command line contains `needle` (case-insensitive).
fn kill_matching(needle: &str) -> usize {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always).with_exe(UpdateKind::Always),
    );
    let needle = needle.to_lowercase();
    let own = std::process::id();
    let mut killed = 0;
    for (pid, process) in system.processes() {
        if pid.as_u32() == own {
            continue;
        }
        let mut text = process.name().to_string_lossy().to_lowercase();
        for arg in process.cmd() {
            text.push(' ');
            text.push_str(&arg.to_string_lossy().to_lowercase());
        }
        if text.contains(&needle) && process.kill() {
            killed += 1;
        }
    }
    killed
}

/// Before this app, telemetry came from host\monitor_windows.py started by a
/// Startup-folder shortcut. Stop it and remove the shortcut so it cannot
/// fight the service for the COM port.
fn retire_python_sender() {
    if kill_matching("monitor_windows.py") > 0 {
        paths::log("stopped the old Python sender");
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let shortcut = std::path::PathBuf::from(appdata)
            .join("Microsoft\\Windows\\Start Menu\\Programs\\Startup\\Moni ESP32 Display.lnk");
        if std::fs::remove_file(&shortcut).is_ok() {
            paths::log("removed the old Python sender's startup shortcut");
        }
    }
}

/// LibreHardwareMonitor reads Ryzen/SuperIO sensors through the signed PawnIO
/// driver. Its setup exits without changes when PawnIO is already installed.
fn install_pawnio() {
    let setup = install_dir().join("tools").join("PawnIO_setup.exe");
    if !setup.exists() {
        return;
    }
    match std::process::Command::new(&setup).args(["-install", "-silent"]).status() {
        Ok(status) => paths::log(&format!("PawnIO setup finished ({status})")),
        Err(error) => paths::log(&format!("PawnIO setup failed: {error}")),
    }
}

/// The helper normally exits with the service; make sure before files are replaced.
fn stop_helper() {
    for _ in 0..20 {
        if kill_matching("moni-sensors.exe") == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

const FIREWALL_RULE: &str = "Moni remote shutdown";

/// Let moni-relay on the LAN reach the remote-shutdown listener (it still
/// refuses everything until a key is set in Moni). Local subnet only, every
/// network profile: home Ethernet is often marked Public.
fn allow_control_port() {
    remove_firewall_rule();
    let exe = std::env::current_exe().unwrap_or_default();
    let result = std::process::Command::new("netsh")
        .args(["advfirewall", "firewall", "add", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .args(["dir=in", "action=allow", "protocol=TCP", "remoteip=localsubnet", "profile=any"])
        .arg(format!("localport={CONTROL_PORT}"))
        .arg(format!("program={}", exe.display()))
        .output();
    match result {
        Ok(out) if out.status.success() => paths::log(&format!("firewall: allowed TCP {CONTROL_PORT} from the local subnet")),
        Ok(out) => paths::log(&format!("firewall rule failed: {}", String::from_utf8_lossy(&out.stdout).trim())),
        Err(error) => paths::log(&format!("firewall rule failed: {error}")),
    }
}

fn remove_firewall_rule() {
    let _ = std::process::Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .output();
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|e| format!("open service manager (run as administrator): {e}"))
}

fn wait_for(service: &windows_service::service::Service, state: ServiceState) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match service.query_status() {
            Ok(status) if status.current_state == state => return,
            Err(_) => return,
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn stop(service: &windows_service::service::Service) {
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            wait_for(service, ServiceState::Stopped);
        }
    }
}

pub fn install() -> Result<(), String> {
    prepare_data_dir();
    retire_python_sender();
    install_pawnio();
    allow_control_port();
    let manager = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe().map_err(|e| e.to_string())?,
        launch_arguments: vec!["service".into()],
        dependencies: vec![],
        account_name: None, // LocalSystem: needed for the PawnIO sensor driver.
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::DELETE;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            stop(&service);
            service.change_config(&info).map_err(|e| format!("update service: {e}"))?;
            service
        }
        Err(_) => manager.create_service(&info, access).map_err(|e| format!("create service: {e}"))?,
    };
    let _ = service.set_description(DESCRIPTION);
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) };
    let _ = service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    });
    let _ = service.set_failure_actions_on_non_crash_failures(true);
    service.start(&[] as &[&OsStr]).map_err(|e| format!("start service: {e}"))?;
    paths::log("service installed and started");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let manager = manager(ServiceManagerAccess::CONNECT)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    remove_firewall_rule();
    let Ok(service) = manager.open_service(SERVICE_NAME, access) else {
        stop_helper();
        return Ok(());
    };
    stop(&service);
    stop_helper();
    service.delete().map_err(|e| format!("delete service: {e}"))?;
    paths::log("service removed");
    Ok(())
}

pub fn restart() -> Result<(), String> {
    let manager = manager(ServiceManagerAccess::CONNECT)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP;
    let service = manager.open_service(SERVICE_NAME, access).map_err(|e| format!("open service: {e}"))?;
    stop(&service);
    service.start(&[] as &[&OsStr]).map_err(|e| format!("start service: {e}"))?;
    Ok(())
}
