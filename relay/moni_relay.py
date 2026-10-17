#!/usr/bin/env python3
"""moni-relay: expose the Windows PC's Moni telemetry to the home page.

The Moni service on the gaming PC broadcasts one JSON line per second on UDP
5005 for the clock. This relay listens to the same broadcasts (it never sends
anything, so the clock is unaffected) and serves the latest reading over HTTP:

  GET /status   flat, display-ready JSON for gethomepage's customapi widget
  GET /raw      the last packet as received, plus age and sender

No packet for OFFLINE_AFTER seconds means the PC is off or asleep.
"""

import json
import os
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

lock = threading.Lock()
latest = {"packet": None, "at": 0.0, "sender": "", "host": "", "last_seen": 0.0}


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


def status() -> dict:
    with lock:
        packet, at, sender, host, last_seen = (
            latest["packet"], latest["at"], latest["sender"], latest["host"], latest["last_seen"])
    age = time.monotonic() - at if packet else None
    online = packet is not None and age is not None and age < OFFLINE_AFTER
    if not online:
        ago = time.strftime("%H:%M", time.localtime(last_seen)) if last_seen else "never"
        return {"status": "Offline", "online": False, "cpu": "–", "gpu": "–", "ram": "–",
                "power": "–", "temps": "–", "fan": "–", "net": "–", "uptime": "–",
                "last_seen": ago, "host": host, "sender": sender}
    p = packet
    sysw = num(p, "sysw")
    power = f"{sysw:.0f} W" if sysw > 0 else "–"
    if p.get("pwrmode") == "parts" and sysw > 0:
        power += " (CPU+GPU)"
    return {
        "status": "Online",
        "online": True,
        "cpu": f"{num(p, 'cpu'):.0f}% · {temp(num(p, 'ct'))}",
        "gpu": f"{num(p, 'gpu'):.0f}% · {temp(num(p, 'gt'))}",
        "ram": f"{num(p, 'ram'):.0f}% of {num(p, 'ramgb'):.0f} GB",
        "power": power,
        "temps": f"board {temp(num(p, 'mt'))} · SSD {temp(num(p, 'st'))}",
        "fan": f"{num(p, 'fan'):.0f} rpm" if num(p, "fan") > 0 else "–",
        "net": f"↓{num(p, 'nr'):.1f} ↑{num(p, 'nt'):.1f} MB/s",
        "uptime": uptime(num(p, "up")),
        "last_seen": "now",
        "host": host,
        "sender": sender,
    }


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (http.server API)
        path = self.path.split("?", 1)[0].rstrip("/")
        if path in ("", "/status"):
            body = status()
        elif path == "/raw":
            with lock:
                age = time.monotonic() - latest["at"] if latest["packet"] else None
                body = {"packet": latest["packet"], "age": age, "sender": latest["sender"], "host": latest["host"]}
        else:
            self.send_error(404)
            return
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(200)
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
