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
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Lights {
    /// Off until switched on, so it never fights another RGB program.
    pub enabled: bool,
    /// 1-12, see [`crate::lights::COMBOS`]; 12 = Blue & Purple.
    pub combo: u8,
    /// 0-100 %.
    pub brightness: u8,
    /// Part of the OpenRGB device name; empty = the motherboard.
    pub device: String,
    /// Comma-separated zone names to light; empty = every zone.
    pub zones: String,
    /// Addressable zones are resized to this many LEDs (OpenRGB guesses 10
    /// for an ARGB header; Jonsbo ZA fans have 70+ on one cable). 0 = leave
    /// OpenRGB's sizes alone.
    pub leds_per_zone: u32,
}

impl Default for Lights {
    fn default() -> Self {
        Self { enabled: false, combo: 12, brightness: 100, device: String::new(), zones: String::new(), leds_per_zone: 120 }
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
        lights.brightness = lights.brightness.min(100);
        lights.device = lights.device.trim().to_string();
        lights.zones = lights.zones.trim().to_string();
        lights.leds_per_zone = lights.leds_per_zone.min(1024);
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
    }

    #[test]
    fn lights_settings_are_kept_in_range() {
        let config: Config = serde_json::from_str(r#"{"lights":{"enabled":true,"combo":40,"brightness":250,"zones":" ARGB_V2_1 "}}"#).unwrap();
        let lights = config.sanitized().lights;
        assert!(lights.enabled);
        assert_eq!((lights.combo, lights.brightness, lights.leds_per_zone), (12, 100, 120));
        assert_eq!(lights.zones, "ARGB_V2_1");
    }
}
