//! Fan lights: the motherboard's ARGB headers glow with CPU load through
//! OpenRGB, 30 frames a second. Replaces cpu_rgb.py, and runs from boot with
//! the service (OpenRGB itself still has to be open with its SDK server on).
//!
//! The effect: comets 18 LEDs long run along each zone. More CPU load means
//! faster comets and a head further along the combo's colors. φ shapes it:
//!
//!   cpu      = old × 1/φ + new × 1/φ²   each sample (0.618 + 0.382 = 1)
//!   effect   = (cpu / 100) ^ (1/φ)      boosts low load (an idle PC is 0-10 %)
//!   speed    = 0.22 … 0.22 × φ⁵ passes a second, gliding to the new value
//!
//! If OpenRGB isn't there (not started yet, closed, SDK server off), this
//! tries again every 5 s and says why in the app.

use crate::config::Lights as Settings;
use crate::openrgb::{self, Client, Controller, TYPE_MOTHERBOARD, ZONE_SINGLE};
use crate::openrgb_host::{self, Host};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use sysinfo::System;
use std::time::{Duration, Instant};

const PHI: f64 = 1.618_033_988_749_895;
const CURVE: f64 = 1.0 / PHI;
const SPEED_MIN: f64 = 0.22;
const PERIOD: f64 = 18.0;
const FRAME: Duration = Duration::from_millis(33);
const RETRY: Duration = Duration::from_secs(5);
/// How often the device list is read again (zones resized in OpenRGB, a
/// controller that came or went).
const RESCAN: Duration = Duration::from_secs(10);

fn speed_max() -> f64 {
    SPEED_MIN * PHI.powi(5)
}

type Rgb = [f64; 3];

pub struct Combo {
    pub name: &'static str,
    /// Low load, middle, high load. Fire & Ice has no middle.
    pub low: Rgb,
    pub mid: Option<Rgb>,
    pub high: Rgb,
}

pub const COMBOS: [Combo; 12] = [
    Combo { name: "Traffic Light", low: [0., 255., 0.], mid: Some([255., 255., 0.]), high: [255., 0., 0.] },
    Combo { name: "Ice to Fire", low: [0., 120., 255.], mid: Some([255., 255., 255.]), high: [255., 40., 0.] },
    Combo { name: "Ocean", low: [0., 255., 200.], mid: Some([0., 120., 255.]), high: [0., 0., 255.] },
    Combo { name: "Cyberpunk", low: [0., 255., 255.], mid: Some([180., 0., 255.]), high: [255., 0., 120.] },
    Combo { name: "Lava", low: [255., 120., 0.], mid: Some([255., 40., 0.]), high: [255., 0., 0.] },
    Combo { name: "Galaxy", low: [60., 0., 255.], mid: Some([160., 0., 255.]), high: [255., 0., 255.] },
    Combo { name: "Toxic", low: [0., 255., 0.], mid: Some([180., 255., 0.]), high: [255., 255., 0.] },
    Combo { name: "Sunset", low: [255., 160., 0.], mid: Some([255., 0., 100.]), high: [120., 0., 255.] },
    Combo { name: "Frost", low: [40., 200., 255.], mid: Some([0., 90., 255.]), high: [0., 20., 255.] },
    Combo { name: "Matrix to Alarm", low: [0., 120., 0.], mid: Some([0., 255., 80.]), high: [255., 0., 0.] },
    Combo { name: "Fire & Ice", low: [0., 140., 255.], mid: None, high: [255., 50., 0.] },
    Combo { name: "Blue & Purple", low: [0., 70., 255.], mid: Some([140., 0., 255.]), high: [220., 0., 255.] },
];

pub fn combo(number: u8) -> &'static Combo {
    &COMBOS[(number.clamp(1, COMBOS.len() as u8) - 1) as usize]
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn mod1(x: f64) -> f64 {
    x.rem_euclid(1.0)
}

/// Effect level 0-1 for a CPU %: (cpu / 100) ^ (1/φ).
pub fn effect_level(cpu: f64) -> f64 {
    (cpu.clamp(0.0, 100.0) / 100.0).powf(CURVE)
}

/// Comet passes per second at an effect level.
pub fn speed(level: f64) -> f64 {
    SPEED_MIN + (speed_max() - SPEED_MIN) * level.clamp(0.0, 1.0)
}

/// The comet head's color at an effect level: low -> mid -> high with load
/// (Fire & Ice: ice -> fire).
pub fn head(level: f64, combo: &Combo) -> Rgb {
    let level = level.clamp(0.0, 1.0);
    match combo.mid {
        Some(mid) if level < 0.5 => mix(combo.low, mid, level * 2.0),
        Some(mid) => mix(mid, combo.high, (level - 0.5) * 2.0),
        None => mix(combo.low, combo.high, level),
    }
}

fn dim(c: Rgb, brightness: f64) -> [u8; 3] {
    c.map(|v| (v * brightness).round().clamp(0.0, 255.0) as u8)
}

/// One zone's colors: `n` LEDs, the comets at `pos` (0-1, one comet length),
/// at effect level `level`, dimmed to `brightness` (0-1).
pub fn frame(n: usize, pos: f64, level: f64, combo: &Combo, brightness: f64) -> Vec<[u8; 3]> {
    let to_u8 = |c: Rgb| dim(c, brightness);
    match combo.mid {
        Some(mid) => {
            // The first part of each comet keeps the head color, the rest fades
            // to the middle color.
            let head = head(level, combo);
            (0..n)
                .map(|i| {
                    let d = mod1(i as f64 / PERIOD - pos);
                    let k = (1.0 - d * 1.1).max(0.12);
                    to_u8(mix(head, mid, ((d - 0.35) * 3.0).clamp(0.0, 1.0)).map(|v| v * k))
                })
                .collect()
        }
        None => {
            // Fire & Ice: an ice comet and a fire comet half a period apart; load
            // shortens the ice and lengthens the fire.
            let (ice, fire) = (combo.low, combo.high);
            let (ice_len, fire_len) = (0.85 - 0.6 * level, 0.25 + 0.6 * level);
            (0..n)
                .map(|i| {
                    let x = i as f64 / PERIOD - pos;
                    let ki = (1.0 - mod1(x) / ice_len).max(0.0);
                    let kf = (1.0 - mod1(x - 0.5) / fire_len).max(0.0);
                    to_u8([0, 1, 2].map(|c| (ice[c] * ki + fire[c] * kf + 8.0).min(255.0)))
                })
                .collect()
        }
    }
}

/// The animation between CPU samples: the level glides to its target so the
/// speed never jumps, and the comets advance by the speed each frame.
#[derive(Default)]
pub struct Motion {
    pub level: f64,
    pub pos: f64,
}

impl Motion {
    /// `scale`: the Speed setting (1 = as on the page).
    pub fn step(&mut self, target: f64, dt: f64, scale: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.level += (target - self.level) * (dt * 2.0).min(1.0);
        self.pos = mod1(self.pos + speed(self.level) * scale * dt);
    }
}

/// The controller to light: the one named in settings (part of its name), else
/// the motherboard, else the first.
pub fn pick<'a>(controllers: &'a [Controller], wanted: &str) -> Option<&'a Controller> {
    let wanted = wanted.trim().to_lowercase();
    if !wanted.is_empty() {
        return controllers.iter().find(|c| c.name.to_lowercase().contains(&wanted));
    }
    controllers.iter().find(|c| c.kind == TYPE_MOTHERBOARD).or_else(|| controllers.first())
}

/// Which of the controller's zones get the effect: settings' comma-separated
/// names (exact, any case), or all of them when empty.
pub fn chosen_zones(controller: &Controller, wanted: &str) -> Vec<bool> {
    let names: Vec<String> = wanted.split(',').map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()).collect();
    controller.zones.iter().map(|z| names.is_empty() || names.contains(&z.name.to_lowercase())).collect()
}

/// The whole controller's colors for one frame: chosen zones get the effect
/// (each zone from its own first LED, like each header on its own), the rest
/// keep what OpenRGB had. A zone of one LED (the board's own light, a 12 V RGB
/// header) can't show a comet, and pulsing it would flash the whole thing at
/// every pass: it glows steadily in the head color instead.
pub fn board_frame(controller: &Controller, chosen: &[bool], motion: &Motion, combo: &Combo, brightness: f64) -> Vec<[u8; 3]> {
    let mut out = Vec::with_capacity(controller.led_count());
    for (i, zone) in controller.zones.iter().enumerate() {
        let start = out.len();
        if !chosen.get(i).copied().unwrap_or(false) {
            out.extend((start..start + zone.leds as usize).map(|at| controller.colors.get(at).copied().unwrap_or([0, 0, 0])));
        } else if zone.kind == ZONE_SINGLE || zone.leds <= 1 {
            out.extend(std::iter::repeat_n(dim(head(motion.level, combo), brightness), zone.leds as usize));
        } else {
            out.extend(frame(zone.leds as usize, motion.pos, motion.level, combo, brightness));
        }
    }
    out
}

/// Shared between the service loop (settings, CPU samples), the service's
/// power events and the light thread.
#[derive(Default)]
struct Shared {
    settings: Settings,
    cpu: Option<f64>,
    on: bool,
    note: String,
    /// Windows is going to sleep: lights off until it wakes.
    asleep: bool,
    /// The thread has turned them off for the sleep.
    dark: bool,
    stop: bool,
    finished: bool,
}

/// The service's lights, for power events (they come in on the service's
/// control thread, which doesn't see the engine).
static CURRENT: Mutex<Option<Arc<Mutex<Shared>>>> = Mutex::new(None);

/// Windows is about to sleep: turn the LEDs off (the board keeps the ARGB
/// headers powered while asleep, showing the last frame) and stop Moni's
/// OpenRGB (USB devices come back new after a sleep). Waits up to `wait`.
pub fn suspend(wait: Duration) {
    let Some(shared) = CURRENT.lock().unwrap().clone() else { return };
    shared.lock().unwrap().asleep = true;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline && !shared.lock().unwrap().dark {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Windows woke up: the lights come back (OpenRGB starts afresh).
pub fn resume() {
    if let Some(shared) = CURRENT.lock().unwrap().clone() {
        let mut s = shared.lock().unwrap();
        s.asleep = false;
        s.dark = false;
    }
}

/// Where the colors go: Moni's own OpenRGB (bundled), or one the user runs on
/// OpenRGB's usual port (a build without the bundle, and the tests).
pub enum Source {
    Own(Host),
    External(String),
}

impl Source {
    pub fn pick() -> Source {
        let exe = openrgb_host::bundled_exe();
        if exe.exists() {
            Source::Own(Host::new(exe))
        } else {
            Source::External(openrgb::DEFAULT_ADDR.into())
        }
    }

    fn addr(&self) -> String {
        match self {
            Source::Own(_) => openrgb_host::addr(),
            Source::External(addr) => addr.clone(),
        }
    }

    fn stop(&mut self) {
        if let Source::Own(host) = self {
            host.stop();
        }
    }
}

pub struct Lights {
    shared: Arc<Mutex<Shared>>,
}

impl Lights {
    pub fn start(settings: Settings) -> Lights {
        let shared = Arc::new(Mutex::new(Shared { settings, note: "starting".into(), ..Shared::default() }));
        *CURRENT.lock().unwrap() = Some(shared.clone());
        let inner = shared.clone();
        let _ = std::thread::Builder::new().name("lights".into()).spawn(move || run(inner, Source::pick()));
        Lights { shared }
    }

    pub fn configure(&self, settings: &Settings) {
        self.shared.lock().unwrap().settings = settings.clone();
    }

    /// One CPU sample (0-100), smoothed with φ weights.
    pub fn sample(&self, cpu: f64) {
        let mut shared = self.shared.lock().unwrap();
        let cpu = cpu.clamp(0.0, 100.0);
        shared.cpu = Some(match shared.cpu {
            Some(old) => old / PHI + cpu / (PHI * PHI),
            None => cpu,
        });
    }

    /// (lighting now, what it's doing or why not)
    pub fn status(&self) -> (bool, String) {
        let shared = self.shared.lock().unwrap();
        (shared.on, shared.note.clone())
    }

    /// The service is stopping (Windows shutting down or restarting, or an
    /// update): LEDs off, then Moni's OpenRGB closed. Waits up to `wait`, so
    /// it never holds Windows up.
    pub fn shutdown(&self, wait: Duration) {
        self.shared.lock().unwrap().stop = true;
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline && !self.shared.lock().unwrap().finished {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

struct Target {
    controller: Controller,
    chosen: Vec<bool>,
    /// The settings this was set up for: a change sets it up again.
    settings: Settings,
}

/// Another program driving the same LEDs, by its command line: the two would
/// fight and the fans flash. cpu_rgb.py is the one this replaces.
pub fn rival(cmd: &[OsString]) -> bool {
    cmd.iter().any(|arg| arg.to_string_lossy().to_lowercase().replace('\\', "/").ends_with("cpu_rgb.py"))
}

/// The lit zones black (the others as they were). OpenRGB hands colors to the
/// device on its own thread, so give it a moment before it's closed.
fn blackout(client: &mut Option<Client>, target: &Option<Target>) {
    if let (Some(c), Some(t)) = (client.as_mut(), target.as_ref()) {
        let colors = board_frame(&t.controller, &t.chosen, &Motion::default(), combo(1), 0.0);
        if c.update_leds(t.controller.index, &colors).is_ok() {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

fn run(shared: Arc<Mutex<Shared>>, mut source: Source) {
    let mut client: Option<Client> = None;
    let mut target: Option<Target> = None;
    let mut retry_at = Instant::now();
    let mut scan_at = Instant::now();
    let mut motion = Motion::default();
    let mut last = Instant::now();
    let mut said = String::new();
    let mut system = System::new();
    let mut rival_at = Instant::now();
    let mut rival_seen: Option<String> = None;

    loop {
        let started = Instant::now();
        let (settings, cpu, stop, asleep, dark) = {
            let s = shared.lock().unwrap();
            (s.settings.clone(), s.cpu.unwrap_or(0.0), s.stop, s.asleep, s.dark)
        };
        let note = |on: bool, text: String, said: &mut String| {
            if *said != text {
                crate::paths::log(&format!("fan lights: {text}"));
                *said = text.clone();
            }
            let mut s = shared.lock().unwrap();
            s.on = on;
            s.note = text;
        };

        if stop {
            blackout(&mut client, &target);
            source.stop();
            shared.lock().unwrap().finished = true;
            break;
        }
        if asleep {
            if !dark {
                blackout(&mut client, &target);
                client = None;
                target = None;
                source.stop();
                shared.lock().unwrap().dark = true;
                note(false, "off while the PC sleeps".into(), &mut said);
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if !settings.enabled {
            blackout(&mut client, &target);
            client = None;
            target = None;
            source.stop();
            note(false, "off".into(), &mut said);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }

        if rival_at <= Instant::now() {
            rival_at = Instant::now() + RETRY;
            let own = match &source {
                Source::Own(host) => Some(host.exe().to_path_buf()),
                Source::External(_) => None,
            };
            rival_seen = openrgb_host::find_rival(&mut system, own.as_deref());
        }
        if let Some(text) = &rival_seen {
            // Leave the fans to the other program rather than both flashing them.
            client = None;
            target = None;
            source.stop();
            note(false, text.clone(), &mut said);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }

        if let Source::Own(host) = &mut source {
            host.ensure();
            if !host.running() {
                client = None;
                let text = if host.note.is_empty() { "starting OpenRGB".to_string() } else { host.note.clone() };
                note(false, text, &mut said);
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
        }

        if client.is_none() {
            if Instant::now() < retry_at {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let addr = source.addr();
            match Client::connect(&addr, "Moni") {
                Ok(c) => {
                    client = Some(c);
                    scan_at = Instant::now();
                }
                Err(e) => {
                    let text = match &mut source {
                        Source::Own(host) if host.starting() => "starting OpenRGB (it looks through the PC's devices first, up to a minute)".to_string(),
                        Source::Own(host) => {
                            host.stop(); // starts again on the next round
                            format!("Moni's OpenRGB isn't answering ({e}); restarting it")
                        }
                        Source::External(_) => format!("can't reach OpenRGB on {addr} ({e}). Open OpenRGB and start its SDK server."),
                    };
                    note(false, text, &mut said);
                    retry_at = Instant::now() + Duration::from_secs(2);
                    continue;
                }
            }
            target = None;
        }
        let c = client.as_mut().expect("connected above");

        // (no device yet: look again when scan_at comes round)
        let stale = target.as_ref().is_some_and(|t| t.settings != settings);
        if stale || scan_at <= Instant::now() {
            scan_at = Instant::now() + RESCAN;
            match setup(c, &settings, target.as_ref()) {
                Ok(Some(t)) => {
                    let lit: Vec<String> = t
                        .controller
                        .zones
                        .iter()
                        .zip(&t.chosen)
                        .filter(|(_, on)| **on)
                        .map(|(z, _)| format!("{} {}", z.name, z.leds))
                        .collect();
                    note(true, format!("{}: {}", t.controller.name, if lit.is_empty() { "no zone matches".into() } else { lit.join(", ") }), &mut said);
                    target = Some(t);
                }
                Ok(None) => {
                    let text = if settings.device.trim().is_empty() {
                        "OpenRGB found no RGB devices (yet)".to_string()
                    } else {
                        format!("OpenRGB has no device named like \"{}\"", settings.device.trim())
                    };
                    note(false, text, &mut said);
                    target = None;
                    scan_at = Instant::now() + RETRY;
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Err(e) => {
                    note(false, format!("OpenRGB stopped answering ({e}); trying again"), &mut said);
                    client = None;
                    retry_at = Instant::now() + RETRY;
                    continue;
                }
            }
        }

        let now = Instant::now();
        motion.step(effect_level(cpu), now.duration_since(last).as_secs_f64(), settings.speed as f64 / 100.0);
        last = now;
        if let Some(t) = &target {
            let colors = board_frame(&t.controller, &t.chosen, &motion, combo(settings.combo), settings.brightness as f64 / 100.0);
            if let Err(e) = c.update_leds(t.controller.index, &colors) {
                note(false, format!("OpenRGB stopped answering ({e}); trying again"), &mut said);
                client = None;
                retry_at = Instant::now() + RETRY;
                continue;
            }
        }
        if let Some(rest) = FRAME.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Finds the controller, sizes its zones, and switches it to Direct mode (only
/// when it's a different controller than last time).
fn setup(c: &mut Client, settings: &Settings, before: Option<&Target>) -> std::io::Result<Option<Target>> {
    let controllers = c.controllers()?;
    let Some(found) = pick(&controllers, &settings.device) else { return Ok(None) };
    let mut controller = found.clone();
    let mut chosen = chosen_zones(&controller, &settings.zones);
    if settings.leds_per_zone > 0 {
        let mut resized = false;
        for (i, zone) in controller.zones.iter().enumerate() {
            let want = settings.leds_per_zone.clamp(zone.leds_min, zone.leds_max);
            if chosen[i] && zone.resizable() && zone.leds != want {
                c.resize_zone(controller.index, i as u32, want)?;
                resized = true;
            }
        }
        if resized {
            controller = c.controller(controller.index)?;
            chosen = chosen_zones(&controller, &settings.zones);
        }
    }
    // (zones we don't light keep what OpenRGB reports: we only ever send that back)
    let same = before.map(|t| t.controller.index == controller.index && t.controller.name == controller.name);
    if same != Some(true) {
        c.set_custom_mode(controller.index)?;
    }
    Ok(Some(Target { controller, chosen, settings: settings.clone() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrgb::tests::description;
    use crate::openrgb::Zone;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn the_curve_matches_the_page() {
        // cpu_rgb.py's table: 0 % 0.22/s, 10 % ≈ 0.97/s, 100 % 2.44/s
        assert!((speed(effect_level(0.0)) - 0.22).abs() < 1e-9);
        assert!((speed(effect_level(100.0)) - 2.4396).abs() < 1e-3);
        assert!((speed(effect_level(10.0)) - (0.22 + (speed_max() - 0.22) * 0.1f64.powf(0.618_034))).abs() < 1e-6);
        assert!(effect_level(5.0) > 0.15, "low load is boosted");
    }

    #[test]
    fn the_head_moves_through_the_combo_with_load() {
        let blue_purple = combo(12);
        assert_eq!(blue_purple.name, "Blue & Purple");
        // The LED at the comet's head (d = 0) is the head color at full strength.
        assert_eq!(frame(1, 0.0, 0.0, blue_purple, 1.0)[0], [0, 70, 255]);
        assert_eq!(frame(1, 0.0, 0.5, blue_purple, 1.0)[0], [140, 0, 255]);
        assert_eq!(frame(1, 0.0, 1.0, blue_purple, 1.0)[0], [220, 0, 255]);
        assert_eq!(frame(1, 0.0, 1.0, blue_purple, 0.5)[0], [110, 0, 128]);
        // Along a comet the light fades but never goes fully dark.
        let strip = frame(18, 0.0, 0.0, blue_purple, 1.0);
        assert!(strip.iter().all(|c| c.iter().any(|&v| v > 0)));
        assert!(strip[17][2] < strip[0][2]);
        assert_eq!(combo(0).name, "Traffic Light");
        assert_eq!(combo(99).name, "Blue & Purple");
    }

    #[test]
    fn fire_and_ice_has_both() {
        let strip = frame(18, 0.0, 0.5, combo(11), 1.0);
        assert!(strip[0][2] > 200, "ice at the start: {:?}", strip[0]);
        assert!(strip[9][0] > 200, "fire half a period on: {:?}", strip[9]);
    }

    #[test]
    fn speed_glides_instead_of_jumping() {
        let mut m = Motion::default();
        m.step(1.0, 0.033, 1.0);
        assert!(m.level > 0.0 && m.level < 0.1, "{}", m.level);
        for _ in 0..200 {
            m.step(1.0, 0.033, 1.0);
        }
        assert!(m.level > 0.99);
        assert!((0.0..1.0).contains(&m.pos));
        // Speed 50 %: half as far per frame.
        let (mut full, mut half) = (Motion { level: 0.3, pos: 0.0 }, Motion { level: 0.3, pos: 0.0 });
        full.step(0.3, 0.05, 1.0);
        half.step(0.3, 0.05, 0.5);
        assert!((half.pos * 2.0 - full.pos).abs() < 1e-9);
    }

    fn board() -> Controller {
        Controller {
            index: 0,
            kind: TYPE_MOTHERBOARD,
            name: "B650 FORCE".into(),
            zones: vec![
                Zone { name: "ARGB_V2_1".into(), kind: 1, leds_min: 0, leds_max: 1024, leds: 3 },
                Zone { name: "Motherboard".into(), kind: 0, leds_min: 1, leds_max: 1, leds: 1 },
                Zone { name: "ARGB_V2_2".into(), kind: 1, leds_min: 0, leds_max: 1024, leds: 2 },
            ],
            colors: vec![[9, 9, 9]; 6],
        }
    }

    #[test]
    fn picks_the_board_and_the_zones() {
        let ram = Controller { kind: 1, name: "Kingston Fury".into(), ..board() };
        let list = vec![ram.clone(), board()];
        assert_eq!(pick(&list, "").unwrap().name, "B650 FORCE");
        assert_eq!(pick(&list, "fury").unwrap().name, "Kingston Fury");
        assert!(pick(&list, "corsair").is_none());
        assert_eq!(pick(&[ram], "").unwrap().name, "Kingston Fury");
        assert_eq!(chosen_zones(&board(), ""), vec![true, true, true]);
        assert_eq!(chosen_zones(&board(), "argb_v2_1, ARGB_V2_2"), vec![true, false, true]);
    }

    #[test]
    fn one_frame_covers_the_whole_board() {
        let b = board();
        let colors = board_frame(&b, &[true, false, true], &Motion::default(), combo(12), 1.0);
        assert_eq!(colors.len(), 6);
        assert_eq!(colors[0], [0, 70, 255]); // ARGB_V2_1 from its own first LED
        assert_eq!(colors[3], [9, 9, 9]); // the zone left alone keeps its color
        assert_eq!(colors[4], [0, 70, 255]); // ARGB_V2_2 from its own first LED
    }

    #[test]
    fn a_single_led_zone_glows_steadily() {
        let b = board();
        let at = |pos: f64| board_frame(&b, &[true, true, true], &Motion { level: 0.0, pos }, combo(12), 1.0)[3];
        assert_eq!(at(0.0), [0, 70, 255]);
        assert_eq!(at(0.5), [0, 70, 255], "no pulse as the comets pass");
        assert_eq!(at(0.9), [0, 70, 255]);
        assert_eq!(board_frame(&b, &[true, true, true], &Motion { level: 1.0, pos: 0.3 }, combo(12), 1.0)[3], [220, 0, 255]);
    }

    #[test]
    fn spots_cpu_rgb_py() {
        let cmd = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(rival(&cmd(&["C:\\Python312\\pythonw.exe", "C:\\Users\\linh\\cpu_rgb.py"])));
        assert!(rival(&cmd(&["py", "CPU_RGB.PY", "4"])));
        assert!(!rival(&cmd(&["pythonw.exe", "C:\\tools\\other.py"])));
        assert!(!rival(&cmd(&["notepad.exe", "cpu_rgb.py.txt"])));
    }

    /// Every packet the pretend OpenRGB got: (packet id, body).
    type Received = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

    /// A pretend OpenRGB: answers the version, the count and one board with a
    /// 10-LED ARGB zone, and records what Moni sends.
    fn fake_server(listener: TcpListener, log: Received) {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut leds = 10;
        let reply = |s: &mut std::net::TcpStream, id: u32, body: &[u8]| {
            let mut p = b"ORGB".to_vec();
            for v in [0u32, id, body.len() as u32] {
                p.extend_from_slice(&v.to_le_bytes());
            }
            p.extend_from_slice(body);
            s.write_all(&p).unwrap();
        };
        loop {
            let mut h = [0u8; 16];
            if s.read_exact(&mut h).is_err() {
                return;
            }
            let id = u32::from_le_bytes(h[8..12].try_into().unwrap());
            let mut body = vec![0u8; u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize];
            s.read_exact(&mut body).unwrap();
            log.lock().unwrap().push((id, body.clone()));
            match id {
                40 => {
                    reply(&mut s, 100, &[]); // a "device list updated" in between is skipped
                    reply(&mut s, 40, &5u32.to_le_bytes())
                }
                0 => reply(&mut s, 0, &1u32.to_le_bytes()),
                1 => {
                    assert_eq!(body, 3u32.to_le_bytes(), "asks for protocol 3");
                    let d = description("B650 FORCE", 0, &[("ARGB_V2_1", 1, 0, 1024, leds), ("Motherboard", 0, 1, 1, 1)], 3, false);
                    reply(&mut s, 1, &d)
                }
                1000 => leds = u32::from_le_bytes(body[4..8].try_into().unwrap()),
                _ => {}
            }
        }
    }

    #[test]
    fn talks_to_openrgb() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let log = Arc::new(Mutex::new(Vec::new()));
        let server_log = log.clone();
        std::thread::spawn(move || fake_server(listener, server_log));

        let settings = Settings { enabled: true, leds_per_zone: 120, ..Settings::default() };
        let shared = Arc::new(Mutex::new(Shared { settings, ..Shared::default() }));
        let inner = shared.clone();
        std::thread::spawn(move || run(inner, Source::External(addr)));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && log.lock().unwrap().iter().filter(|(id, _)| *id == 1050).count() < 3 {
            std::thread::sleep(Duration::from_millis(20));
        }
        shared.lock().unwrap().stop = true;

        let (on, note) = { let s = shared.lock().unwrap(); (s.on, s.note.clone()) };
        assert!(on, "{note}");
        assert_eq!(note, "B650 FORCE: ARGB_V2_1 120, Motherboard 1");
        let log = log.lock().unwrap();
        let ids: Vec<u32> = log.iter().map(|(id, _)| *id).collect();
        assert_eq!(&ids[..2], &[40, 50]);
        let resize = log.iter().find(|(id, _)| *id == 1000).expect("resized the ARGB zone");
        assert_eq!(resize.1, [0u32.to_le_bytes(), 120u32.to_le_bytes()].concat());
        assert!(!log.iter().any(|(id, b)| *id == 1000 && b[..4] == 1u32.to_le_bytes()), "a fixed zone isn't resized");
        let custom = ids.iter().position(|id| *id == 1100).expect("Direct mode");
        let first_frame = ids.iter().position(|id| *id == 1050).unwrap();
        assert!(custom < first_frame);
        let frame = &log[first_frame].1;
        assert_eq!(u16::from_le_bytes([frame[4], frame[5]]), 121, "every LED of the board in one packet");
    }

    #[test]
    fn says_why_when_openrgb_is_not_there() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let shared = Arc::new(Mutex::new(Shared { settings: Settings { enabled: true, ..Settings::default() }, ..Shared::default() }));
        let inner = shared.clone();
        std::thread::spawn(move || run(inner, Source::External(addr)));
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && shared.lock().unwrap().note.is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = shared.lock().unwrap();
        assert!(!s.on);
        assert!(s.note.contains("start its SDK server"), "{}", s.note);
        drop(s);
        shared.lock().unwrap().stop = true;
    }
}
