//! LibreHardwareMonitor sensor feed. The C# helper (moni-sensors.exe) prints
//! every sensor once a second; this module keeps the latest set and picks the
//! values the clock shows, using the same scoring as host/monitor_windows.py.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// A fan command the helper couldn't carry out.
    #[serde(default)]
    fan_error: Option<String>,
}

#[derive(Default)]
struct Latest {
    rows: Vec<Row>,
    at: Option<Instant>,
    error: Option<String>,
    fan_error: Option<(String, Instant)>,
}

/// Owns the helper process and restarts it whenever it exits, until [`SensorFeed::stop`].
pub struct SensorFeed {
    latest: Arc<Mutex<Latest>>,
    child: Arc<Mutex<Option<Child>>>,
    stopping: Arc<AtomicBool>,
}

impl SensorFeed {
    pub fn start(exe: PathBuf, interval_ms: u64) -> SensorFeed {
        let feed = SensorFeed {
            latest: Arc::new(Mutex::new(Latest::default())),
            child: Arc::new(Mutex::new(None)),
            stopping: Arc::new(AtomicBool::new(false)),
        };
        let (latest, child, stopping) = (feed.latest.clone(), feed.child.clone(), feed.stopping.clone());
        thread::Builder::new()
            .name("moni-sensors".into())
            .spawn(move || supervise(exe, interval_ms, latest, child, stopping))
            .expect("spawn sensor thread");
        feed
    }

    /// Stop the helper for good: close its stdin so it shuts LibreHardwareMonitor
    /// down cleanly, and kill it if it hasn't exited after 2 s. Returns what happened.
    pub fn stop(&self) -> String {
        self.stopping.store(true, Ordering::SeqCst);
        let Some(mut child) = self.child.lock().unwrap().take() else {
            return "not running".into();
        };
        drop(child.stdin.take());
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if let Ok(Some(_)) = child.try_wait() {
                return format!("closed in {} ms", started.elapsed().as_millis());
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        "did not close in 2 s, killed".into()
    }

    /// Rows from the last 5 s; older data is treated as no data.
    pub fn rows(&self) -> Vec<Row> {
        let latest = self.latest.lock().unwrap();
        match latest.at {
            Some(at) if at.elapsed() < Duration::from_secs(5) => latest.rows.clone(),
            _ => Vec::new(),
        }
    }

    /// One fan command for the helper (`set <control id> <percent>` or
    /// `default <control id>`). False when the helper isn't running.
    pub fn command(&self, line: &str) -> bool {
        let mut slot = self.child.lock().unwrap();
        let Some(stdin) = slot.as_mut().and_then(|child| child.stdin.as_mut()) else { return false };
        stdin.write_all(format!("{line}\n").as_bytes()).and_then(|_| stdin.flush()).is_ok()
    }

    /// The helper's last fan error, if it's from the last 10 s.
    pub fn fan_error(&self) -> Option<String> {
        let latest = self.latest.lock().unwrap();
        latest.fan_error.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(10)).map(|(e, _)| e.clone())
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

fn supervise(
    exe: PathBuf,
    interval_ms: u64,
    latest: Arc<Mutex<Latest>>,
    slot: Arc<Mutex<Option<Child>>>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::SeqCst) {
        match spawn(&exe, interval_ms) {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                {
                    // stdin stays open (held in `slot`) so the helper keeps running.
                    let mut slot = slot.lock().unwrap();
                    if stopping.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    *slot = Some(child);
                }
                if let Some(stdout) = stdout {
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
                        } else if let Some(error) = parsed.fan_error {
                            slot.fan_error = Some((error, Instant::now()));
                        }
                    }
                }
                if stopping.load(Ordering::SeqCst) {
                    return; // stop() owns the child now
                }
                let status = slot.lock().unwrap().take().map(|mut child| {
                    let _ = child.kill();
                    child.wait()
                });
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

/// User overrides from Settings: a sensor identifier (or any unique part of
/// its name/identifier). Empty means pick automatically.
#[derive(Debug, Default, Clone, Copy)]
pub struct Choices<'a> {
    pub fan: &'a str,
    pub mobo: &'a str,
    pub ssd: &'a str,
}

/// Exact identifier match first, then substring of name/id/parent.
fn chosen<'a>(rows: &[&'a Row], selector: &str) -> Option<&'a Row> {
    if selector.is_empty() {
        return None;
    }
    let wanted = lower(selector);
    rows.iter()
        .find(|r| lower(&r.id) == wanted)
        .or_else(|| rows.iter().find(|r| lower(&format!("{} {} {}", r.name, r.id, r.parent)).contains(&wanted)))
        .copied()
}

/// Unlabelled Super I/O channels: LibreHardwareMonitor names channel N
/// "Temperature #N+1" when it does not know the board.
fn generic_name(row: &Row) -> bool {
    let name = lower(&row.name);
    name.starts_with("temperature #") || name == "temperature"
}

pub fn pick(rows: &[Row], choices: Choices) -> Picked {
    let of = |kind: &str| -> Vec<&Row> {
        rows.iter().filter(|r| r.kind.eq_ignore_ascii_case(kind)).collect()
    };
    let temperatures = of("temperature");
    let powers = of("power");
    let fans: Vec<&Row> = of("fan").into_iter().filter(|r| r.value > 0.0).collect();
    let clocks: Vec<&Row> = of("clock").into_iter().filter(|r| r.value > 0.0).collect();
    let throughput = of("throughput");
    let selector = lower(choices.fan);
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
    // Disconnected thermistors read 0, -55 or 127+; never show those.
    let plausible = |row: &Row| row.value > 0.0 && row.value < 120.0;
    let mobo_temp_score = |row: &Row| {
        let t = text(row);
        let name = lower(&row.name);
        let id = lower(&row.id);
        if !plausible(row) || ["cpu", "gpu", "vrm", "mos", "nvme", "storage", "/ssd/", "/hdd/"].iter().any(|w| t.contains(w)) {
            return -1;
        }
        if name.contains("motherboard") || name.contains("system") {
            100
        } else if id.contains("/lpc/it") && generic_name(row) && id.ends_with("/temperature/0") {
            // Gigabyte ITE chips (IT8689E on every B650 board LHM knows):
            // channel 0 is "System", 1 PCH, 2 CPU, 3 PCIe, 4 VRM MOS, 5 VSoC MOS.
            90
        } else if name.contains("pch") || name.contains("chipset") {
            80
        } else if id.contains("/lpc/") && !generic_name(row) {
            // A labelled board sensor we do not recognise (e.g. "PCIe x16").
            20
        } else {
            -1
        }
    };
    // Most boards expose no VRM sensor, while every drive reports SMART
    // temperature, so the sixth reading is the hottest drive. NVMe drives
    // also list their Warning/Critical thresholds as temperatures (fixed
    // values such as 84 C); those are limits, not readings.
    let ssd_temp_score = |row: &Row| {
        let t = text(row);
        let name = lower(&row.name);
        let drive = t.contains("/nvme/") || t.contains("/ssd/") || t.contains("/hdd/");
        if !drive || !plausible(row) || name.contains("warning") || name.contains("critical") {
            return -1;
        }
        if name == "composite temperature" || name == "temperature" {
            100
        } else {
            // Individual NVMe sensors (controller, NAND): only without a composite.
            50
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

    let mobo_temp = match chosen(&temperatures, choices.mobo) {
        Some(row) => row.value,
        None => best(&temperatures, mobo_temp_score),
    };
    let ssd_temp = match chosen(&temperatures, choices.ssd) {
        Some(row) => row.value,
        None => best(&temperatures, ssd_temp_score),
    };

    Picked {
        cpu_temp: best(&temperatures, temperature_score),
        mobo_temp,
        ssd_temp,
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

    #[cfg(unix)]
    fn script(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("moni-{name}-{}.sh", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn stop_closes_the_helper_and_keeps_it_stopped() {
        // a polite helper: exits when stdin closes
        let feed = SensorFeed::start(script("polite", "cat >/dev/null"), 1000);
        thread::sleep(Duration::from_millis(300));
        assert!(feed.stop().starts_with("closed in"));
        thread::sleep(Duration::from_millis(300));
        assert!(feed.child.lock().unwrap().is_none(), "not restarted");
        // a stuck helper: ignores stdin, gets killed
        let feed = SensorFeed::start(script("stuck", "exec sleep 30"), 1000);
        thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        assert_eq!(feed.stop(), "did not close in 2 s, killed");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

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
        let picked = pick(&sample_rows(), Choices::default());
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
        let fan = |fan| pick(&sample_rows(), Choices { fan, ..Default::default() }).fan;
        assert_eq!(fan("fan #1"), 900.0);
        assert_eq!(fan("nothing matches"), 0.0);
    }

    /// Gigabyte B650M/B650E FORCE: LHM does not know the board, so the
    /// IT8689E channels are unlabelled; channel 2 is the CPU socket.
    fn unlabelled_gigabyte() -> Vec<Row> {
        let mut rows = vec![row("Temperature", "Core (Tctl/Tdie)", "/amdcpu/0/temperature/2", "/amdcpu/0", 51.5)];
        for (i, value) in [33.0, 45.0, 51.0, 38.0, 47.0, 44.0].into_iter().enumerate() {
            rows.push(row(
                "Temperature",
                &format!("Temperature #{}", i + 1),
                &format!("/lpc/it8689e/0/temperature/{i}"),
                "/lpc/it8689e/0",
                value,
            ));
        }
        for (i, name, value) in [
            (0, "Composite Temperature", 41.0),
            (1, "Temperature #1", 41.0),
            (2, "Temperature #2", 58.0),
            (10, "Warning Temperature", 84.0),
            (11, "Critical Temperature", 85.0),
        ] {
            rows.push(row("Temperature", name, &format!("/nvme/0/temperature/{i}"), "/nvme/0", value));
        }
        rows.push(row("Temperature", "Composite Temperature", "/nvme/1/temperature/0", "/nvme/1", 39.0));
        rows
    }

    #[test]
    fn unlabelled_gigabyte_board_uses_system_channel() {
        let picked = pick(&unlabelled_gigabyte(), Choices::default());
        assert_eq!(picked.mobo_temp, 33.0, "channel 0 is System, not the CPU socket (51)");
    }

    #[test]
    fn ssd_ignores_nvme_thresholds() {
        let picked = pick(&unlabelled_gigabyte(), Choices::default());
        assert_eq!(picked.ssd_temp, 41.0, "hottest composite, not the 84 C warning threshold");
    }

    #[test]
    fn labelled_board_prefers_system() {
        let mut rows = unlabelled_gigabyte();
        for (r, name) in rows.iter_mut().skip(1).zip(["System", "PCH", "CPU", "PCIe x16", "VRM MOS", "VSoC MOS"]) {
            r.name = name.into();
        }
        assert_eq!(pick(&rows, Choices::default()).mobo_temp, 33.0);
    }

    #[test]
    fn manual_choices_override() {
        let rows = unlabelled_gigabyte();
        let picked = pick(&rows, Choices { mobo: "/lpc/it8689e/0/temperature/1", ssd: "/nvme/0/temperature/2", ..Default::default() });
        assert_eq!(picked.mobo_temp, 45.0);
        assert_eq!(picked.ssd_temp, 58.0);
    }

    #[test]
    fn helper_line_parses() {
        let line = r#"{"rows":[{"name":"Package","type":"Power","id":"/amdcpu/0/power/0","parent":"/amdcpu/0","hw":"AMD Ryzen","value":12.5}]}"#;
        let parsed: Line = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.rows.unwrap()[0].value, 12.5);
    }
}
