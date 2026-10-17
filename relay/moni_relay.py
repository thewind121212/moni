#!/usr/bin/env python3
"""moni-relay: expose the Windows PC's Moni telemetry to the home page.

The Moni service on the gaming PC broadcasts one JSON line per second on UDP
5005 for the clock. This relay listens to the same broadcasts (it never sends
anything, so the clock is unaffected) and serves the latest reading over HTTP:

  GET  /status  flat, display-ready JSON (home page cards)
  GET  /raw     the last packet as received, plus age and sender
  GET  /health  200 while the PC reports, 503 when it is off/asleep
  POST /wake    send a Wake-on-LAN magic packet to the PC; needs the header
                "Authorization: Bearer <MONI_WAKE_TOKEN>" (wake is disabled
                when no token is configured)

No packet for OFFLINE_AFTER seconds means the PC is off or asleep.
"""

import hmac
import json
import os
import re
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UDP_PORT = int(os.environ.get("MONI_UDP_PORT", "5005"))
HTTP_PORT = int(os.environ.get("MONI_HTTP_PORT", "5007"))
HTTP_BIND = os.environ.get("MONI_HTTP_BIND", "0.0.0.0")
OFFLINE_AFTER = float(os.environ.get("MONI_OFFLINE_AFTER", "10"))
# Only the Windows sender by default; the Mac sender reports os "mac".
WANT_OS = os.environ.get("MONI_OS", "win")
WAKE_MAC = os.environ.get("MONI_WAKE_MAC", "")
WAKE_TOKEN = os.environ.get("MONI_WAKE_TOKEN", "")
# Comma-separated broadcast addresses for the magic packet.
WAKE_TARGETS = [t.strip() for t in os.environ.get("MONI_WAKE_BROADCAST", "255.255.255.255").split(",") if t.strip()]
WAKING_FOR = 180.0  # seconds to show "Waking…" after a wake request

lock = threading.Lock()
latest = {"packet": None, "at": 0.0, "sender": "", "host": "", "last_seen": 0.0, "woken_at": 0.0}


def listen() -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("0.0.0.0", UDP_PORT))
    while True:
        data, (addr, _) = sock.recvfrom(4096)
        try:
            packet = json.loads(data.decode("utf-8", "replace"))
        except ValueError:
            continue
        if not isinstance(packet, dict):
            continue
        with lock:
            if packet.get("type") == "hello":
                latest["host"] = str(packet.get("host", ""))[:64]
                continue
            if packet.get("v") != 1 or (WANT_OS and packet.get("os") != WANT_OS):
                continue
            latest.update(packet=packet, at=time.monotonic(), sender=addr, last_seen=time.time())


def num(packet: dict, key: str) -> float:
    try:
        return float(packet.get(key) or 0)
    except (TypeError, ValueError):
        return 0.0


def temp(value: float) -> str:
    return f"{value:.0f}°C" if value > 0 else "–"


def uptime(seconds: float) -> str:
    seconds = int(seconds)
    days, hours, minutes = seconds // 86400, seconds % 86400 // 3600, seconds % 3600 // 60
    return f"{days}d {hours}h" if days else f"{hours}h {minutes}m"


def is_online() -> bool:
    with lock:
        return latest["packet"] is not None and time.monotonic() - latest["at"] < OFFLINE_AFTER


def magic_packet(mac: str) -> bytes:
    digits = re.sub(r"[^0-9a-fA-F]", "", mac)
    if len(digits) != 12:
        raise ValueError("MONI_WAKE_MAC must be a 6-byte MAC address")
    return b"\xff" * 6 + bytes.fromhex(digits) * 16


def wake() -> None:
    packet = magic_packet(WAKE_MAC)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try:
        # Ports 9 and 7 are the usual WoL ports; NICs match the payload, not the port.
        for target in WAKE_TARGETS:
            for port in (9, 7):
                sock.sendto(packet, (target, port))
    finally:
        sock.close()
    with lock:
        latest["woken_at"] = time.monotonic()


def status() -> dict:
    with lock:
        packet, at, sender, host, last_seen, woken_at = (
            latest["packet"], latest["at"], latest["sender"], latest["host"],
            latest["last_seen"], latest["woken_at"])
    age = time.monotonic() - at if packet else None
    online = packet is not None and age is not None and age < OFFLINE_AFTER
    if not online:
        ago = time.strftime("%H:%M", time.localtime(last_seen)) if last_seen else "never"
        waking = woken_at and time.monotonic() - woken_at < WAKING_FOR
        return {"status": "Waking…" if waking else "Offline", "online": False, "waking": bool(waking),
                "can_wake": bool(WAKE_MAC and WAKE_TOKEN),
                "cpu": "–", "gpu": "–", "ram": "–", "power": "–", "temps": "–", "board": "–",
                "ssd": "–", "fan": "–", "net": "–", "uptime": "–",
                "last_seen": ago, "host": host, "sender": sender}
    p = packet
    sysw = num(p, "sysw")
    power = f"{sysw:.0f} W" if sysw > 0 else "–"
    if p.get("pwrmode") == "parts" and sysw > 0:
        power += " (CPU+GPU)"
    return {
        "status": "Online",
        "online": True,
        "waking": False,
        "can_wake": bool(WAKE_MAC and WAKE_TOKEN),
        "cpu": f"{num(p, 'cpu'):.0f}% · {temp(num(p, 'ct'))}",
        "gpu": f"{num(p, 'gpu'):.0f}% · {temp(num(p, 'gt'))}",
        "ram": f"{num(p, 'ram'):.0f}% of {num(p, 'ramgb'):.0f} GB",
        "power": power,
        "temps": f"board {temp(num(p, 'mt'))} · SSD {temp(num(p, 'st'))}",
        "board": temp(num(p, "mt")),
        "ssd": temp(num(p, "st")),
        "fan": f"{num(p, 'fan'):.0f} rpm" if num(p, "fan") > 0 else "–",
        "net": f"↓{num(p, 'nr'):.1f} ↑{num(p, 'nt'):.1f} MB/s",
        "uptime": uptime(num(p, "up")),
        "last_seen": "now",
        "host": host,
        "sender": sender,
        # numbers for apps that draw their own bars / units (home-lofi)
        "n": {k: num(p, k) for k in ("cpu", "ct", "gpu", "gt", "mt", "st", "ram", "ramgb", "vram", "vramgb",
                                      "sysw", "cpuw", "gpuw", "fan", "up", "pf", "gf")},
        "parts": p.get("pwrmode") == "parts",
    }


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (http.server API)
        path = self.path.split("?", 1)[0].rstrip("/")
        if path in ("", "/status"):
            body = status()
        elif path == "/health":
            online = is_online()
            self.reply(200 if online else 503, {"online": online})
            return
        elif path == "/raw":
            with lock:
                age = time.monotonic() - latest["at"] if latest["packet"] else None
                body = {"packet": latest["packet"], "age": age, "sender": latest["sender"], "host": latest["host"]}
        else:
            self.send_error(404)
            return
        self.reply(200, body)

    def do_POST(self) -> None:  # noqa: N802
        if self.path.split("?", 1)[0].rstrip("/") != "/wake":
            self.send_error(404)
            return
        if not (WAKE_MAC and WAKE_TOKEN):
            self.reply(503, {"ok": False, "error": "wake is not configured"})
            return
        supplied = self.headers.get("Authorization", "").removeprefix("Bearer ").strip()
        if not hmac.compare_digest(supplied.encode(), WAKE_TOKEN.encode()):
            self.reply(401, {"ok": False, "error": "unauthorized"})
            return
        if is_online():
            self.reply(200, {"ok": True, "sent": False, "status": "already online"})
            return
        try:
            wake()
        except (OSError, ValueError) as error:
            self.reply(500, {"ok": False, "error": str(error)})
            return
        print(f"moni-relay: wake packet sent to {WAKE_MAC}", flush=True)
        self.reply(200, {"ok": True, "sent": True, "status": "Waking…"})

    def reply(self, code: int, body: dict) -> None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args) -> None:  # homepage polls often; keep logs quiet
        pass


def main() -> None:
    threading.Thread(target=listen, daemon=True, name="udp").start()
    print(f"moni-relay: UDP {UDP_PORT} -> http://{HTTP_BIND}:{HTTP_PORT}/status", flush=True)
    ThreadingHTTPServer((HTTP_BIND, HTTP_PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
