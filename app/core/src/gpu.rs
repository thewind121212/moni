//! NVIDIA GPU readings through NVML (nvml.dll ships with the driver).

use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Copy)]
pub struct GpuSample {
    pub load: f64,
    pub temp: f64,
    pub clock: u32,
    pub watts: f64,
    pub vram_gb: f64,
    pub vram_total_gb: f64,
}

pub struct NvidiaGpu {
    nvml: Option<Nvml>,
    pub name: String,
    pub error: Option<String>,
    last_attempt: Option<Instant>,
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

impl NvidiaGpu {
    pub fn new() -> NvidiaGpu {
        let mut gpu = NvidiaGpu { nvml: None, name: "NVIDIA GPU".into(), error: None, last_attempt: None };
        gpu.connect();
        gpu
    }

    /// At boot the service can start before the display driver; retry every 30 s.
    fn connect(&mut self) {
        if self.nvml.is_some() {
            return;
        }
        if self.last_attempt.map(|at| at.elapsed() < Duration::from_secs(30)).unwrap_or(false) {
            return;
        }
        self.last_attempt = Some(Instant::now());
        match Nvml::init() {
            Ok(nvml) => match nvml.device_count() {
                Ok(0) => self.error = Some("no NVIDIA GPU found".into()),
                Ok(_) => {
                    if let Ok(name) = nvml.device_by_index(0).and_then(|d| d.name()) {
                        self.name = name;
                    }
                    self.error = None;
                    self.nvml = Some(nvml);
                }
                Err(error) => self.error = Some(error.to_string()),
            },
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn sample(&mut self) -> GpuSample {
        self.connect();
        let Some(nvml) = &self.nvml else { return GpuSample::default() };
        let device = match nvml.device_by_index(0) {
            Ok(device) => device,
            Err(error) => {
                self.error = Some(error.to_string());
                self.nvml = None;
                return GpuSample::default();
            }
        };
        let memory = device.memory_info().ok();
        GpuSample {
            load: device.utilization_rates().map(|u| u.gpu as f64).unwrap_or(0.0),
            temp: device.temperature(TemperatureSensor::Gpu).map(|t| t as f64).unwrap_or(0.0),
            clock: device.clock_info(Clock::Graphics).unwrap_or(0),
            watts: device.power_usage().map(|mw| mw as f64 / 1000.0).unwrap_or(0.0),
            vram_gb: memory.as_ref().map(|m| m.used as f64 / GIB).unwrap_or(0.0),
            vram_total_gb: memory.as_ref().map(|m| m.total as f64 / GIB).unwrap_or(0.0),
        }
    }
}

impl Default for NvidiaGpu {
    fn default() -> Self {
        Self::new()
    }
}
