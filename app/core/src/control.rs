//! Remote shutdown for the home page's Turn off button.
//!
//! A tiny HTTP listener on [`CONTROL_PORT`] accepts one request,
//! `POST /shutdown`, sent by moni-relay on the LAN. The request carries
//!
//!   X-Moni-Time   unix seconds
//!   X-Moni-Nonce  16-64 hex characters, new for every request
//!   X-Moni-Sig    hex(HMAC-SHA256(key, "moni-shutdown|<time>|<nonce>"))
//!
//! where the key is `control_key` in settings, the same value as the relay's
//! MONI_PC_KEY. A request more than [`MAX_SKEW`] seconds off, or with a nonce
//! already used, is refused, so a captured request cannot be replayed. With no
//! key set every request is refused. Only private LAN addresses may connect.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CONTROL_PORT: u16 = 5008;
/// Seconds before Windows starts shutting down. 0, like Start > Shut down: the
/// home page already asked twice.
pub const SHUTDOWN_DELAY: u32 = 0;
const MAX_SKEW: u64 = 60;
const MIN_KEY: usize = 16;

pub fn key_ok(key: &str) -> bool {
    key.len() >= MIN_KEY
}

pub fn sign(key: &str, time: &str, nonce: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
    mac.update(format!("moni-shutdown|{time}|{nonce}").as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Remembers recent nonces so each signed request works once.
#[derive(Default)]
pub struct Guard {
    seen: Vec<(String, u64)>,
}

impl Guard {
    pub fn check(&mut self, key: &str, time: &str, nonce: &str, sig: &str, now: u64) -> Result<(), &'static str> {
        if !key_ok(key) {
            return Err("remote shutdown is off on this PC");
        }
        let at: u64 = time.parse().map_err(|_| "bad time")?;
        if at.abs_diff(now) > MAX_SKEW {
            return Err("request too old (check both clocks)");
        }
        if !(16..=64).contains(&nonce.len()) || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("bad nonce");
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
        mac.update(format!("moni-shutdown|{time}|{nonce}").as_bytes());
        let sig = unhex(sig).ok_or("bad signature")?;
        mac.verify_slice(&sig).map_err(|_| "bad signature")?;
        self.seen.retain(|(_, t)| t.abs_diff(now) <= 2 * MAX_SKEW);
        if self.seen.iter().any(|(n, _)| n == nonce) {
            return Err("request already used");
        }
        self.seen.push((nonce.to_string(), now));
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Start listening in the background. `key` is read on every request, so the
/// engine can change it when settings change. Returns a note for status.json.
pub fn start(key: Arc<Mutex<String>>) -> Arc<Mutex<String>> {
    let note = Arc::new(Mutex::new(String::new()));
    let shared = note.clone();
    let set = move |text: String| *shared.lock().unwrap() = text;
    match TcpListener::bind(("0.0.0.0", CONTROL_PORT)) {
        Ok(listener) => {
            set(format!("listening on TCP {CONTROL_PORT}"));
            std::thread::Builder::new()
                .name("control".into())
                .spawn(move || {
                    let mut guard = Guard::default();
                    for stream in listener.incoming().flatten() {
                        let key = key.lock().unwrap().clone();
                        if let Some(text) = handle(stream, &key, &mut guard) {
                            set(text);
                        }
                    }
                })
                .ok();
        }
        Err(error) => set(format!("TCP {CONTROL_PORT} unavailable: {error}")),
    }
    note
}

/// One request. Returns a new status note when something happened.
fn handle(mut stream: TcpStream, key: &str, guard: &mut Guard) -> Option<String> {
    let peer = stream.peer_addr().ok()?;
    if !lan(peer.ip()) {
        return None; // drop silently
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while buf.len() < 4096 && !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request = lines.next().unwrap_or("");
    let header = |name: &str| {
        text.split("\r\n")
            .skip(1)
            .take_while(|l| !l.is_empty())
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
            .unwrap_or("")
            .to_string()
    };
    if !request.starts_with("POST /shutdown ") {
        reply(&mut stream, 404, r#"{"ok":false,"error":"not found"}"#);
        return None;
    }
    match guard.check(key, &header("x-moni-time"), &header("x-moni-nonce"), &header("x-moni-sig"), now()) {
        Ok(()) => match shutdown() {
            Ok(()) => {
                reply(&mut stream, 200, &format!(r#"{{"ok":true,"in":{SHUTDOWN_DELAY}}}"#));
                crate::paths::log(&format!("shutdown requested by {}", peer.ip()));
                Some(format!("shutting down (asked by {})", peer.ip()))
            }
            Err(error) => {
                reply(&mut stream, 500, &format!(r#"{{"ok":false,"error":{}}}"#, serde_json::json!(error)));
                crate::paths::log(&format!("shutdown failed: {error}"));
                None
            }
        },
        Err(error) => {
            reply(&mut stream, 403, &format!(r#"{{"ok":false,"error":"{error}"}}"#));
            crate::paths::log(&format!("refused a shutdown request from {}: {error}", peer.ip()));
            None
        }
    }
}

fn reply(stream: &mut TcpStream, code: u16, body: &str) {
    let reason = match code {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[cfg(windows)]
fn shutdown() -> Result<(), String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    // /f: nobody is at the PC to answer "this app is preventing shutdown".
    let status = std::process::Command::new(format!(r"{root}\System32\shutdown.exe"))
        .args(["/s", "/f", "/t", &SHUTDOWN_DELAY.to_string(), "/d", "p:0:0"])
        .status()
        .map_err(|e| e.to_string())?;
    // 1190: a shutdown is already scheduled. That's what was asked for.
    if status.success() || status.code() == Some(1190) { Ok(()) } else { Err(format!("shutdown.exe {status}")) }
}

#[cfg(not(windows))]
fn shutdown() -> Result<(), String> {
    Err("remote shutdown only works on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "test-key-0123456789abcdef";
    const NONCE: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn signed_request_works_once() {
        let mut guard = Guard::default();
        let sig = sign(KEY, "1000", NONCE);
        assert_eq!(guard.check(KEY, "1000", NONCE, &sig, 1010), Ok(()));
        assert_eq!(guard.check(KEY, "1000", NONCE, &sig, 1011), Err("request already used"));
    }

    #[test]
    fn refuses_bad_requests() {
        let mut guard = Guard::default();
        let sig = sign(KEY, "1000", NONCE);
        assert!(guard.check("", "1000", NONCE, &sig, 1000).is_err(), "no key = off");
        assert!(guard.check("short", "1000", NONCE, &sig, 1000).is_err(), "short key = off");
        assert!(guard.check(KEY, "1000", NONCE, &sig, 1100).is_err(), "stale");
        assert!(guard.check(KEY, "1001", NONCE, &sig, 1000).is_err(), "time changed");
        assert!(guard.check("other-key-0123456789abcdef", "1000", NONCE, &sig, 1000).is_err(), "wrong key");
        assert!(guard.check(KEY, "1000", "zz", &sig, 1000).is_err(), "bad nonce");
        assert!(guard.check(KEY, "1000", NONCE, "abc", 1000).is_err(), "bad hex");
        assert_eq!(guard.check(KEY, "1000", NONCE, &sig, 1000), Ok(()), "the good one still works");
    }

    #[test]
    fn matches_python_hmac() {
        // python3 -c 'import hmac,hashlib;print(hmac.new(b"test-key-0123456789abcdef",
        //   b"moni-shutdown|1000|00112233445566778899aabbccddeeff",hashlib.sha256).hexdigest())'
        assert_eq!(sign(KEY, "1000", NONCE), "9526878b35dae7779be7a73ad5179956919cef1c56136a4783b6ed47b2a30da5");
    }
}
