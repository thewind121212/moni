//! The service's main loop: sample once per interval, feed the clock, and
//! publish status for the app.

use crate::config::{Config, Transport};
use crate::hardware::{self, SensorFeed};
use crate::paths;
use crate::sampler::{Payload, Sampler};
use crate::transport::{list_ports, PortInfo, SerialLink, UdpLink};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Written to status.json every sample; the app renders it.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Status {
    pub updated: u64,
    pub version: String,
    pub enabled: bool,
    pub transport: Transport,
    pub usb_port: Option<String>,
    pub usb_note: String,
    pub usb_paused: bool,
    pub wifi_targets: Vec<String>,
    pub wifi_note: String,
    pub sensors_ok: bool,
    pub sensors_note: String,
    pub gpu_name: String,
    pub gpu_note: String,
    pub ports: Vec<PortInfo>,
    pub sample: Option<Payload>,
}

pub struct Options {
    /// Print each line instead of sending it (`moni-service preview`).
    pub preview: bool,
    pub samples: u64,
    pub use_helper: bool,
}

pub fn run(stop: Arc<AtomicBool>, options: Options) {
    let config_path = paths::config_path();
    let mut config = Config::load(&config_path);
    let mut config_stamp = modified(&config_path);
    if !config_path.exists() && !options.preview {
        let _ = config.save(&config_path);
    }

    let feed = options
        .use_helper
        .then(|| SensorFeed::start(hardware::default_helper_path(), config.interval_ms));
    let mut sampler = Sampler::new(feed);
    let mut serial = SerialLink::new();
    let mut udp = UdpLink::new();
    let mut ports: Vec<PortInfo> = Vec::new();
    let mut ports_at: Option<Instant> = None;
    let mut sensors_at: Option<Instant> = None;
    let mut sent = 0u64;
    paths::log(&format!("moni-service {VERSION} started ({:?})", config.transport));

    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();

        let stamp = modified(&config_path);
        if stamp != config_stamp {
            config_stamp = stamp;
            let fresh = Config::load(&config_path);
            if fresh != config {
                paths::log(&format!("settings changed: {fresh:?}"));
                if fresh.serial_port != config.serial_port || fresh.baud != config.baud || !fresh.transport.uses_usb() {
                    serial.close("settings changed");
                }
                config = fresh;
            }
        }

        let payload = sampler.sample(&config.fan_sensor);
        let line = serde_json::to_string(&payload).unwrap_or_default();
        let hello = serde_json::to_string(&sampler.hello()).unwrap_or_default();
        let paused = paths::usb_paused();

        if options.preview {
            println!("{line}");
        } else if config.enabled {
            if config.transport.uses_usb() && !paused {
                serial.send(&line, &hello, &config.serial_port, config.transport == Transport::Auto, config.baud);
            } else if serial.name.is_some() {
                serial.close(if paused { "paused for firmware update" } else { "USB off" });
            }
            if config.transport.uses_wifi() {
                udp.send(&line, &hello, &config.wifi_target);
            }
        } else if serial.name.is_some() {
            serial.close("sending turned off");
        }

        if !options.preview {
            if ports_at.map(|at| at.elapsed() > Duration::from_secs(5)).unwrap_or(true) {
                ports_at = Some(Instant::now());
                ports = list_ports();
            }
            let sensor_error = sampler.sensors.as_ref().and_then(|feed| feed.error());
            let status = Status {
                updated: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                version: VERSION.into(),
                enabled: config.enabled,
                transport: config.transport,
                usb_port: serial.name.clone(),
                usb_note: if paused { "paused for firmware update".into() } else { serial.note.clone() },
                usb_paused: paused,
                wifi_targets: udp.targets(),
                wifi_note: udp.note.clone(),
                sensors_ok: sampler.sensors.is_some() && sensor_error.is_none(),
                sensors_note: sensor_error.unwrap_or_default(),
                gpu_name: sampler.gpu.name.clone(),
                gpu_note: sampler.gpu.error.clone().unwrap_or_default(),
                ports: ports.clone(),
                sample: Some(payload),
            };
            if let Ok(text) = serde_json::to_vec(&status) {
                let _ = paths::write_atomic(&paths::status_path(), &text);
            }
            if sensors_at.map(|at| at.elapsed() > Duration::from_secs(5)).unwrap_or(true) {
                sensors_at = Some(Instant::now());
                let rows = sampler.sensor_rows();
                if !rows.is_empty() {
                    if let Ok(text) = serde_json::to_vec(&rows) {
                        let _ = paths::write_atomic(&paths::sensors_path(), &text);
                    }
                }
            }
        }

        sent += 1;
        if options.samples > 0 && sent >= options.samples {
            break;
        }
        let interval = Duration::from_millis(config.interval_ms);
        while started.elapsed() < interval && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50).min(interval.saturating_sub(started.elapsed())));
        }
    }
    paths::log("moni-service stopped");
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
