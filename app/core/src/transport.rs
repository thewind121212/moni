//! Delivery to the clock: USB serial (CH340) and WiFi UDP.
//!
//! The firmware binds UDP 5005 on any address and never replies, so without a
//! configured IP we send each line to the directed broadcast address of every
//! LAN this PC is on. The clock picks it up whatever DHCP gave it.

use serde::{Deserialize, Serialize};
use serialport::{SerialPort, SerialPortType};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub const UDP_PORT: u16 = 5005;
const CH340_VIDS: [u16; 2] = [0x1A86, 0x1A2C];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PortInfo {
    pub name: String,
    pub description: String,
    /// Higher means more likely to be the clock's CH340.
    pub score: i32,
}

pub fn list_ports() -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|port| {
            let mut score = 0;
            let mut description = String::new();
            if let SerialPortType::UsbPort(usb) = &port.port_type {
                if CH340_VIDS.contains(&usb.vid) || usb.pid == 0x7523 {
                    score += 100;
                }
                description = format!(
                    "{} {}",
                    usb.product.clone().unwrap_or_default(),
                    usb.manufacturer.clone().unwrap_or_default()
                )
                .trim()
                .to_string();
                let lower = description.to_lowercase();
                if ["ch340", "usb-serial", "usb serial", "cp210"].iter().any(|l| lower.contains(l)) {
                    score += 50;
                }
            }
            if port.port_name.to_uppercase().starts_with("COM") {
                score += 1;
            }
            PortInfo { name: port.port_name, description, score }
        })
        .collect();
    ports.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    ports
}

/// The clock's COM port. `strict` (Auto mode) only accepts real USB-serial
/// adapters; USB mode, like the Python sender, also takes a lone COM port.
pub fn resolve_port(explicit: &str, strict: bool) -> Option<String> {
    if !explicit.is_empty() {
        return Some(explicit.to_string());
    }
    let ports = list_ports();
    let first = ports.first()?;
    if first.score > 1 || (!strict && ports.len() == 1) {
        Some(first.name.clone())
    } else {
        None
    }
}

pub struct SerialLink {
    port: Option<Box<dyn SerialPort>>,
    pub name: Option<String>,
    pub note: String,
    last_attempt: Option<Instant>,
}

impl SerialLink {
    pub fn new() -> SerialLink {
        SerialLink { port: None, name: None, note: "not connected".into(), last_attempt: None }
    }

    pub fn close(&mut self, note: &str) {
        self.port = None;
        self.name = None;
        self.note = note.into();
    }

    /// Opens the port if needed (at most every 2 s) and writes one line.
    /// `hello` is sent first on every fresh connection.
    pub fn send(&mut self, line: &str, hello: &str, explicit: &str, strict: bool, baud: u32) {
        if self.port.is_none() {
            if self.last_attempt.map(|at| at.elapsed() < Duration::from_secs(2)).unwrap_or(false) {
                return;
            }
            self.last_attempt = Some(Instant::now());
            let Some(name) = resolve_port(explicit, strict) else {
                self.note = "clock not plugged in".into();
                return;
            };
            // DTR/RTS stay low: toggling them resets the ESP32.
            match serialport::new(&name, baud)
                .timeout(Duration::from_secs(5))
                .dtr_on_open(false)
                .open()
            {
                Ok(mut port) => {
                    let _ = port.write_request_to_send(false);
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = port.clear(serialport::ClearBuffer::Input);
                    let _ = port.write_all(format!("{hello}\n").as_bytes());
                    crate::paths::log(&format!("USB connected on {name} at {baud} baud"));
                    self.note = format!("connected on {name}");
                    self.name = Some(name);
                    self.port = Some(port);
                }
                Err(error) => {
                    self.note = format!("{name}: {error}");
                    return;
                }
            }
        }
        if let Some(port) = self.port.as_mut() {
            if let Err(error) = port.write_all(format!("{line}\n").as_bytes()) {
                crate::paths::log(&format!("USB disconnected: {error}"));
                self.close(&format!("disconnected: {error}"));
            }
        }
    }
}

impl SerialLink {
    /// Reboot the ESP32 through the USB adapter's auto-reset circuit: RTS pulls
    /// EN low while DTR (IO0) stays released, the pulse esptool ends a flash
    /// with. Uses the open port, or opens the clock's port just for this.
    /// Returns the port name.
    pub fn restart_clock(&mut self, explicit: &str, baud: u32) -> Result<String, String> {
        let name = match &self.name {
            Some(name) => name.clone(),
            None => resolve_port(explicit, true).ok_or("the clock is not plugged in over USB")?,
        };
        let mut opened;
        let port = match self.port.as_mut() {
            Some(port) => port,
            None => {
                opened = serialport::new(&name, baud)
                    .timeout(Duration::from_secs(2))
                    .dtr_on_open(false)
                    .open()
                    .map_err(|e| format!("{name}: {e}"))?;
                &mut opened
            }
        };
        let pulse = |port: &mut Box<dyn SerialPort>| -> serialport::Result<()> {
            // usbser.sys only applies an RTS change with a DTR write; esptool does the same.
            port.write_data_terminal_ready(false)?;
            port.write_request_to_send(true)?;
            port.write_data_terminal_ready(false)?;
            std::thread::sleep(Duration::from_millis(100));
            port.write_request_to_send(false)?;
            port.write_data_terminal_ready(false)
        };
        pulse(port).map_err(|e| format!("{name}: {e}"))?;
        // Reconnect on the next send, which greets the rebooted clock with hello.
        self.close("restarting the clock");
        self.last_attempt = Some(Instant::now());
        Ok(name)
    }
}

impl Default for SerialLink {
    fn default() -> Self {
        Self::new()
    }
}

/// Directed broadcast address (e.g. 192.168.1.255) of every active IPv4 LAN.
pub fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut targets: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_link_local() && !v4.ip.is_loopback() => {
                directed_broadcast(v4.ip, v4.netmask)
            }
            _ => None,
        })
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

fn directed_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    // A /32 or /31 has no broadcast address worth using.
    if mask == u32::MAX || mask == u32::MAX - 1 {
        return None;
    }
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

/// "192.168.1.50" or "192.168.1.50:5005" or a hostname such as "moni.lan".
pub fn parse_target(text: &str) -> Option<SocketAddr> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, UDP_PORT));
    }
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Some(addr);
    }
    let with_port = if text.contains(':') { text.to_string() } else { format!("{text}:{UDP_PORT}") };
    with_port.to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

pub struct UdpLink {
    socket: Option<UdpSocket>,
    targets: Vec<SocketAddr>,
    target_key: String,
    refreshed: Option<Instant>,
    sent: u64,
    pub note: String,
}

impl UdpLink {
    pub fn new() -> UdpLink {
        UdpLink { socket: None, targets: Vec::new(), target_key: String::new(), refreshed: None, sent: 0, note: String::new() }
    }

    pub fn targets(&self) -> Vec<String> {
        self.targets.iter().map(|t| t.to_string()).collect()
    }

    fn refresh(&mut self, configured: &str) {
        // Interfaces come and go (WiFi/VPN, DHCP at boot); re-read every 10 s.
        let stale = self.refreshed.map(|at| at.elapsed() > Duration::from_secs(10)).unwrap_or(true);
        if !stale && self.target_key == configured {
            return;
        }
        self.refreshed = Some(Instant::now());
        self.target_key = configured.to_string();
        self.targets = if configured.is_empty() {
            broadcast_targets()
                .into_iter()
                .map(|ip| SocketAddr::V4(SocketAddrV4::new(ip, UDP_PORT)))
                .collect()
        } else {
            match parse_target(configured) {
                Some(addr) => vec![addr],
                None => {
                    self.note = format!("cannot resolve {configured}");
                    Vec::new()
                }
            }
        };
        if self.socket.is_none() {
            match UdpSocket::bind("0.0.0.0:0").and_then(|s| s.set_broadcast(true).map(|_| s)) {
                Ok(socket) => self.socket = Some(socket),
                Err(error) => self.note = format!("UDP socket: {error}"),
            }
        }
    }

    pub fn send(&mut self, line: &str, hello: &str, configured: &str) {
        self.refresh(configured);
        let Some(socket) = &self.socket else { return };
        if self.targets.is_empty() {
            if configured.is_empty() {
                self.note = "no network yet".into();
            }
            return;
        }
        let mut failures = Vec::new();
        for target in &self.targets {
            // Re-announce every minute so a rebooted clock gets the metadata.
            if self.sent % 60 == 0 {
                let _ = socket.send_to(format!("{hello}\n").as_bytes(), target);
            }
            if let Err(error) = socket.send_to(format!("{line}\n").as_bytes(), target) {
                failures.push(format!("{target}: {error}"));
            }
        }
        self.sent += 1;
        let mode = if configured.is_empty() { "broadcast" } else { "direct" };
        self.note = if failures.is_empty() {
            format!("{mode} to {}", self.targets().join(", "))
        } else {
            format!("{mode} failed: {}", failures.join("; "))
        };
        if !failures.is_empty() {
            // Network changed under us: rebuild the socket and targets next time.
            self.socket = None;
            self.refreshed = None;
        }
    }
}

impl Default for UdpLink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        assert_eq!(parse_target("192.168.1.50"), Some("192.168.1.50:5005".parse().unwrap()));
        assert_eq!(parse_target("10.0.0.9:6000"), Some("10.0.0.9:6000".parse().unwrap()));
        assert_eq!(parse_target("  "), None);
    }

    #[test]
    fn computes_directed_broadcast() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(directed_broadcast(ip("192.168.1.23"), ip("255.255.255.0")), Some(ip("192.168.1.255")));
        assert_eq!(directed_broadcast(ip("10.10.0.217"), ip("255.255.0.0")), Some(ip("10.10.255.255")));
        assert_eq!(directed_broadcast(ip("10.0.0.5"), ip("255.255.255.255")), None);
    }

    // Hosts with strict firewalls (Docker, nftables) drop looped-back
    // broadcasts, so this only runs on request: cargo test -- --ignored
    #[test]
    #[ignore]
    fn broadcast_reaches_a_local_listener() {
        // The clock binds 0.0.0.0:5005; emulate it on a free port.
        let listener = UdpSocket::bind("0.0.0.0:0").unwrap();
        listener.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let Some(target) = broadcast_targets().into_iter().next() else { return };
        let sender = UdpSocket::bind("0.0.0.0:0").unwrap();
        sender.set_broadcast(true).unwrap();
        sender.send_to(b"{\"v\":1}\n", SocketAddrV4::new(target, port)).unwrap();
        let mut buf = [0u8; 64];
        let (len, _) = listener.recv_from(&mut buf).expect("broadcast not received");
        assert_eq!(&buf[..len], b"{\"v\":1}\n");
    }
}
