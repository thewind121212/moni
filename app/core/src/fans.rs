//! Fan control: the board's fan headers by curve, from boot, in place of
//! Gigabyte Control Center. The sensor helper (LibreHardwareMonitor) sets the
//! headers; this decides the speeds once a sample.
//!
//! - CPU fans (the radiator) follow the CPU curve.
//! - Intakes follow the higher of the GPU curve and the CPU fans' speed,
//!   plus the intake offset: more air always comes in than goes out, so the
//!   case stays at positive pressure and dust only gets in through filters
//!   (a hot CPU with a cool GPU would otherwise pull air in through every gap).
//! - Exhausts follow the higher of the two curves. A pump runs at 100 %.
//!
//! Temperatures are smoothed and get hysteresis (a Ryzen's one-second spikes
//! don't rev the fans); speeds rise quickly and fall slowly. At 90 °C CPU or
//! 85 °C GPU every fan goes to 100 %. When fan control is switched off, or
//! the service stops (or dies), the headers go back to the BIOS curves.

use crate::config::{FanRole, Fans as Settings};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const CPU_HOT: f64 = 90.0;
pub const GPU_HOT: f64 = 85.0;
/// How fast speeds may change, % a second.
const RISE: f64 = 10.0;
const FALL: f64 = 3.0;
/// Temperature smoothing, seconds.
const CPU_SMOOTH: f64 = 4.0;
const GPU_SMOOTH: f64 = 2.0;
/// Identify: a header at 100 % this long.
pub const IDENTIFY: Duration = Duration::from_secs(6);

/// The speed for a temperature on a curve of [°C, %] points: linear between
/// points, flat past the ends.
pub fn curve(points: &[[f64; 2]], t: f64) -> f64 {
    let Some(first) = points.first() else { return 100.0 };
    let last = points.last().expect("not empty");
    if t <= first[0] {
        return first[1];
    }
    if t >= last[0] {
        return last[1];
    }
    for pair in points.windows(2) {
        let ([t0, p0], [t1, p1]) = (pair[0], pair[1]);
        if t <= t1 {
            return if t1 > t0 { p0 + (p1 - p0) * (t - t0) / (t1 - t0) } else { p1 };
        }
    }
    last[1]
}

/// A temperature, smoothed and with hysteresis: rises are followed (after
/// smoothing), falls only once they're `hysteresis` below the last level.
#[derive(Default, Debug, Clone)]
pub struct Smooth {
    average: Option<f64>,
    level: Option<f64>,
}

impl Smooth {
    pub fn feed(&mut self, t: f64, dt: f64, tau: f64, hysteresis: f64) -> f64 {
        let average = match self.average {
            Some(old) => old + (t - old) * (dt / tau.max(0.1)).min(1.0),
            None => t,
        };
        self.average = Some(average);
        let level = match self.level {
            Some(level) if average < level && average > level - hysteresis => level,
            _ => average,
        };
        self.level = Some(level);
        level
    }
}

/// The speed each role asks for, from smoothed temperatures (None: no sensor).
pub fn role_speeds(s: &Settings, cpu: Option<f64>, gpu: Option<f64>) -> HashMap<FanRole, f64> {
    // No CPU temperature: fast enough to be safe. No GPU: the CPU leads.
    let cpu_speed = cpu.map(|t| curve(&s.cpu_curve, t)).unwrap_or(80.0);
    let gpu_speed = gpu.map(|t| curve(&s.gpu_curve, t)).unwrap_or(cpu_speed);
    HashMap::from([
        (FanRole::Cpu, cpu_speed),
        (FanRole::Intake, (gpu_speed.max(cpu_speed) + s.intake_offset as f64).min(100.0)),
        (FanRole::Exhaust, cpu_speed.max(gpu_speed)),
        (FanRole::Pump, 100.0),
    ])
}

/// One command for the sensor helper.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Set(String, f64),
    /// Back to the BIOS.
    Default(String),
}

impl Command {
    pub fn line(&self) -> String {
        match self {
            Command::Set(id, pct) => format!("set {id} {pct:.0}"),
            Command::Default(id) => format!("default {id}"),
        }
    }
}

/// The running state: smoothed temperatures, the speed each header has now,
/// and the headers taken from the BIOS.
#[derive(Default)]
pub struct FanControl {
    cpu: Smooth,
    gpu: Smooth,
    speeds: HashMap<String, f64>,
    taken: HashSet<String>,
    identify: Option<(String, Instant)>,
    pub note: String,
}

impl FanControl {
    /// Spin one header to 100 % for a few seconds.
    pub fn identify(&mut self, control: &str) {
        self.identify = Some((control.to_string(), Instant::now()));
    }

    /// The speed each header has now (for the app).
    pub fn speed(&self, control: &str) -> Option<f64> {
        self.speeds.get(control).copied()
    }

    /// Give every header back to the BIOS.
    pub fn release(&mut self) -> Vec<Command> {
        self.speeds.clear();
        self.cpu = Smooth::default();
        self.gpu = Smooth::default();
        self.taken.drain().map(Command::Default).collect()
    }

    /// One sample: the commands to send. `cpu` / `gpu`: raw temperatures, °C
    /// (0 or less: no sensor). `paused`: another fan program is running.
    pub fn step(&mut self, s: &Settings, cpu: f64, gpu: f64, dt: f64, paused: Option<&str>) -> Vec<Command> {
        if !s.enabled || paused.is_some() {
            self.note = match paused {
                Some(why) if s.enabled => format!("paused: {why}"),
                _ => "off: the BIOS runs the fans".into(),
            };
            return self.release();
        }
        let dt = dt.clamp(0.0, 5.0);
        let (cpu_raw, gpu_raw) = ((cpu > 0.0).then_some(cpu), (gpu > 0.0).then_some(gpu));
        let cpu_t = cpu_raw.map(|t| self.cpu.feed(t, dt, CPU_SMOOTH, s.hysteresis));
        let gpu_t = gpu_raw.map(|t| self.gpu.feed(t, dt, GPU_SMOOTH, s.hysteresis));
        let hot = cpu_raw.is_some_and(|t| t >= CPU_HOT) || gpu_raw.is_some_and(|t| t >= GPU_HOT);
        let wanted = role_speeds(s, cpu_t, gpu_t);
        if self.identify.as_ref().is_some_and(|(_, at)| at.elapsed() >= IDENTIFY) {
            self.identify = None;
        }

        let mut out = Vec::new();
        let mut used = HashSet::new();
        for group in s.groups.iter().filter(|g| !g.control.is_empty() && g.role != FanRole::Bios) {
            if !used.insert(group.control.clone()) {
                continue; // (one header in two groups: the first wins)
            }
            let identifying = self.identify.as_ref().is_some_and(|(id, _)| *id == group.control);
            let target = if hot || identifying || group.role == FanRole::Pump {
                100.0
            } else {
                wanted.get(&group.role).copied().unwrap_or(100.0).max(group.min as f64).min(100.0)
            };
            let now = self.speeds.get(&group.control).copied();
            let speed = match now {
                // (an emergency, an identify or a pump jump straight there)
                Some(now) if !(hot || identifying) => {
                    if target > now { (now + RISE * dt).min(target) } else { (now - FALL * dt).max(target) }
                }
                _ => target,
            };
            self.speeds.insert(group.control.clone(), speed);
            self.taken.insert(group.control.clone());
            out.push(Command::Set(group.control.clone(), speed.round()));
        }
        // Headers no group uses any more go back to the BIOS.
        let dropped: Vec<String> = self.taken.iter().filter(|id| !used.contains(*id)).cloned().collect();
        for id in dropped {
            self.taken.remove(&id);
            self.speeds.remove(&id);
            out.push(Command::Default(id));
        }

        self.note = if hot {
            "too hot: every fan at 100 %".into()
        } else if used.is_empty() {
            "on, but no group has a header yet: pick one for each".into()
        } else {
            let fmt = |t: Option<f64>| t.map(|t| format!("{t:.0} °C")).unwrap_or_else(|| "–".into());
            format!("on: CPU {}, GPU {}", fmt(cpu_t), fmt(gpu_t))
        };
        out
    }
}

/// Another program running the fans: Gigabyte Control Center or FanControl.
/// (name: the process' file name; exe: its full path)
pub fn rival(name: &str, exe: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let exe = exe.to_lowercase();
    if name == "fancontrol.exe" {
        Some("FanControl is running (quit it, and take it out of startup)")
    } else if name == "gcc.exe" && exe.contains("gigabyte") {
        Some("Gigabyte Control Center is running (quit it, and turn off its start with Windows)")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FanGroup, Fans};

    fn mine() -> Settings {
        let mut s = Fans { enabled: true, ..Fans::default() };
        for (group, id) in s.groups.iter_mut().zip(["rad", "bottom", "side", "rear", "pump"]) {
            group.control = id.into();
        }
        s
    }

    fn set(out: &[Command], id: &str) -> f64 {
        out.iter()
            .find_map(|c| match c {
                Command::Set(i, p) if i == id => Some(*p),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no speed for {id} in {out:?}"))
    }

    #[test]
    fn curves_are_linear_and_flat_past_the_ends() {
        let c = [[40.0, 30.0], [60.0, 40.0], [75.0, 60.0], [85.0, 80.0]];
        assert_eq!(curve(&c, 20.0), 30.0);
        assert_eq!(curve(&c, 50.0), 35.0);
        assert_eq!(curve(&c, 67.5), 50.0);
        assert_eq!(curve(&c, 99.0), 80.0);
    }

    #[test]
    fn intakes_never_fall_behind_the_exhausts() {
        let s = Fans::default();
        // Gaming: GPU hot, CPU cool.
        let gaming = role_speeds(&s, Some(55.0), Some(75.0));
        assert!(gaming[&FanRole::Intake] > gaming[&FanRole::Cpu]);
        // CPU work: CPU hot, GPU cool: intakes follow the radiator, + 10 %.
        let cpu_work = role_speeds(&s, Some(80.0), Some(40.0));
        assert_eq!(cpu_work[&FanRole::Cpu], 70.0);
        assert_eq!(cpu_work[&FanRole::Intake], 80.0);
        assert_eq!(cpu_work[&FanRole::Exhaust], 70.0);
        // Idle.
        let idle = role_speeds(&s, Some(38.0), Some(35.0));
        assert_eq!((idle[&FanRole::Cpu], idle[&FanRole::Intake]), (30.0, 40.0));
    }

    #[test]
    fn smoothing_and_hysteresis() {
        let mut t = Smooth::default();
        assert_eq!(t.feed(50.0, 1.0, 4.0, 3.0), 50.0);
        // A one-second spike to 80 moves the level only a quarter of the way.
        assert_eq!(t.feed(80.0, 1.0, 4.0, 3.0), 57.5);
        // Falling back a little: the level holds (hysteresis 3 °C).
        assert_eq!(t.feed(56.0, 1.0, 4.0, 3.0), 57.5);
        // Falling well below: it follows, in steps of at least 3 °C, so it
        // settles within 3 °C of the real temperature.
        let level = (0..20).map(|_| t.feed(45.0, 1.0, 4.0, 3.0)).last().unwrap();
        assert!((45.0..48.0).contains(&level), "{level}");
    }

    #[test]
    fn speeds_rise_fast_and_fall_slowly() {
        let s = mine();
        let mut f = FanControl::default();
        let first = f.step(&s, 40.0, 40.0, 1.0, None);
        assert_eq!(set(&first, "rad"), 30.0, "first sample: straight to the curve");
        assert_eq!(set(&first, "pump"), 100.0);
        let mut rad = 30.0;
        for _ in 0..60 {
            rad = set(&f.step(&s, 85.0, 40.0, 1.0, None), "rad");
        }
        assert_eq!(rad, 80.0);
        let after = set(&f.step(&s, 40.0, 40.0, 1.0, None), "rad");
        assert!((76.0..80.0).contains(&after), "falls 3 %/s: {after}");
    }

    #[test]
    fn too_hot_means_full_speed_at_once() {
        let s = mine();
        let mut f = FanControl::default();
        f.step(&s, 40.0, 40.0, 1.0, None);
        let out = f.step(&s, 92.0, 40.0, 1.0, None);
        for id in ["rad", "bottom", "side", "rear"] {
            assert_eq!(set(&out, id), 100.0, "{id}");
        }
        assert!(f.note.contains("too hot"));
    }

    #[test]
    fn minimum_speed_and_missing_sensors() {
        let mut s = mine();
        s.groups[3].min = 45;
        let mut f = FanControl::default();
        let out = f.step(&s, 30.0, 0.0, 1.0, None);
        assert_eq!(set(&out, "rear"), 45.0, "never below its minimum");
        assert_eq!(set(&out, "bottom"), 40.0, "no GPU sensor: intakes follow the radiator + 10 %");
        let out = FanControl::default().step(&s, 0.0, 0.0, 1.0, None);
        assert_eq!(set(&out, "rad"), 80.0, "no CPU sensor: safe and fast");
    }

    #[test]
    fn off_or_paused_hands_the_fans_back() {
        let s = mine();
        let mut f = FanControl::default();
        f.step(&s, 50.0, 50.0, 1.0, None);
        let out = f.step(&s, 50.0, 50.0, 1.0, Some("FanControl is running"));
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|c| matches!(c, Command::Default(_))));
        assert!(f.note.starts_with("paused"));
        assert!(f.step(&s, 50.0, 50.0, 1.0, Some("x")).is_empty(), "once");
        let off = Fans { enabled: false, ..s.clone() };
        f.step(&s, 50.0, 50.0, 1.0, None);
        assert_eq!(f.step(&off, 50.0, 50.0, 1.0, None).len(), 5);
    }

    #[test]
    fn identify_spins_one_header() {
        let s = mine();
        let mut f = FanControl::default();
        f.step(&s, 40.0, 40.0, 1.0, None);
        f.identify("side");
        let out = f.step(&s, 40.0, 40.0, 1.0, None);
        assert_eq!(set(&out, "side"), 100.0);
        assert_eq!(set(&out, "bottom"), 40.0);
    }

    #[test]
    fn a_header_taken_out_of_a_group_goes_back_to_the_bios() {
        let mut s = mine();
        let mut f = FanControl::default();
        f.step(&s, 40.0, 40.0, 1.0, None);
        s.groups[2] = FanGroup { control: String::new(), ..s.groups[2].clone() };
        let out = f.step(&s, 40.0, 40.0, 1.0, None);
        assert!(out.contains(&Command::Default("side".into())));
        assert_eq!(Command::Set("/lpc/it8689e/0/control/1".into(), 42.0).line(), "set /lpc/it8689e/0/control/1 42");
    }

    #[test]
    fn spots_other_fan_programs() {
        assert!(rival("FanControl.exe", "C:\\Tools\\FanControl\\FanControl.exe").is_some());
        assert!(rival("GCC.exe", "C:\\Program Files\\GIGABYTE\\Control Center\\GCC.exe").is_some());
        assert!(rival("gcc.exe", "C:\\msys64\\mingw64\\bin\\gcc.exe").is_none(), "the compiler isn't it");
    }
}
