//! LibreHardwareMonitor sensor feed. The C# helper (moni-sensors.exe) prints
//! every sensor once a second; this module keeps the latest set and picks the
//! values the clock shows, using the same scoring as host/monitor_windows.py.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    /// Identifier of the hardware that owns the sensor, e.g. "/amdcpu/0".
    pub parent: String,
    /// Human-readable hardware name, e.g. "AMD Ryzen 7 7800X3D".
    #[serde(default)]
    pub hw: String,
    pub value: f64,
}

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    rows: Option<Vec<Row>>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Default)]
struct Latest {
    rows: Vec<Row>,
    at: Option<Instant>,
    error: Option<String>,
}

/// Owns the helper process and restarts it whenever it exits.
pub struct SensorFeed {
    latest: Arc<Mutex<Latest>>,
}

impl SensorFeed {
    pub fn start(exe: PathBuf, interval_ms: u64) -> SensorFeed {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let shared = latest.clone();
        thread::Builder::new()
            .name("moni-sensors".into())
            .spawn(move || supervise(exe, interval_ms, shared))
            .expect("spawn sensor thread");
        SensorFeed { latest }
    }

    /// Rows from the last 5 s; older data is treated as no data.
    pub fn rows(&self) -> Vec<Row> {
        let latest = self.latest.lock().unwrap();
        match latest.at {
            Some(at) if at.elapsed() < Duration::from_secs(5) => latest.rows.clone(),
            _ => Vec::new(),
        }
    }

    pub fn error(&self) -> Option<String> {
        let latest = self.latest.lock().unwrap();
        if latest.at.map(|at| at.elapsed() < Duration::from_secs(5)).unwrap_or(false) {
            return None;
        }
        Some(latest.error.clone().unwrap_or_else(|| "waiting for LibreHardwareMonitor".into()))
    }
}

/// Default location: `sensors\moni-sensors.exe` next to the running binary.
pub fn default_helper_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MONI_SENSORS_EXE") {
        return PathBuf::from(path);
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    dir.join("sensors").join("moni-sensors.exe")
}

fn spawn(exe: &PathBuf, interval_ms: u64) -> std::io::Result<Child> {
    let mut command = Command::new(exe);
    command
        .arg("--interval")
        .arg(interval_ms.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

fn supervise(exe: PathBuf, interval_ms: u64, latest: Arc<Mutex<Latest>>) {
    loop {
        match spawn(&exe, interval_ms) {
            Ok(mut child) => {
                // stdin stays open (held in `child`) so the helper keeps running.
                if let Some(stdout) = child.stdout.take() {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let Ok(parsed) = serde_json::from_str::<Line>(&line) else { continue };
                        let mut slot = latest.lock().unwrap();
                        if let Some(rows) = parsed.rows {
                            slot.rows = rows;
                            slot.at = Some(Instant::now());
                            slot.error = None;
                        } else if let Some(error) = parsed.error {
                            slot.error = Some(error);
                        }
                    }
                }
                let _ = child.kill();
                let status = child.wait();
                latest.lock().unwrap().error =
                    Some(format!("sensor helper exited ({status:?}); restarting"));
            }
            Err(error) => {
                latest.lock().unwrap().error =
                    Some(format!("cannot start {}: {error}", exe.display()));
            }
        }
        thread::sleep(Duration::from_secs(5));
    }
}

/// Values the clock needs from LibreHardwareMonitor.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Picked {
    pub cpu_temp: f64,
    pub mobo_temp: f64,
    pub ssd_temp: f64,
    pub cpu_w: f64,
    pub fan: f64,
    pub cpu_clock: f64,
    pub total_w: f64,
    pub disk_read: f64,
    pub disk_write: f64,
}

fn lower(text: &str) -> String {
    text.to_lowercase()
}

fn is_cpu(row: &Row) -> bool {
    let identity = lower(&format!("{} {}", row.id, row.parent));
    identity.contains("/amdcpu/") || identity.contains("/intelcpu/") || identity.contains("cpu")
}

fn best(rows: &[&Row], score: impl Fn(&Row) -> i32) -> f64 {
    // Highest score wins; ties keep the larger value, like Python's max().
    rows.iter()
        .map(|row| (score(row), row.value))
        .filter(|(s, _)| *s >= 0)
        .fold(None, |acc: Option<(i32, f64)>, cand| match acc {
            Some(a) if (a.0, a.1) >= (cand.0, cand.1) => Some(a),
            _ => Some(cand),
        })
        .map(|(_, v)| v)
        .unwrap_or(0.0)
}

pub fn pick(rows: &[Row], fan_selector: &str) -> Picked {
    let of = |kind: &str| -> Vec<&Row> {
        rows.iter().filter(|r| r.kind.eq_ignore_ascii_case(kind)).collect()
    };
    let temperatures = of("temperature");
    let powers = of("power");
    let fans: Vec<&Row> = of("fan").into_iter().filter(|r| r.value > 0.0).collect();
    let clocks: Vec<&Row> = of("clock").into_iter().filter(|r| r.value > 0.0).collect();
    let throughput = of("throughput");
    let selector = lower(fan_selector);
    let text = |row: &Row| lower(&format!("{} {} {}", row.name, row.id, row.parent));

    let temperature_score = |row: &Row| {
        if !is_cpu(row) {
            return -1;
        }
        let name = lower(&row.name);
        if name.contains("tctl") || name.contains("tdie") {
            100
        } else if name.contains("package") {
            95
        } else if name.contains("core max") {
            90
        } else if name.contains("core average") {
            80
        } else {
            10
        }
    };
    let power_score = |row: &Row| {
        if !is_cpu(row) {
            return -1;
        }
        if lower(&row.name).contains("package") { 100 } else { 10 }
    };
    let fan_score = |row: &Row| {
        let t = text(row);
        if t.contains("/gpu") || t.contains("gpu fan") {
            return -1;
        }
        if !selector.is_empty() {
            return if t.contains(&selector) { 200 } else { -1 };
        }
        if t.contains("cpu_fan") || t.contains("cpu fan") {
            150
        } else if t.contains("cpu") {
            120
        } else if t.contains("fan #1") || t.contains("fan 1") || t.contains("/fan/0") {
            50
        } else {
            1
        }
    };
    let mobo_temp_score = |row: &Row| {
        let t = text(row);
        if ["cpu", "gpu", "vrm", "nvme", "storage"].iter().any(|w| t.contains(w)) {
            return -1;
        }
        if t.contains("motherboard") || lower(&row.name).contains("system") {
            100
        } else if lower(&row.id).contains("/lpc/") {
            50
        } else {
            -1
        }
    };
    // Most boards expose no VRM sensor, while every NVMe drive reports SMART
    // temperature, so the sixth reading is the hottest SSD.
    let ssd_temp_score = |row: &Row| {
        let t = text(row);
        if t.contains("gpu") || t.contains("cpu") {
            return -1;
        }
        if t.contains("nvme") {
            100
        } else if t.contains("/hdd/") || t.contains("storage") || t.contains("ssd") {
            80
        } else {
            -1
        }
    };
    let total_power_score = |row: &Row| {
        let t = lower(&format!("{} {}", row.name, row.id));
        if t.contains("total system power") {
            200
        } else if t.contains("psu") && (t.contains("input") || t.contains("total")) {
            150
        } else {
            -1
        }
    };

    let core_clocks: Vec<f64> = clocks
        .iter()
        .filter(|r| {
            let name = lower(&r.name);
            is_cpu(r)
                && !name.contains("bus")
                && (name.contains("core") || lower(&r.id).contains("/clock/"))
                && r.value > 200.0
        })
        .map(|r| r.value)
        .collect();

    // Storage throughput sensors are bytes per second, one pair per drive.
    let rate = |word: &str| -> f64 {
        throughput
            .iter()
            .filter(|r| lower(&r.name).contains(word))
            .map(|r| r.value.max(0.0))
            .sum()
    };

    Picked {
        cpu_temp: best(&temperatures, temperature_score),
        mobo_temp: best(&temperatures, mobo_temp_score),
        ssd_temp: best(&temperatures, ssd_temp_score),
        cpu_w: best(&powers, power_score),
        fan: best(&fans, fan_score),
        cpu_clock: if core_clocks.is_empty() {
            0.0
        } else {
            (core_clocks.iter().sum::<f64>() / core_clocks.len() as f64).round()
        },
        total_w: best(&powers, total_power_score),
        disk_read: rate("read rate"),
        disk_write: rate("write rate"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, name: &str, id: &str, parent: &str, value: f64) -> Row {
        Row { name: name.into(), kind: kind.into(), id: id.into(), parent: parent.into(), hw: String::new(), value }
    }

    fn sample_rows() -> Vec<Row> {
        vec![
            row("Temperature", "Core (Tctl/Tdie)", "/amdcpu/0/temperature/2", "/amdcpu/0", 61.5),
            row("Temperature", "CCD1 (Tdie)", "/amdcpu/0/temperature/3", "/amdcpu/0", 58.0),
            row("Temperature", "System", "/lpc/nct6799d/0/temperature/1", "/lpc/nct6799d/0", 36.0),
            row("Temperature", "CPU", "/lpc/nct6799d/0/temperature/0", "/lpc/nct6799d/0", 50.0),
            row("Temperature", "Composite Temperature", "/nvme/0/temperature/0", "/nvme/0", 44.0),
            row("Temperature", "Composite Temperature", "/nvme/1/temperature/0", "/nvme/1", 47.0),
            row("Power", "Package", "/amdcpu/0/power/0", "/amdcpu/0", 72.3),
            row("Power", "Core #1 (SMU)", "/amdcpu/0/power/1", "/amdcpu/0", 9.0),
            row("Fan", "Fan #1", "/lpc/nct6799d/0/fan/0", "/lpc/nct6799d/0", 900.0),
            row("Fan", "CPU Fan", "/lpc/nct6799d/0/fan/1", "/lpc/nct6799d/0", 1200.0),
            row("Fan", "Fan #3", "/lpc/nct6799d/0/fan/2", "/lpc/nct6799d/0", 0.0),
            row("Clock", "Core #1", "/amdcpu/0/clock/1", "/amdcpu/0", 5000.0),
            row("Clock", "Core #2", "/amdcpu/0/clock/2", "/amdcpu/0", 4800.0),
            row("Clock", "Bus Speed", "/amdcpu/0/clock/0", "/amdcpu/0", 100.0),
            row("Throughput", "Read Rate", "/nvme/0/throughput/0", "/nvme/0", 2.0 * 1048576.0),
            row("Throughput", "Write Rate", "/nvme/0/throughput/1", "/nvme/0", 1048576.0),
            row("Throughput", "Read Rate", "/nvme/1/throughput/0", "/nvme/1", 1048576.0),
        ]
    }

    #[test]
    fn picks_like_the_python_sender() {
        let picked = pick(&sample_rows(), "");
        assert_eq!(picked.cpu_temp, 61.5);
        assert_eq!(picked.mobo_temp, 36.0);
        assert_eq!(picked.ssd_temp, 47.0);
        assert_eq!(picked.cpu_w, 72.3);
        assert_eq!(picked.fan, 1200.0);
        assert_eq!(picked.cpu_clock, 4900.0);
        assert_eq!(picked.total_w, 0.0);
        assert_eq!(picked.disk_read, 3.0 * 1048576.0);
        assert_eq!(picked.disk_write, 1048576.0);
    }

    #[test]
    fn fan_selector_overrides() {
        assert_eq!(pick(&sample_rows(), "fan #1").fan, 900.0);
        assert_eq!(pick(&sample_rows(), "nothing matches").fan, 0.0);
    }

    #[test]
    fn helper_line_parses() {
        let line = r#"{"rows":[{"name":"Package","type":"Power","id":"/amdcpu/0/power/0","parent":"/amdcpu/0","hw":"AMD Ryzen","value":12.5}]}"#;
        let parsed: Line = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.rows.unwrap()[0].value, 12.5);
    }
}
