//! User settings, shared by the service (reader) and the app (writer).

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// USB when the clock is plugged in, plus WiFi. The clock only listens on
    /// whichever link its mode button selected, so feeding both is harmless.
    #[default]
    Auto,
    Usb,
    Wifi,
}

impl Transport {
    pub fn uses_usb(self) -> bool {
        matches!(self, Transport::Auto | Transport::Usb)
    }
    pub fn uses_wifi(self) -> bool {
        matches!(self, Transport::Auto | Transport::Wifi)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Master switch: false keeps the service running but sends nothing.
    pub enabled: bool,
    pub transport: Transport,
    /// COM port such as "COM5"; empty auto-detects the CH340.
    pub serial_port: String,
    /// Clock address "IP" or "IP:port"; empty broadcasts on every LAN.
    pub wifi_target: String,
    /// Substring of the CPU-fan sensor name or identifier; empty auto-picks.
    pub fan_sensor: String,
    /// Motherboard / SSD temperature sensor identifiers; empty auto-picks.
    pub mobo_sensor: String,
    pub ssd_sensor: String,
    pub interval_ms: u64,
    pub baud: u32,
    /// Shared with moni-relay (MONI_PC_KEY) so the home page can turn this PC
    /// off; empty = remote shutdown off. See [`crate::control`].
    pub control_key: String,
    /// The fans' ARGB lights through OpenRGB. See [`crate::lights`].
    pub lights: Lights,
    /// The digital display (USB HID 5131:2007). See [`crate::display`].
    pub display: Display,
    /// Fan speeds by curve. See [`crate::fans`].
    pub fans: Fans,
}

/// What a fan group follows.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum FanRole {
    /// The CPU curve (an AIO radiator, a CPU cooler).
    #[default]
    Cpu,
    /// Air in: the higher of the GPU curve and the CPU fans' speed, plus the
    /// intake offset, so more air always comes in than goes out (dust only
    /// gets in through the filters).
    Intake,
    /// Air out: the higher of the CPU and GPU curves.
    Exhaust,
    /// An AIO pump: always 100 %.
    Pump,
    /// Left to the BIOS.
    Bios,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FanGroup {
    pub name: String,
    pub role: FanRole,
    /// The LibreHardwareMonitor control sensor id of its header, e.g.
    /// "/lpc/it8689e/0/control/0"; empty = not assigned yet.
    pub control: String,
    /// Never slower than this, %.
    pub min: u8,
}

impl Default for FanGroup {
    fn default() -> Self {
        Self { name: String::new(), role: FanRole::Cpu, control: String::new(), min: 30 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Fans {
    /// Off until switched on (the BIOS curves run the fans until then).
    pub enabled: bool,
    pub groups: Vec<FanGroup>,
    /// [°C, %] points, linear between them, flat past the ends.
    pub cpu_curve: Vec<[f64; 2]>,
    pub gpu_curve: Vec<[f64; 2]>,
    /// Intakes run this much faster than the fans they follow, %.
    pub intake_offset: u8,
    /// Speeds only drop once the temperature has fallen this far, °C.
    pub hysteresis: f64,
}

impl Default for Fans {
    /// The owner's PC (ASUS AP202, B650EM FORCE): a 360 radiator on top
    /// blowing out, 3 bottom and 2 side fans in, 1 rear fan out, an AIO pump.
    fn default() -> Self {
        let group = |name: &str, role: FanRole, min: u8| FanGroup { name: name.into(), role, control: String::new(), min };
        Self {
            enabled: false,
            groups: vec![
                group("Radiator (top, out)", FanRole::Cpu, 30),
                group("Bottom (in)", FanRole::Intake, 30),
                group("Side (in)", FanRole::Intake, 30),
                group("Rear (out)", FanRole::Exhaust, 30),
                group("Pump", FanRole::Pump, 100),
            ],
            cpu_curve: vec![[40.0, 30.0], [60.0, 40.0], [75.0, 60.0], [85.0, 80.0]],
            gpu_curve: vec![[45.0, 30.0], [65.0, 50.0], [75.0, 65.0], [82.0, 80.0]],
            intake_offset: 10,
            hysteresis: 3.0,
        }
    }
}

/// 2-8 points, by temperature, within 0-110 °C and 0-100 %; else the default.
fn clean_curve(points: &[[f64; 2]], default: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = points
        .iter()
        .filter(|[t, p]| t.is_finite() && p.is_finite())
        .map(|[t, p]| [t.clamp(0.0, 110.0), p.clamp(0.0, 100.0)])
        .take(8)
        .collect();
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    out.dedup_by(|a, b| a[0] == b[0]);
    if out.len() < 2 { default.to_vec() } else { out }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Display {
    /// On by default: it does nothing until the display is plugged in.
    pub enabled: bool,
    pub fahrenheit: bool,
    /// Experimental, sent as the maker's app sends them (it never changes
    /// them; boards differ in what they do): display mode (0 temperature,
    /// 1 usage, 2 cycle, 3 cycle with RAM on some boards), CPU or GPU,
    /// seconds per page when cycling, and which readings to show (1 CPU
    /// temperature, 2 CPU usage, 4 GPU temperature, 8 GPU usage, added up).
    pub mode: u8,
    pub show_gpu: bool,
    pub cycle: u8,
    pub items: u8,
}

impl Default for Display {
    fn default() -> Self {
        Self { enabled: true, fahrenheit: false, mode: 0, show_gpu: false, cycle: 5, items: 1 }
    }
}

/// What the fans show. See [`crate::lights::Effect`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LightMode {
    /// Comets in a combo's colors, faster and hotter with CPU load.
    #[default]
    Load,
    /// One color, still.
    Static,
    /// A running rainbow (or the user's own colors) along each zone.
    Rainbow,
}

pub const RAINBOW_COLORS_MAX: usize = 8;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Lights {
    /// Off until switched on, so it never fights another RGB program.
    pub enabled: bool,
    pub mode: LightMode,
    /// Load mode: 1-12, see [`crate::lights::COMBOS`]; 12 = Blue & Purple.
    pub combo: u8,
    /// Load mode: the comets follow CPU load (speed and color). Off = Fixed:
    /// they run at Speed (100 % = one comet a second) in the color at
    /// `comet_level`, whatever the CPU does.
    pub comet_load: bool,
    /// Fixed comets: where their head color sits on the combo, 0 (its idle
    /// color) to 100 (its full-load color).
    pub comet_level: u8,
    /// Static mode: "#rrggbb".
    pub color: String,
    /// Rainbow mode: the colors it runs through ("#rrggbb", up to 8, looping
    /// back to the first); empty = the full spectrum.
    pub rainbow: Vec<String>,
    /// Rainbow mode: LEDs one whole rainbow spans, 6-240.
    pub rainbow_length: u32,
    /// Rainbow mode: runs the other way along the zones.
    pub reverse: bool,
    /// Rainbow mode: runs faster with CPU load.
    pub rainbow_load: bool,
    /// 0-100 %.
    pub brightness: u8,
    /// How fast the comets that follow CPU load run, 25-200 %.
    pub speed: u8,
    /// Each effect keeps its own Speed, so one never changes another's:
    /// Fixed comets (100 % = one comet a second) and the rainbow. 0 = not
    /// set yet (a settings file from before): the same as `speed`.
    pub comet_speed: u8,
    pub rainbow_speed: u8,
    /// Rainbow mode: the user's own colors (`rainbow`) rather than the full
    /// spectrum. `rainbow` is kept either way, so switching back finds them.
    /// Unset (a file from before) = own colors when there are any.
    pub rainbow_own: Option<bool>,
    /// Part of the OpenRGB device name; empty = the motherboard.
    pub device: String,
    /// Comma-separated zone names to light; empty = every zone.
    pub zones: String,
    /// Addressable zones are resized to this many LEDs (OpenRGB guesses 10
    /// for an ARGB header; Jonsbo ZA fans have 70+ on one cable). 0 = leave
    /// OpenRGB's sizes alone.
    pub leds_per_zone: u32,
}

impl Lights {
    /// The Speed of the effect showing now (each effect has its own), 25-200 %.
    pub fn active_speed(&self) -> u8 {
        let own = match self.mode {
            LightMode::Rainbow => self.rainbow_speed,
            LightMode::Load if !self.comet_load => self.comet_speed,
            _ => self.speed,
        };
        if own == 0 { self.speed } else { own }
    }

    /// The rainbow runs through the user's own colors (else the spectrum).
    pub fn own_rainbow(&self) -> bool {
        self.rainbow_own.unwrap_or(!self.rainbow.is_empty()) && !self.rainbow.is_empty()
    }
}

impl Default for Lights {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: LightMode::Load,
            combo: 12,
            comet_load: true,
            comet_level: 50,
            color: "#0046ff".into(),
            rainbow: Vec::new(),
            rainbow_length: 60,
            reverse: false,
            rainbow_load: false,
            brightness: 100,
            speed: 100,
            comet_speed: 0,
            rainbow_speed: 0,
            rainbow_own: None,
            device: String::new(),
            zones: String::new(),
            leds_per_zone: 120,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: Transport::Auto,
            serial_port: String::new(),
            wifi_target: String::new(),
            fan_sensor: String::new(),
            mobo_sensor: String::new(),
            ssd_sensor: String::new(),
            interval_ms: 1000,
            baud: 115_200,
            control_key: String::new(),
            lights: Lights::default(),
            display: Display::default(),
            fans: Fans::default(),
        }
    }
}

impl Config {
    /// Missing or unreadable files fall back to defaults so the clock keeps
    /// working even if someone hand-edits the JSON badly.
    pub fn load(path: &Path) -> Config {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Config>(&text).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&self.clone().sanitized())?;
        crate::paths::write_atomic(path, text.as_bytes())
    }

    pub fn sanitized(mut self) -> Config {
        self.interval_ms = self.interval_ms.clamp(250, 10_000);
        if self.baud == 0 {
            self.baud = 115_200;
        }
        self.serial_port = self.serial_port.trim().to_string();
        self.wifi_target = self.wifi_target.trim().to_string();
        self.fan_sensor = self.fan_sensor.trim().to_string();
        self.mobo_sensor = self.mobo_sensor.trim().to_string();
        self.ssd_sensor = self.ssd_sensor.trim().to_string();
        self.control_key = self.control_key.trim().to_string();
        let lights = &mut self.lights;
        lights.combo = lights.combo.clamp(1, crate::lights::COMBOS.len() as u8);
        lights.color = crate::lights::parse_hex(&lights.color).map(crate::lights::to_hex).unwrap_or_else(|| Lights::default().color);
        lights.rainbow = lights.rainbow.iter().filter_map(|c| crate::lights::parse_hex(c)).map(crate::lights::to_hex).take(RAINBOW_COLORS_MAX).collect();
        lights.rainbow_length = lights.rainbow_length.clamp(6, 240);
        lights.brightness = lights.brightness.min(100);
        lights.comet_level = lights.comet_level.min(100);
        lights.speed = lights.speed.clamp(25, 200);
        for speed in [&mut lights.comet_speed, &mut lights.rainbow_speed] {
            *speed = if *speed == 0 { lights.speed } else { (*speed).clamp(25, 200) };
        }
        lights.rainbow_own = Some(lights.own_rainbow());
        lights.device = lights.device.trim().to_string();
        lights.zones = lights.zones.trim().to_string();
        lights.leds_per_zone = lights.leds_per_zone.min(1024);
        let fans = &mut self.fans;
        let defaults = Fans::default();
        fans.cpu_curve = clean_curve(&fans.cpu_curve, &defaults.cpu_curve);
        fans.gpu_curve = clean_curve(&fans.gpu_curve, &defaults.gpu_curve);
        fans.intake_offset = fans.intake_offset.min(50);
        fans.hysteresis = if fans.hysteresis.is_finite() { fans.hysteresis.clamp(0.0, 10.0) } else { defaults.hysteresis };
        fans.groups.truncate(8);
        for group in &mut fans.groups {
            group.name = group.name.trim().chars().take(40).collect();
            group.control = group.control.trim().to_string();
            group.min = if group.role == FanRole::Pump { 100 } else { group.min.clamp(20, 100) };
        }
        let display = &mut self.display;
        display.mode = display.mode.min(3);
        display.cycle = display.cycle.clamp(1, 60);
        display.items = if display.items == 0 { 1 } else { display.items.min(15) };
        self
    }

    /// For logs: the key never goes into service.log.
    pub fn redacted(&self) -> Config {
        let mut copy = self.clone();
        if !copy.control_key.is_empty() {
            copy.control_key = "<set>".into();
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_keeps_defaults() {
        let config: Config = serde_json::from_str(r#"{"transport":"wifi"}"#).unwrap();
        assert_eq!(config.transport, Transport::Wifi);
        assert!(config.enabled);
        assert_eq!(config.interval_ms, 1000);
        assert_eq!(config.lights, Lights::default());
        assert!(config.lights.comet_load, "comets follow the CPU unless set to Fixed");
    }

    #[test]
    fn each_effect_keeps_its_own_settings() {
        // A file from before: one Speed for all, own rainbow colors in use.
        let old: Config = serde_json::from_str(r##"{"lights":{"speed":60,"rainbow":["#ff0000","#00ff00"]}}"##).unwrap();
        let l = old.sanitized().lights;
        assert_eq!((l.speed, l.comet_speed, l.rainbow_speed), (60, 60, 60));
        assert_eq!(l.rainbow_own, Some(true));
        // From then on each Speed is its own.
        let mut l = Lights { speed: 50, comet_speed: 120, rainbow_speed: 180, ..Lights::default() };
        assert_eq!(l.active_speed(), 50);
        l.comet_load = false;
        assert_eq!(l.active_speed(), 120);
        l.mode = LightMode::Rainbow;
        assert_eq!(l.active_speed(), 180);
        // "Full rainbow" picked: the own colors stay saved for later.
        let l = Lights { rainbow: vec!["#ff0000".into(), "#0000ff".into()], rainbow_own: Some(false), ..Lights::default() };
        assert!(!l.own_rainbow());
        let l = Config { lights: l, ..Config::default() }.sanitized().lights;
        assert_eq!(l.rainbow.len(), 2, "kept");
        assert_eq!(l.rainbow_own, Some(false));
    }

    #[test]
    fn lights_settings_are_kept_in_range() {
        let config: Config = serde_json::from_str(r#"{"lights":{"enabled":true,"combo":40,"brightness":250,"speed":3,"zones":" ARGB_V2_1 "}}"#).unwrap();
        let lights = config.sanitized().lights;
        assert!(lights.enabled);
        assert_eq!((lights.combo, lights.brightness, lights.speed, lights.leds_per_zone), (12, 100, 25, 120));
        assert_eq!(lights.zones, "ARGB_V2_1");
        assert_eq!(lights.mode, LightMode::Load, "settings from before the modes keep the load effect");
    }

    #[test]
    fn colors_are_cleaned_up() {
        let config: Config = serde_json::from_str(
            r##"{"lights":{"mode":"rainbow","color":"red","rainbow":["#FF0000"," 00ff00 ","nope","#123","#0000ff"],"rainbow_length":2}}"##,
        )
        .unwrap();
        let lights = config.sanitized().lights;
        assert_eq!(lights.mode, LightMode::Rainbow);
        assert_eq!(lights.color, "#0046ff", "a bad color goes back to the default");
        assert_eq!(lights.rainbow, ["#ff0000", "#00ff00", "#0000ff"]);
        assert_eq!(lights.rainbow_length, 6);
        let many: Config = serde_json::from_str(&format!(r#"{{"lights":{{"rainbow":{:?}}}}}"#, vec!["#ffffff"; 12])).unwrap();
        assert_eq!(many.sanitized().lights.rainbow.len(), RAINBOW_COLORS_MAX);
    }
}
