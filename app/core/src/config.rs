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
    pub interval_ms: u64,
    pub baud: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: Transport::Auto,
            serial_port: String::new(),
            wifi_target: String::new(),
            fan_sensor: String::new(),
            interval_ms: 1000,
            baud: 115_200,
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
        self
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
    }
}
