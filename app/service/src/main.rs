//! moni-service: feeds the Moni clock from boot, before anyone signs in.
//!
//!   moni-service install      register + start the Windows service (admin)
//!   moni-service uninstall    stop + remove it (admin)
//!   moni-service restart      restart it (admin)
//!   moni-service run          run the sender in this console
//!   moni-service preview [N]  print N JSON lines instead of sending
//!   moni-service sensors      list LibreHardwareMonitor sensors (admin)
//!   moni-service service      entry point used by the Service Control Manager

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod winsvc;

use moni_core::engine::{self, Options};
use moni_core::hardware::{self, Row};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result: Result<(), String> = match args.first().map(String::as_str) {
        Some("run") => {
            engine::run(Arc::new(AtomicBool::new(false)), Options { preview: false, samples: 0, use_helper: true });
            Ok(())
        }
        Some("preview") => {
            let samples = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(5);
            let use_helper = hardware::default_helper_path().exists();
            engine::run(Arc::new(AtomicBool::new(false)), Options { preview: true, samples, use_helper });
            Ok(())
        }
        Some("sensors") => list_sensors(),
        #[cfg(windows)]
        Some("service") => winsvc::dispatch(),
        #[cfg(windows)]
        Some("install") => winsvc::install(),
        #[cfg(windows)]
        Some("uninstall") => winsvc::uninstall(),
        #[cfg(windows)]
        Some("restart") => winsvc::restart(),
        _ => {
            eprintln!("{}", include_str!("usage.txt"));
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            moni_core::paths::log(&format!("error: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn list_sensors() -> Result<(), String> {
    let exe = hardware::default_helper_path();
    let output = std::process::Command::new(&exe)
        .arg("--once")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    #[derive(serde::Deserialize)]
    struct Line {
        rows: Option<Vec<Row>>,
        error: Option<String>,
    }
    let line: Line = serde_json::from_str(text.trim()).map_err(|e| format!("bad helper output: {e}"))?;
    if let Some(error) = line.error {
        return Err(error);
    }
    for row in line.rows.unwrap_or_default() {
        if ["temperature", "power", "fan", "clock"].contains(&row.kind.to_lowercase().as_str()) {
            println!("{:12} {:8.1}  {:<28} {}", row.kind, row.value, row.name, row.id);
        }
    }
    Ok(())
}
