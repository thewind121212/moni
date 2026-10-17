//! Builds the one-line JSON the firmware parses (protocol v1). Field names and
//! units match host/monitor_windows.py exactly; the firmware is unchanged.

use crate::gpu::NvidiaGpu;
use crate::hardware::{self, Row, SensorFeed};
use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

pub const PROTOCOL_VERSION: u8 = 1;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Payload {
    pub v: u8,
    pub t: u64,
    pub os: String,
    pub cores: usize,
    pub threads: usize,
    pub ramgb: f64,
    pub cpu: f64,
    pub pc: f64,
    pub ec: f64,
    pub ram: f64,
    pub swap: f64,
    pub ct: f64,
    pub mt: f64,
    pub st: f64,
    pub pf: u64,
    pub ef: u64,
    pub fan: u64,
    pub cpuw: f64,
    pub sysw: f64,
    pub pwrmode: String,
    pub disk: f64,
    pub free: f64,
    pub load: f64,
    pub up: u64,
    pub gpu: f64,
    pub gt: f64,
    pub gf: u32,
    pub gpuw: f64,
    pub vram: f64,
    pub vramgb: f64,
    pub nr: f64,
    pub nt: f64,
    pub dr: f64,
    pub dw: f64,
}

#[derive(Serialize)]
pub struct Hello {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub host: String,
    pub chip: String,
    pub gpu_name: String,
    pub cores: usize,
    pub ram_gb: u64,
}

fn round(value: f64, digits: i32) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let scale = 10f64.powi(digits);
    // `+ 0.0` turns -0.0 (an empty f64 sum) into 0.0.
    (value * scale).round() / scale + 0.0
}

fn percent(value: f64) -> f64 {
    round(value.clamp(0.0, 100.0), 1)
}

pub struct Sampler {
    system: System,
    networks: Networks,
    disks: Disks,
    pub gpu: NvidiaGpu,
    pub sensors: Option<SensorFeed>,
    previous: Option<(Instant, u64, u64)>,
    physical_cores: usize,
    system_drive: String,
}

impl Sampler {
    /// `sensors` is None when running without the LibreHardwareMonitor helper.
    pub fn new(sensors: Option<SensorFeed>) -> Sampler {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage().with_frequency())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        Sampler {
            physical_cores: System::physical_core_count().unwrap_or(0),
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            gpu: NvidiaGpu::new(),
            sensors,
            previous: None,
            system_drive: format!("{system_drive}\\"),
        }
    }

    pub fn sensor_rows(&self) -> Vec<Row> {
        self.sensors.as_ref().map(|feed| feed.rows()).unwrap_or_default()
    }

    pub fn sample(&mut self, fan_selector: &str) -> Payload {
        self.system.refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage().with_frequency());
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);

        let rows = self.sensor_rows();
        let picked = hardware::pick(&rows, fan_selector);
        let gpu = self.gpu.sample();

        // Network: cumulative counters diffed against the previous sample.
        let (rx, tx) = self
            .networks
            .iter()
            .fold((0u64, 0u64), |(rx, tx), (_, data)| (rx + data.total_received(), tx + data.total_transmitted()));
        let now = Instant::now();
        let (nr, nt) = match self.previous {
            Some((at, prx, ptx)) => {
                let elapsed = now.duration_since(at).as_secs_f64().max(0.001);
                (rx.saturating_sub(prx) as f64 / elapsed / MIB, tx.saturating_sub(ptx) as f64 / elapsed / MIB)
            }
            None => (0.0, 0.0),
        };
        self.previous = Some((now, rx, tx));

        let threads = self.system.cpus().len();
        let cpu = percent(self.system.global_cpu_usage() as f64);
        let base_clock = self.system.cpus().first().map(|c| c.frequency()).unwrap_or(0);

        let total = self.system.total_memory() as f64;
        let ram = if total > 0.0 {
            (total - self.system.available_memory() as f64) / total * 100.0
        } else {
            0.0
        };
        let swap_total = self.system.total_swap() as f64;
        let swap = if swap_total > 0.0 { self.system.used_swap() as f64 / swap_total * 100.0 } else { 0.0 };

        let drive = self
            .disks
            .iter()
            .find(|d| d.mount_point().to_string_lossy().eq_ignore_ascii_case(&self.system_drive))
            .or_else(|| self.disks.iter().next());
        let (disk, free) = drive
            .map(|d| {
                let size = d.total_space() as f64;
                let available = d.available_space() as f64;
                let used = if size > 0.0 { (size - available) / size * 100.0 } else { 0.0 };
                (used, available / GIB)
            })
            .unwrap_or((0.0, 0.0));

        let cpu_w = picked.cpu_w;
        let measured_total = picked.total_w;
        let component = if cpu_w > 0.0 && gpu.watts > 0.0 { cpu_w + gpu.watts } else { 0.0 };
        let (sysw, pwrmode) = if measured_total > 0.0 {
            (measured_total, "total")
        } else if component > 0.0 {
            (component, "parts")
        } else {
            (0.0, "none")
        };

        Payload {
            v: PROTOCOL_VERSION,
            t: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            os: "win".into(),
            cores: self.physical_cores,
            threads,
            ramgb: round(total / GIB, 1),
            cpu,
            pc: cpu,
            ec: 0.0,
            ram: percent(ram),
            swap: percent(swap),
            ct: round(picked.cpu_temp, 1),
            mt: round(picked.mobo_temp, 1),
            st: round(picked.ssd_temp, 1),
            pf: if picked.cpu_clock > 0.0 { picked.cpu_clock as u64 } else { base_clock },
            ef: 0,
            fan: picked.fan.max(0.0) as u64,
            cpuw: round(cpu_w, 2),
            sysw: round(sysw, 2),
            pwrmode: pwrmode.into(),
            disk: round(disk, 1),
            free: round(free, 1),
            load: round(cpu * threads.max(1) as f64 / 100.0, 2),
            up: System::uptime(),
            gpu: percent(gpu.load),
            gt: round(gpu.temp, 1),
            gf: gpu.clock,
            gpuw: round(gpu.watts, 2),
            vram: round(gpu.vram_gb, 1),
            vramgb: round(gpu.vram_total_gb, 1),
            nr: round(nr, 2),
            nt: round(nt, 2),
            dr: round(picked.disk_read / MIB, 2),
            dw: round(picked.disk_write / MIB, 2),
        }
    }

    pub fn hello(&self) -> Hello {
        Hello {
            v: PROTOCOL_VERSION,
            kind: "hello",
            host: System::host_name().unwrap_or_default(),
            chip: self
                .system
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "AMD Ryzen".into()),
            gpu_name: self.gpu.name.clone(),
            cores: self.system.cpus().len(),
            ram_gb: (self.system.total_memory() as f64 / GIB).round() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_keeps_firmware_field_names() {
        let line = serde_json::to_string(&Payload { v: 1, os: "win".into(), ..Default::default() }).unwrap();
        for key in ["\"v\":1", "\"os\":\"win\"", "\"pwrmode\"", "\"vramgb\"", "\"dw\""] {
            assert!(line.contains(key), "{key} missing from {line}");
        }
        assert!(line.len() < 700, "line must fit the firmware's 768-byte buffer");
    }

    #[test]
    fn sample_runs_without_helper() {
        let mut sampler = Sampler::new(None);
        let payload = sampler.sample("");
        assert!(payload.threads > 0);
        assert!(serde_json::to_string(&sampler.hello()).unwrap().contains("\"type\":\"hello\""));
    }
}
