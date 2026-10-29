//! Moni's own OpenRGB: the installer ships OpenRGB (GPL-2.0, unmodified, in
//! `<install dir>\openrgb`, source: https://codeberg.org/OpenRGB/OpenRGB) and
//! the service runs it with no window, as a server on 127.0.0.1:6743 only, so
//! there's nothing to install, start or forget. 6743 rather than OpenRGB's
//! 6742 so it never answers for (or collides with) a copy the user started.
//!
//! Its configuration lives next to it, under Program Files, not in
//! %ProgramData%\Moni: OpenRGB loads plugins from its configuration folder,
//! and %ProgramData%\Moni is writable by every user, while this runs as
//! LocalSystem.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub const PORT: u16 = 6743;
/// OpenRGB scans every bus before its server answers: give it this long
/// before saying it's stuck.
pub const STARTUP: Duration = Duration::from_secs(60);
const RESTART_AFTER: Duration = Duration::from_secs(5);

pub fn addr() -> String {
    format!("127.0.0.1:{PORT}")
}

/// The bundled OpenRGB.exe (MONI_OPENRGB_EXE overrides, for testing).
pub fn bundled_exe() -> PathBuf {
    if let Some(path) = std::env::var_os("MONI_OPENRGB_EXE") {
        return PathBuf::from(path);
    }
    let dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    dir.join("openrgb").join("OpenRGB.exe")
}

/// The command line: server only (no GUI, so no window and no tray icon), on
/// loopback, not looking for other servers, settings in `config`.
pub fn args(config: &Path) -> Vec<String> {
    vec![
        "--server".into(),
        "--server-host".into(),
        "127.0.0.1".into(),
        "--server-port".into(),
        PORT.to_string(),
        "--noautoconnect".into(),
        "--config".into(),
        config.to_string_lossy().into_owned(),
    ]
}

pub struct Host {
    exe: PathBuf,
    child: Option<Child>,
    started: Option<Instant>,
    retry_at: Instant,
    swept: bool,
    pub note: String,
}

impl Host {
    pub fn new(exe: PathBuf) -> Host {
        Host { exe, child: None, started: None, retry_at: Instant::now(), swept: false, note: String::new() }
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Starts OpenRGB if it isn't running (again 5 s after it stopped).
    pub fn ensure(&mut self) {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) => {
                    self.note = format!("Moni's OpenRGB stopped ({status}); starting it again");
                    crate::paths::log(&self.note);
                }
                Err(_) => {}
            }
            self.child = None;
            self.started = None;
            self.retry_at = Instant::now() + RESTART_AFTER;
        }
        if Instant::now() < self.retry_at {
            return;
        }
        if !self.swept {
            // One left behind by a service that crashed would hold the USB device.
            self.swept = true;
            let gone = kill_copies_of(&self.exe);
            if gone > 0 {
                crate::paths::log(&format!("stopped {gone} OpenRGB left running from before"));
            }
        }
        let config = self.exe.parent().unwrap_or(Path::new(".")).join("config");
        let _ = std::fs::create_dir_all(&config);
        let mut command = Command::new(&self.exe);
        command.args(args(&config)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        match command.spawn() {
            Ok(child) => {
                crate::paths::log(&format!("started Moni's OpenRGB (process {}, port {PORT})", child.id()));
                self.child = Some(child);
                self.started = Some(Instant::now());
                self.note.clear();
            }
            Err(e) => {
                self.note = format!("can't start Moni's OpenRGB ({e}); reinstall Moni");
                crate::paths::log(&self.note);
                self.retry_at = Instant::now() + RESTART_AFTER * 6;
            }
        }
    }

    /// Still within its start-up scan.
    pub fn starting(&self) -> bool {
        self.started.map(|at| at.elapsed() < STARTUP).unwrap_or(false)
    }

    pub fn running(&self) -> bool {
        self.child.is_some()
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            crate::paths::log("stopped Moni's OpenRGB");
        }
        self.started = None;
        self.retry_at = Instant::now();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Is this process an OpenRGB? (by file name, any case)
pub fn is_openrgb(name: &str) -> bool {
    name.eq_ignore_ascii_case("openrgb.exe") || name.eq_ignore_ascii_case("openrgb")
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

fn processes() -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet).with_cmd(UpdateKind::OnlyIfNotSet),
    );
    system
}

/// Kills every process running this exact OpenRGB.exe.
pub fn kill_copies_of(exe: &Path) -> usize {
    processes()
        .processes()
        .values()
        .filter(|p| p.exe().map(|e| same_file(e, exe)).unwrap_or(false))
        .filter(|p| p.kill())
        .count()
}

/// Another program on the same LEDs (the two would fight and the fans flash):
/// cpu_rgb.py, or an OpenRGB that isn't Moni's own. -> what to tell the user
pub fn find_rival(system: &mut System, own: Option<&Path>) -> Option<String> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet).with_cmd(UpdateKind::OnlyIfNotSet),
    );
    for p in system.processes().values() {
        if crate::lights::rival(p.cmd()) {
            return Some(format!(
                "paused: cpu_rgb.py (process {}) is lighting the fans too, and the two would flash. Stop it (Stop-Process -Name pythonw) and remove it from startup.",
                p.pid()
            ));
        }
        let other_openrgb = is_openrgb(&p.name().to_string_lossy())
            && own.is_some_and(|own| !p.exe().map(|e| same_file(e, own)).unwrap_or(false));
        if other_openrgb {
            return Some(format!(
                "paused: your own OpenRGB is open (process {}). Moni runs its own now: close yours, take it out of startup (or uninstall it), and the fans come back here.",
                p.pid()
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_headless_on_loopback() {
        let a = args(Path::new("C:\\Program Files\\Moni\\openrgb\\config"));
        assert!(a.contains(&"--server".to_string()));
        assert!(!a.iter().any(|x| x == "--gui" || x == "--startminimized"), "no window");
        let at = |flag: &str| a[a.iter().position(|x| x == flag).unwrap() + 1].clone();
        assert_eq!(at("--server-host"), "127.0.0.1");
        assert_eq!(at("--server-port"), "6743");
        assert_eq!(at("--config"), "C:\\Program Files\\Moni\\openrgb\\config");
        assert!(a.contains(&"--noautoconnect".to_string()));
    }

    #[test]
    fn knows_openrgb_by_name() {
        assert!(is_openrgb("OpenRGB.exe"));
        assert!(is_openrgb("openrgb.EXE"));
        assert!(!is_openrgb("OpenRGBHelper.exe"));
        assert!(same_file(Path::new("C:\\Program Files\\Moni\\openrgb\\OpenRGB.exe"), Path::new("c:\\program files\\moni\\OPENRGB\\openrgb.exe")));
    }

    #[test]
    fn restarts_a_program_that_stops() {
        // A stand-in that exits at once: the host notices and waits before the next start.
        let mut host = Host::new(PathBuf::from("/bin/true"));
        host.swept = true;
        host.ensure();
        assert!(host.running());
        std::thread::sleep(Duration::from_millis(200));
        host.ensure();
        assert!(!host.running(), "noticed it stopped");
        assert!(host.note.contains("stopped"), "{}", host.note);
        host.ensure();
        assert!(!host.running(), "waits 5 s before starting it again");
    }

    #[test]
    fn says_so_when_it_cannot_start() {
        let mut host = Host::new(PathBuf::from("/nonexistent/OpenRGB.exe"));
        host.swept = true;
        host.ensure();
        assert!(!host.running());
        assert!(host.note.contains("reinstall Moni"), "{}", host.note);
    }
}
