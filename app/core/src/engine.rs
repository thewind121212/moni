//! The service's main loop: sample once per interval, feed the clock, and
//! publish status for the app.

use crate::config::{Config, Transport};
use crate::control;
use crate::hardware::{self, Choices, SensorFeed};
use crate::display::DisplayFeed;
use crate::lights::Lights;
use crate::paths;
use crate::sampler::{Payload, Sampler};
use crate::transport::{list_ports, PortInfo, SerialLink, UdpLink};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Result of the last "Restart clock", e.g. "restarted 14:02:11 on COM3".
    pub restart_note: String,
    pub control_on: bool,
    pub control_note: String,
    /// The fan lights: lighting now, and what they light or why not.
    pub lights_on: bool,
    pub lights_note: String,
    /// The digital display: showing now, and what it's doing or why not.
    pub display_on: bool,
    pub display_note: String,
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
    let mut restart_note = String::new();
    let control_key = Arc::new(Mutex::new(config.control_key.clone()));
    let control_note = (!options.preview).then(|| control::start(control_key.clone()));
    let lights = (!options.preview).then(|| Lights::start(config.lights.clone()));
    let display = (!options.preview).then(|| DisplayFeed::start(config.display.clone()));
    paths::log(&format!("moni-service {VERSION} started ({:?})", config.transport));
    if let Some(note) = &control_note {
        paths::log(&format!("remote shutdown: {}", note.lock().unwrap()));
    }

    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();

        let stamp = modified(&config_path);
        if stamp != config_stamp {
            config_stamp = stamp;
            let fresh = Config::load(&config_path);
            if fresh != config {
                paths::log(&format!("settings changed: {:?}", fresh.redacted()));
                if fresh.serial_port != config.serial_port || fresh.baud != config.baud || !fresh.transport.uses_usb() {
                    serial.close("settings changed");
                }
                *control_key.lock().unwrap() = fresh.control_key.clone();
                if let Some(lights) = &lights {
                    lights.configure(&fresh.lights);
                }
                if let Some(display) = &display {
                    display.configure(&fresh.display);
                }
                config = fresh;
            }
        }

        let payload = sampler.sample(Choices {
            fan: &config.fan_sensor,
            mobo: &config.mobo_sensor,
            ssd: &config.ssd_sensor,
        });
        if let Some(lights) = &lights {
            lights.sample(payload.cpu);
        }
        if let Some(display) = &display {
            display.sample(&payload);
        }
        let line = serde_json::to_string(&payload).unwrap_or_default();
        let control_on = control_note.is_some() && control::key_ok(&config.control_key);
        let mut hello = sampler.hello();
        hello.ctl = if control_on { control::CONTROL_PORT } else { 0 };
        let hello = serde_json::to_string(&hello).unwrap_or_default();
        let paused = paths::usb_paused();

        if !options.preview && paths::take_request(&paths::restart_clock_path()) {
            restart_note = match serial.restart_clock(&config.serial_port, config.baud) {
                Ok(name) => format!("restarted on {name}"),
                Err(error) => format!("restart failed: {error}"),
            };
            paths::log(&format!("clock {restart_note}"));
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            restart_note = format!("{now} {restart_note}");
        }

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
            let (lights_on, lights_note) = lights.as_ref().map(|l| l.status()).unwrap_or_default();
            let (display_on, display_note) = display.as_ref().map(|d| d.status()).unwrap_or_default();
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
                restart_note: restart_note.clone(),
                control_on,
                control_note: control_note.as_ref().map(|n| n.lock().unwrap().clone()).unwrap_or_default(),
                lights_on,
                lights_note,
                display_on,
                display_note,
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
    // Let go of the hardware before Windows tears the drivers down: a process
    // stuck in a USB-serial or sensor-driver call can hold up a shutdown.
    let stopping = Instant::now();
    serial.close("service stopping");
    if let Some(display) = &display {
        display.stop();
    }
    if let Some(lights) = &lights {
        // LEDs off before Windows goes (the board keeps them lit when the PC is off)
        lights.shutdown(Duration::from_millis(1500));
    }
    if let Some(feed) = &sampler.sensors {
        paths::log(&format!("stopping: sensor helper {}", feed.stop()));
    }
    paths::log(&format!("moni-service stopped ({} ms)", stopping.elapsed().as_millis()));
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
