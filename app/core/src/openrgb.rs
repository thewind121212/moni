//! A small client for the OpenRGB SDK server (OpenRGB > SDK Server, port 6742).
//!
//! Only what the fan lights need: list the controllers, switch one to Direct
//! ("custom") mode, resize its addressable zones, and send every LED's color
//! in one packet. The protocol is little-endian packets with a 16-byte header:
//!
//!   "ORGB"  u32 device index  u32 packet id  u32 body size  | body
//!
//! We ask for protocol version 3 so the controller description has a known
//! layout (versions 4+ add zone segments and flags we don't need); the server
//! answers in min(3, its own version).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub const DEFAULT_ADDR: &str = "127.0.0.1:6742";
const PROTOCOL: u32 = 3;

const REQUEST_CONTROLLER_COUNT: u32 = 0;
const REQUEST_CONTROLLER_DATA: u32 = 1;
const REQUEST_PROTOCOL_VERSION: u32 = 40;
const SET_CLIENT_NAME: u32 = 50;
const RESIZE_ZONE: u32 = 1000;
const UPDATE_LEDS: u32 = 1050;
const SET_CUSTOM_MODE: u32 = 1100;

/// OpenRGB's device_type for a motherboard.
pub const TYPE_MOTHERBOARD: i32 = 0;
/// zone_type: one LED for the whole zone (not addressable).
pub const ZONE_SINGLE: i32 = 0;

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub name: String,
    pub kind: i32,
    pub leds_min: u32,
    pub leds_max: u32,
    pub leds: u32,
}

impl Zone {
    pub fn resizable(&self) -> bool {
        self.leds_min < self.leds_max
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controller {
    pub index: u32,
    pub kind: i32,
    pub name: String,
    pub zones: Vec<Zone>,
    /// The colors OpenRGB holds now, one per LED, zone after zone.
    pub colors: Vec<[u8; 3]>,
}

impl Controller {
    pub fn led_count(&self) -> usize {
        self.zones.iter().map(|z| z.leds as usize).sum()
    }
}

pub struct Client {
    stream: TcpStream,
    pub protocol: u32,
}

impl Client {
    pub fn connect(addr: &str, name: &str) -> io::Result<Client> {
        let addr: SocketAddr = addr.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad OpenRGB address"))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut client = Client { stream, protocol: 0 };
        client.send(0, REQUEST_PROTOCOL_VERSION, &PROTOCOL.to_le_bytes())?;
        // A server from before version 1 never answers this; it speaks 0.
        client.protocol = match client.expect(REQUEST_PROTOCOL_VERSION) {
            Ok(body) if body.len() >= 4 => u32_at(&body, 0).min(PROTOCOL),
            Ok(_) => 0,
            Err(e) if timed_out(&e) => 0,
            Err(e) => return Err(e),
        };
        let mut label = name.as_bytes().to_vec();
        label.push(0);
        client.send(0, SET_CLIENT_NAME, &label)?;
        Ok(client)
    }

    pub fn controllers(&mut self) -> io::Result<Vec<Controller>> {
        self.send(0, REQUEST_CONTROLLER_COUNT, &[])?;
        let count = u32_at(&self.expect(REQUEST_CONTROLLER_COUNT)?, 0);
        (0..count.min(64)).map(|index| self.controller(index)).collect()
    }

    pub fn controller(&mut self, index: u32) -> io::Result<Controller> {
        let body = if self.protocol > 0 { self.protocol.to_le_bytes().to_vec() } else { Vec::new() };
        self.send(index, REQUEST_CONTROLLER_DATA, &body)?;
        let data = self.expect(REQUEST_CONTROLLER_DATA)?;
        parse_controller(index, &data, self.protocol).ok_or_else(|| bad_data("controller description"))
    }

    /// Direct mode: the colors we send show as they are, no effect on top.
    pub fn set_custom_mode(&mut self, index: u32) -> io::Result<()> {
        self.send(index, SET_CUSTOM_MODE, &[])
    }

    pub fn resize_zone(&mut self, index: u32, zone: u32, leds: u32) -> io::Result<()> {
        let mut body = zone.to_le_bytes().to_vec();
        body.extend_from_slice(&leds.to_le_bytes());
        self.send(index, RESIZE_ZONE, &body)
    }

    /// Every LED of the controller in one packet (one per zone overloads it).
    pub fn update_leds(&mut self, index: u32, colors: &[[u8; 3]]) -> io::Result<()> {
        self.send(index, UPDATE_LEDS, &leds_body(colors))
    }

    fn send(&mut self, index: u32, id: u32, body: &[u8]) -> io::Result<()> {
        let mut packet = Vec::with_capacity(16 + body.len());
        packet.extend_from_slice(b"ORGB");
        packet.extend_from_slice(&index.to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&(body.len() as u32).to_le_bytes());
        packet.extend_from_slice(body);
        self.stream.write_all(&packet)
    }

    /// The next answer with this packet id; anything else the server sends in
    /// between (such as "device list updated") is read and dropped.
    fn expect(&mut self, id: u32) -> io::Result<Vec<u8>> {
        for _ in 0..32 {
            let mut header = [0u8; 16];
            self.stream.read_exact(&mut header)?;
            if &header[..4] != b"ORGB" {
                return Err(bad_data("packet header"));
            }
            let size = u32_at(&header, 12) as usize;
            if size > 4 << 20 {
                return Err(bad_data("packet size"));
            }
            let mut body = vec![0u8; size];
            self.stream.read_exact(&mut body)?;
            if u32_at(&header, 8) == id {
                return Ok(body);
            }
        }
        Err(bad_data("no answer"))
    }
}

pub fn leds_body(colors: &[[u8; 3]]) -> Vec<u8> {
    let count = colors.len().min(u16::MAX as usize);
    let size = 4 + 2 + 4 * count;
    let mut body = Vec::with_capacity(size);
    body.extend_from_slice(&(size as u32).to_le_bytes());
    body.extend_from_slice(&(count as u16).to_le_bytes());
    for [r, g, b] in &colors[..count] {
        body.extend_from_slice(&[*r, *g, *b, 0]);
    }
    body
}

fn timed_out(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

fn bad_data(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("OpenRGB sent a bad {what}"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
}

/// Reads the controller description OpenRGB's RGBController::GetDeviceDescription
/// writes, for protocol versions 0-3.
pub fn parse_controller(index: u32, data: &[u8], protocol: u32) -> Option<Controller> {
    let mut r = Reader { data, at: 4 }; // (u32 total size first)
    let kind = r.i32()?;
    let name = r.string()?;
    if protocol >= 1 {
        r.string()?; // vendor
    }
    for _ in 0..4 {
        r.string()?; // description, version, serial, location
    }
    let modes = r.u16()?;
    r.i32()?; // active mode
    for _ in 0..modes {
        r.string()?;
        r.skip(4 * 4)?; // value, flags, speed min / max
        if protocol >= 3 {
            r.skip(4 * 2)?; // brightness min / max
        }
        r.skip(4 * 3)?; // colors min / max, speed
        if protocol >= 3 {
            r.skip(4)?; // brightness
        }
        r.skip(4 * 2)?; // direction, color mode
        let colors = r.u16()? as usize;
        r.skip(4 * colors)?;
    }
    let zone_count = r.u16()?;
    let mut zones = Vec::with_capacity(zone_count as usize);
    for _ in 0..zone_count {
        let name = r.string()?;
        let kind = r.i32()?;
        let leds_min = r.u32()?;
        let leds_max = r.u32()?;
        let leds = r.u32()?;
        let matrix = r.u16()? as usize;
        r.skip(matrix)?;
        zones.push(Zone { name, kind, leds_min, leds_max, leds });
    }
    let leds = r.u16()?;
    for _ in 0..leds {
        r.string()?;
        r.skip(4)?;
    }
    let color_count = r.u16()?;
    let mut colors = Vec::with_capacity(color_count as usize);
    for _ in 0..color_count {
        let c = r.bytes(4)?;
        colors.push([c[0], c[1], c[2]]);
    }
    Some(Controller { index, kind, name, zones, colors })
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }
    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
    /// u16 length (counting the trailing NUL), then the bytes.
    fn string(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        let raw = self.bytes(len)?;
        let text = raw.strip_suffix(&[0]).unwrap_or(raw);
        Some(String::from_utf8_lossy(text).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&((text.len() + 1) as u16).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }

    /// A description as OpenRGB writes it: one mode, these zones (name, type,
    /// min, max, count), and a matrix on the first zone when `matrix`.
    pub(crate) fn description(name: &str, kind: i32, zones: &[(&str, i32, u32, u32, u32)], protocol: u32, matrix: bool) -> Vec<u8> {
        let mut out = vec![0u8; 4];
        out.extend_from_slice(&kind.to_le_bytes());
        string(&mut out, name);
        if protocol >= 1 {
            string(&mut out, "Gigabyte");
        }
        for text in ["RGB Fusion 2", "1.0", "", "HID: 1"] {
            string(&mut out, text);
        }
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        string(&mut out, "Direct");
        let fields = if protocol >= 3 { 12 } else { 9 };
        for i in 0..fields {
            out.extend_from_slice(&(i as u32).to_le_bytes());
        }
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        out.extend_from_slice(&(zones.len() as u16).to_le_bytes());
        let mut total = 0;
        for (i, (zone, zkind, min, max, count)) in zones.iter().enumerate() {
            string(&mut out, zone);
            out.extend_from_slice(&zkind.to_le_bytes());
            for v in [*min, *max, *count] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            if matrix && i == 0 {
                let (h, w) = (2u32, 3u32);
                out.extend_from_slice(&((h * w * 4 + 8) as u16).to_le_bytes());
                out.extend_from_slice(&h.to_le_bytes());
                out.extend_from_slice(&w.to_le_bytes());
                for cell in 0..h * w {
                    out.extend_from_slice(&cell.to_le_bytes());
                }
            } else {
                out.extend_from_slice(&0u16.to_le_bytes());
            }
            total += count;
        }
        out.extend_from_slice(&(total as u16).to_le_bytes());
        for i in 0..total {
            string(&mut out, &format!("LED {i}"));
            out.extend_from_slice(&i.to_le_bytes());
        }
        out.extend_from_slice(&(total as u16).to_le_bytes());
        for i in 0..total {
            out.extend_from_slice(&[i as u8, 10, 20, 0]);
        }
        let size = out.len() as u32;
        out[..4].copy_from_slice(&size.to_le_bytes());
        out
    }

    #[test]
    fn reads_a_board_at_each_protocol_version() {
        let zones = [("ARGB_V2_1", 1, 0, 1024, 120), ("Motherboard", ZONE_SINGLE, 1, 1, 1)];
        for protocol in 0..=3 {
            for matrix in [false, true] {
                let data = description("B650 FORCE", TYPE_MOTHERBOARD, &zones, protocol, matrix);
                let c = parse_controller(2, &data, protocol).expect("parses");
                assert_eq!(c.index, 2);
                assert_eq!(c.name, "B650 FORCE");
                assert_eq!(c.kind, TYPE_MOTHERBOARD);
                assert_eq!(c.zones.len(), 2);
                assert_eq!(c.zones[0], Zone { name: "ARGB_V2_1".into(), kind: 1, leds_min: 0, leds_max: 1024, leds: 120 });
                assert!(c.zones[0].resizable() && !c.zones[1].resizable());
                assert_eq!(c.led_count(), 121);
                assert_eq!(c.colors.len(), 121);
                assert_eq!(c.colors[5], [5, 10, 20]);
            }
        }
    }

    #[test]
    fn a_cut_short_description_is_refused() {
        let data = description("B650", 0, &[("ARGB_V2_1", 1, 0, 1024, 10)], 3, false);
        for cut in [10, data.len() / 2, data.len() - 1] {
            assert_eq!(parse_controller(0, &data[..cut], 3), None, "cut at {cut}");
        }
    }

    #[test]
    fn led_packet_layout() {
        let body = leds_body(&[[255, 0, 10], [1, 2, 3]]);
        assert_eq!(body, vec![14, 0, 0, 0, 2, 0, 255, 0, 10, 0, 1, 2, 3, 0]);
    }
}
