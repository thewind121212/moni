//! The Jonsbo digital display (USB HID 5131:2007, the "Digital Display" in
//! JONSBO PC Monitor): the service feeds it from boot, so that app (an
//! Electron program that starts with Windows) isn't needed.
//!
//! The protocol is what JONSBO PC Monitor 1.0.6 sends
//! (resources/app.asar: main/_baseClass/device_hid.js): one 64-byte output
//! report every 200 ms. Two layouts, picked by the HID collection's usage page:
//!
//!   page 12 ("new" boards):  00 40 | cpu °  cpu %  cpu MHz(be16)  fan rpm(be16)  cpu W(be16)
//!                                  | gpu °  gpu %  gpu MHz(be16)  fan rpm(be16)  gpu W(be16)
//!                                  | ram %  ram MB(be16)  disk %  disk GB(be16)  (zeros)
//!   any other page:          01 00 01 02 | values split in hundreds (see [`classic`]),
//!                                  the date and time, and display-mode bytes
//!
//! Temperatures are whole degrees, °C, or °F with the unit byte set (classic)
//! or +128 on the value (page 12), as the app does.

use crate::config::Display as Settings;
use crate::sampler::Payload;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const VID: u16 = 0x5131;
pub const PID: u16 = 0x2007;
/// The usage page of the boards that take the short layout.
pub const NEW_PAGE: u16 = 12;
const SEND_EVERY: Duration = Duration::from_millis(200);
const RESCAN: Duration = Duration::from_secs(5);

/// What the display shows, from one Moni sample.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub cpu_temp: f64,
    pub cpu_util: f64,
    pub cpu_mhz: f64,
    pub cpu_watts: f64,
    pub cpu_fan: f64,
    pub gpu_temp: f64,
    pub gpu_util: f64,
    pub gpu_mhz: f64,
    pub gpu_watts: f64,
    pub ram_util: f64,
    pub ram_gb: f64,
    pub disk_util: f64,
    pub disk_gb: f64,
    /// Local time: year, month, day, hour, minute, second, weekday (0 = Sunday).
    pub time: [u16; 7],
}

impl Reading {
    pub fn from_sample(p: &Payload, time: [u16; 7]) -> Reading {
        let used = p.disk.clamp(0.0, 99.9) / 100.0;
        Reading {
            cpu_temp: p.ct,
            cpu_util: p.cpu,
            cpu_mhz: p.pf as f64,
            cpu_watts: p.cpuw,
            cpu_fan: p.fan as f64,
            gpu_temp: p.gt,
            gpu_util: p.gpu,
            gpu_mhz: p.gf as f64,
            gpu_watts: p.gpuw,
            ram_util: p.ram,
            ram_gb: p.ramgb,
            disk_util: p.disk,
            disk_gb: if p.free > 0.0 { p.free / (1.0 - used) } else { 0.0 },
            time,
        }
    }
}

/// The app's getIntValue: capped (999 for temperatures, 9999 otherwise), whole.
fn whole(v: f64, cap: i64) -> i64 {
    if v.is_finite() { v.clamp(0.0, cap as f64).floor() as i64 } else { 0 }
}

fn hundredths(v: f64) -> i64 {
    if v.is_finite() && v > 0.0 { ((v - v.floor()) * 100.0).floor() as i64 } else { 0 }
}

fn be16(v: i64) -> [u8; 2] {
    let v = v.clamp(0, 0xffff) as u16;
    v.to_be_bytes()
}

fn fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// The short layout (usage page 12).
pub fn short(r: &Reading, s: &Settings) -> [u8; 64] {
    let temp = |c: f64| (whole(c, 999) + if s.fahrenheit { 128 } else { 0 }) as u8;
    let mut out = [0u8; 64];
    let fields: Vec<u8> = [
        vec![0x00, 0x40, temp(r.cpu_temp), whole(r.cpu_util, 9999) as u8],
        be16(whole(r.cpu_mhz, 9999)).to_vec(),
        be16(whole(r.cpu_fan, 9999)).to_vec(),
        be16(whole(r.cpu_watts, 9999)).to_vec(),
        vec![temp(r.gpu_temp), whole(r.gpu_util, 9999) as u8],
        be16(whole(r.gpu_mhz, 9999)).to_vec(),
        be16(0).to_vec(), // GPU fan: not measured
        be16(whole(r.gpu_watts, 9999)).to_vec(),
        vec![whole(r.ram_util, 9999) as u8],
        be16(((r.ram_gb * 10.0).round() / 10.0 * 1024.0 / 16.0).floor() as i64 * 16).to_vec(),
        vec![whole(r.disk_util, 9999) as u8],
        be16(whole(r.disk_gb, 65535)).to_vec(),
    ]
    .concat();
    out[..fields.len()].copy_from_slice(&fields);
    out
}

/// The classic layout: report 1, numbers split as the app does (whole /
/// hundredths, or hundreds / rest), then date, time and display settings.
pub fn classic(r: &Reading, s: &Settings) -> [u8; 64] {
    let unit = s.fahrenheit as i64;
    let shown = |c: f64| if s.fahrenheit { fahrenheit(c) } else { c };
    let (ct, gt) = (shown(r.cpu_temp), shown(r.gpu_temp));
    let cpu_w = whole(r.cpu_watts, 9999);
    let gpu_w = whole(r.gpu_watts, 9999);
    let cpu_mhz = whole(r.cpu_mhz, 9999);
    let gpu_mhz = whole(r.gpu_mhz, 9999);
    let fan = whole(r.cpu_fan, 9999);
    let [year, month, day, hour, minute, second, weekday] = r.time.map(i64::from);
    let mode = s.mode as i64;
    let v: [i64; 52] = [
        1, 0, 1, 2,
        whole(ct, 999), hundredths(ct), unit, whole(r.cpu_util, 9999),
        cpu_w % 100, hundredths(r.cpu_watts), cpu_mhz / 100, cpu_mhz % 100,
        0, 0, // CPU voltage: not measured
        whole(gt, 999), hundredths(gt), unit, whole(r.gpu_util, 9999),
        gpu_w % 100, 0, gpu_mhz / 100, gpu_mhz % 100,
        fan / 100, fan % 100,
        0, 0, // a second fan
        year / 100, year % 100, month, day, hour, minute, second, weekday,
        whole(r.ram_util, 9999), cpu_w / 100, gpu_w / 100,
        mode, s.cycle as i64, s.show_gpu as i64, s.items as i64, mode, mode,
        0, 0, unit, 0, 0, // RAM temperature and clock: not measured
        mode,
        hundredths(r.cpu_util), hundredths(r.gpu_util), hundredths(r.ram_util),
    ];
    let mut out = [0u8; 64];
    for (o, x) in out.iter_mut().zip(v) {
        *o = x as u8; // (Buffer.from keeps the low byte, too)
    }
    out
}

pub fn packet(r: &Reading, s: &Settings, usage_page: u16) -> [u8; 64] {
    if usage_page == NEW_PAGE { short(r, s) } else { classic(r, s) }
}

/// The local time as the display wants it.
pub fn local_time() -> [u16; 7] {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    [
        now.year().clamp(0, 9999) as u16,
        now.month() as u16,
        now.day() as u16,
        now.hour() as u16,
        now.minute() as u16,
        now.second() as u16,
        now.weekday().num_days_from_sunday() as u16,
    ]
}

/// JONSBO PC Monitor itself (it would write the same display, and the two
/// would flicker).
pub fn is_jonsbo_app(name: &str) -> bool {
    let name = name.to_lowercase();
    name.starts_with("jonsbo pc monitor") && name.ends_with(".exe")
}

#[derive(Default)]
struct Shared {
    settings: Settings,
    sample: Option<Payload>,
    note: String,
    on: bool,
    stop: bool,
}

pub struct DisplayFeed {
    shared: Arc<Mutex<Shared>>,
}

impl DisplayFeed {
    pub fn start(settings: Settings) -> DisplayFeed {
        let shared = Arc::new(Mutex::new(Shared { settings, note: "looking for it".into(), ..Shared::default() }));
        let inner = shared.clone();
        let _ = std::thread::Builder::new().name("display".into()).spawn(move || run(inner));
        DisplayFeed { shared }
    }

    pub fn configure(&self, settings: &Settings) {
        self.shared.lock().unwrap().settings = settings.clone();
    }

    pub fn sample(&self, payload: &Payload) {
        self.shared.lock().unwrap().sample = Some(payload.clone());
    }

    /// (showing now, what it's doing or why not)
    pub fn status(&self) -> (bool, String) {
        let s = self.shared.lock().unwrap();
        (s.on, s.note.clone())
    }

    pub fn stop(&self) {
        self.shared.lock().unwrap().stop = true;
    }
}

fn run(shared: Arc<Mutex<Shared>>) {
    let mut port = hid::Port::default();
    let mut said = String::new();
    let mut system = sysinfo::System::new();
    let mut checked_at: Option<Instant> = None;
    let mut rival = false;
    loop {
        let started = Instant::now();
        let (settings, sample, stop) = {
            let s = shared.lock().unwrap();
            (s.settings.clone(), s.sample.clone(), s.stop)
        };
        if stop {
            port.close();
            break;
        }
        let note = |on: bool, text: &str, said: &mut String| {
            if said != text {
                crate::paths::log(&format!("digital display: {text}"));
                *said = text.to_string();
            }
            let mut s = shared.lock().unwrap();
            s.on = on;
            s.note = text.to_string();
        };
        if !settings.enabled {
            port.close();
            note(false, "off", &mut said);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        if checked_at.is_none_or(|at| at.elapsed() >= RESCAN) {
            checked_at = Some(Instant::now());
            use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
            rival = system.processes().values().any(|p| is_jonsbo_app(&p.name().to_string_lossy()));
            if !port.is_open() {
                port.open(VID, PID);
            }
        }
        if rival {
            port.close();
            note(false, "paused: JONSBO PC Monitor is running and would fight Moni for the display. Quit it and turn off its start with Windows (or uninstall it).", &mut said);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let Some(page) = port.usage_page() else {
            note(false, &port.note, &mut said);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        };
        if let Some(p) = &sample {
            let data = packet(&Reading::from_sample(p, local_time()), &settings, page);
            match port.write(&data) {
                Ok(()) => note(true, &format!("showing ({}, usage page {page})", if page == NEW_PAGE { "short layout" } else { "classic layout" }), &mut said),
                Err(e) => {
                    port.close();
                    note(false, &format!("lost the display ({e}); looking for it again"), &mut said);
                }
            }
        }
        if let Some(rest) = SEND_EVERY.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

#[cfg(windows)]
mod hid {
    use hidapi::{HidApi, HidDevice};

    #[derive(Default)]
    pub struct Port {
        device: Option<(HidDevice, u16)>,
        failures: u32,
        pub note: String,
    }

    impl Port {
        pub fn is_open(&self) -> bool {
            self.device.is_some()
        }

        pub fn usage_page(&self) -> Option<u16> {
            self.device.as_ref().map(|(_, page)| *page)
        }

        /// The first collection of the device, as JONSBO PC Monitor picks it.
        pub fn open(&mut self, vid: u16, pid: u16) {
            let api = match HidApi::new() {
                Ok(api) => api,
                Err(e) => {
                    self.note = format!("can't list USB devices ({e})");
                    return;
                }
            };
            let Some(info) = api.device_list().find(|d| d.vendor_id() == vid && d.product_id() == pid) else {
                self.note = format!("not connected (no USB device {vid:04X}:{pid:04X})");
                return;
            };
            match info.open_device(&api) {
                Ok(device) => {
                    crate::paths::log(&format!("digital display: opened {vid:04X}:{pid:04X}, usage page {}", info.usage_page()));
                    self.device = Some((device, info.usage_page()));
                    self.failures = 0;
                }
                Err(e) => self.note = format!("found it but can't open it ({e})"),
            }
        }

        /// Like the app: a few failed writes in a row are let go, more close it.
        pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
            let Some((device, _)) = &self.device else { return Err("not open".into()) };
            match device.write(data) {
                Ok(_) => {
                    self.failures = 0;
                    Ok(())
                }
                Err(e) if self.failures < 10 => {
                    self.failures += 1;
                    let _ = e;
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }

        pub fn close(&mut self) {
            self.device = None;
        }
    }
}

#[cfg(not(windows))]
mod hid {
    /// Only Windows has the display; elsewhere it's never found.
    #[derive(Default)]
    pub struct Port {
        pub note: String,
    }

    impl Port {
        pub fn is_open(&self) -> bool {
            false
        }
        pub fn usage_page(&self) -> Option<u16> {
            None
        }
        pub fn open(&mut self, _vid: u16, _pid: u16) {
            self.note = "not connected (Windows only)".into();
        }
        pub fn write(&mut self, _data: &[u8]) -> Result<(), String> {
            Err("not open".into())
        }
        pub fn close(&mut self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading() -> Reading {
        Reading {
            cpu_temp: 58.37,
            cpu_util: 17.25,
            cpu_mhz: 4312.0,
            cpu_watts: 147.62,
            cpu_fan: 1819.0,
            gpu_temp: 46.0,
            gpu_util: 8.5,
            gpu_mhz: 2467.0,
            gpu_watts: 66.94,
            ram_util: 46.1,
            ram_gb: 31.1,
            disk_util: 62.0,
            disk_gb: 931.5,
            time: [2026, 10, 29, 21, 5, 9, 4],
        }
    }

    #[test]
    fn classic_layout_matches_the_app() {
        let p = classic(&reading(), &Settings::default());
        assert_eq!(&p[..4], &[1, 0, 1, 2]);
        // (58.37 - 58) * 100 is 36.99… in floating point, floored to 36: the
        // app's JavaScript sends the same)
        assert_eq!(&p[4..8], &[58, 36, 0, 17], "CPU 58.37 °C, °C, 17 %");
        assert_eq!(&p[8..12], &[47, 62, 43, 12], "147.62 W -> 47 | .62; 4312 MHz -> 43 | 12");
        assert_eq!(&p[14..18], &[46, 0, 0, 8]);
        assert_eq!(&p[18..22], &[66, 0, 24, 67], "GPU W hundredths are 0 in the app too");
        assert_eq!(&p[22..24], &[18, 19], "1819 rpm");
        assert_eq!(&p[26..34], &[20, 26, 10, 29, 21, 5, 9, 4], "2026-10-29 21:05:09, Thursday");
        assert_eq!(&p[34..37], &[46, 1, 0], "RAM 46 %, CPU W hundreds 1, GPU 0");
        assert_eq!(&p[37..43], &[0, 5, 0, 1, 0, 0], "mode, cycle 5 s, CPU, items 1");
        let s = Settings { mode: 2, cycle: 8, show_gpu: true, items: 1 | 4, ..Settings::default() };
        let p = classic(&reading(), &s);
        assert_eq!((p[37], p[38], p[39], p[40], p[41], p[42], p[48]), (2, 8, 1, 5, 2, 2, 2));
        assert_eq!(&p[49..52], &[25, 50, 10], "usage hundredths");
        assert!(p[52..].iter().all(|&b| b == 0));
    }

    #[test]
    fn short_layout_matches_the_app() {
        let p = short(&reading(), &Settings::default());
        assert_eq!(&p[..4], &[0x00, 0x40, 58, 17]);
        assert_eq!(&p[4..10], &[0x10, 0xd8, 0x07, 0x1b, 0x00, 147], "4312 MHz, 1819 rpm, 147 W");
        assert_eq!(&p[10..18], &[46, 8, 0x09, 0xa3, 0, 0, 0, 66], "2467 MHz = 0x09A3");
        // 31.1 GB -> 31846 MB, down to a multiple of 16: 31840
        assert_eq!(&p[18..24], &[46, 0x7c, 0x60, 62, 0x03, 0xa3]);
        assert!(p[24..].iter().all(|&b| b == 0));
        assert_eq!(packet(&reading(), &Settings::default(), NEW_PAGE), p);
    }

    #[test]
    fn fahrenheit_as_the_app_sends_it() {
        let s = Settings { fahrenheit: true, ..Settings::default() };
        let c = classic(&reading(), &s);
        assert_eq!(&c[4..7], &[137, 6, 1], "58.37 °C = 137.07 °F (hundredths floored), unit 1");
        let n = short(&reading(), &s);
        assert_eq!(n[2], 58 + 128, "page 12: °C + 128 marks °F");
    }

    #[test]
    fn readings_from_a_moni_sample() {
        let p = Payload { ct: 58.0, cpu: 17.0, pf: 4312, cpuw: 47.6, fan: 1819, gt: 46.0, gpu: 8.0, gf: 2467, gpuw: 66.9, ram: 46.1, ramgb: 31.1, disk: 75.0, free: 250.0, ..Payload::default() };
        let r = Reading::from_sample(&p, [2026, 1, 2, 3, 4, 5, 6]);
        assert_eq!(r.disk_gb, 1000.0, "250 GB free of 75 % used");
        assert_eq!((r.cpu_mhz, r.cpu_fan, r.gpu_mhz), (4312.0, 1819.0, 2467.0));
    }

    #[test]
    fn knows_the_jonsbo_app() {
        assert!(is_jonsbo_app("JONSBO PC Monitor.exe"));
        assert!(!is_jonsbo_app("Moni.exe"));
    }
}
