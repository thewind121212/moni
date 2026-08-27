#!/usr/bin/env python3
"""Stream compact Apple Silicon telemetry to an ESP32 over USB serial."""

from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Any, Iterator

import psutil
import serial


PROTOCOL_VERSION = 1
DEFAULT_PORT_GLOB = "/dev/cu.usbserial-*"


def clamp_percent(value: float | int | None) -> float:
    return round(max(0.0, min(100.0, float(value or 0.0))), 1)


def ratio_percent(value: float | int | None) -> float:
    return clamp_percent(float(value or 0.0) * 100.0)


def rounded(value: float | int | None, digits: int = 1) -> float:
    return round(float(value or 0.0), digits)


@dataclass
class RateState:
    at: float
    rx: int
    tx: int
    disk_read: int
    disk_write: int


def rate_state() -> RateState:
    net = psutil.net_io_counters()
    disk = psutil.disk_io_counters()
    return RateState(
        at=time.monotonic(),
        rx=net.bytes_recv,
        tx=net.bytes_sent,
        disk_read=disk.read_bytes if disk else 0,
        disk_write=disk.write_bytes if disk else 0,
    )


def rates(previous: RateState) -> tuple[dict[str, float], RateState]:
    current = rate_state()
    elapsed = max(current.at - previous.at, 0.001)
    mib = 1024 * 1024
    values = {
        "nr": rounded((current.rx - previous.rx) / elapsed / mib, 2),
        "nt": rounded((current.tx - previous.tx) / elapsed / mib, 2),
        "dr": rounded((current.disk_read - previous.disk_read) / elapsed / mib, 2),
        "dw": rounded((current.disk_write - previous.disk_write) / elapsed / mib, 2),
    }
    return values, current


def macmon_samples(interval_ms: int) -> Iterator[dict[str, Any]]:
    executable = shutil.which("macmon")
    if not executable and os.path.isfile("/opt/homebrew/bin/macmon"):
        executable = "/opt/homebrew/bin/macmon"
    if not executable:
        raise RuntimeError("macmon is not installed; run: brew install macmon")

    process = subprocess.Popen(
        [executable, "pipe", "-i", str(interval_ms)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    assert process.stdout is not None
    try:
        for line in process.stdout:
            line = line.strip()
            if line:
                yield json.loads(line)
    finally:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()


def compact_sample(raw: dict[str, Any], previous: RateState) -> tuple[dict[str, Any], RateState]:
    rate_values, current = rates(previous)
    memory = raw.get("memory", {})
    temp = raw.get("temp", {})
    fan_list = raw.get("fans", [])
    fan = fan_list[0].get("rpm", 0) if fan_list else 0
    disk = psutil.disk_usage("/")
    ram_total = max(int(memory.get("ram_total", 0)), 1)
    swap_total = max(int(memory.get("swap_total", 0)), 1)

    payload: dict[str, Any] = {
        "v": PROTOCOL_VERSION,
        "t": int(time.time()),
        "os": "mac",
        "cores": psutil.cpu_count(logical=False) or 0,
        "threads": psutil.cpu_count(logical=True) or 0,
        "ramgb": rounded(psutil.virtual_memory().total / (1024**3), 1),
        "cpu": clamp_percent(psutil.cpu_percent(interval=None)),
        "pc": ratio_percent(raw.get("pcpu_scaled_ratio")),
        "ec": ratio_percent(raw.get("ecpu_scaled_ratio")),
        "gpu": ratio_percent(raw.get("gpu_scaled_ratio")),
        "ram": clamp_percent(int(memory.get("ram_usage", 0)) * 100 / ram_total),
        "swap": clamp_percent(int(memory.get("swap_usage", 0)) * 100 / swap_total),
        "ct": rounded(temp.get("cpu_temp_avg")),
        "gt": rounded(temp.get("gpu_temp_avg")),
        "pf": int(raw.get("pcpu_freq_mhz", 0)),
        "ef": int(raw.get("ecpu_freq_mhz", 0)),
        "gf": int(raw.get("gpu_freq_mhz", 0)),
        "fan": int(fan),
        "sysw": rounded(raw.get("sys_power"), 2),
        "cpuw": rounded(raw.get("cpu_power"), 2),
        "gpuw": rounded(raw.get("gpu_power"), 2),
        "disk": rounded(disk.percent),
        "free": rounded(disk.free / (1024**3)),
        "load": rounded(os.getloadavg()[0], 2),
        "up": int(time.time() - psutil.boot_time()),
        **rate_values,
    }
    return payload, current


def metadata() -> dict[str, Any]:
    return {
        "v": PROTOCOL_VERSION,
        "type": "hello",
        "host": platform.node().split(".")[0],
        "chip": "Apple M2 Pro",
        "cores": psutil.cpu_count(logical=True),
        "ram_gb": round(psutil.virtual_memory().total / (1024**3)),
    }


def resolve_port(explicit: str | None) -> str | None:
    if explicit:
        return explicit
    ports = sorted(glob.glob(DEFAULT_PORT_GLOB))
    return ports[0] if ports else None


def open_serial(port: str, baud: int) -> serial.Serial:
    # Configure modem-control lines before opening. Opening a CH340 port with
    # pyserial's defaults briefly asserts DTR/RTS, which resets ESP32 boards.
    connection = serial.Serial(port=None, baudrate=baud, timeout=0, write_timeout=5)
    connection.dtr = False
    connection.rts = False
    connection.port = port
    connection.open()
    time.sleep(0.1)
    connection.reset_input_buffer()
    connection.write((json.dumps(metadata(), separators=(",", ":")) + "\n").encode())
    return connection


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", help="serial port; auto-detected by default")
    parser.add_argument("--udp", default="",
                        help="stream over WiFi instead of USB: device IP or IP:port (default port 5005)")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--interval", type=int, default=1000, help="sample interval in milliseconds")
    parser.add_argument("--preview", action="store_true", help="print JSON instead of opening serial")
    parser.add_argument("--samples", type=int, default=0, help="stop after N samples (0 = forever)")
    args = parser.parse_args()

    connection: serial.Serial | None = None
    previous = rate_state()
    psutil.cpu_percent(interval=None)
    sent = 0

    udp_target: tuple[str, int] | None = None
    udp_socket: socket.socket | None = None
    if args.udp:
        host_part, _, port_part = args.udp.partition(":")
        udp_target = (host_part, int(port_part) if port_part else 5005)
        udp_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        print(f"Streaming UDP telemetry to {udp_target[0]}:{udp_target[1]}", file=sys.stderr)

    try:
        for raw in macmon_samples(args.interval):
            payload, previous = compact_sample(raw, previous)
            line = json.dumps(payload, separators=(",", ":"))

            if args.preview:
                print(line, flush=True)
            elif udp_socket is not None and udp_target is not None:
                try:
                    # Re-announce periodically so a rebooted display gets metadata.
                    if sent % 60 == 0:
                        hello = json.dumps(metadata(), separators=(",", ":"))
                        udp_socket.sendto((hello + "\n").encode(), udp_target)
                    udp_socket.sendto((line + "\n").encode(), udp_target)
                except OSError as error:
                    print(f"UDP send failed: {error}", file=sys.stderr)
            else:
                if connection is None or not connection.is_open:
                    port = resolve_port(args.port)
                    if not port:
                        print("Waiting for ESP32 serial port...", file=sys.stderr)
                        time.sleep(1)
                        continue
                    try:
                        connection = open_serial(port, args.baud)
                        print(f"Connected to {port} at {args.baud} baud", file=sys.stderr)
                    except serial.SerialException as error:
                        print(f"Serial unavailable: {error}", file=sys.stderr)
                        connection = None
                        time.sleep(1)
                        continue

                try:
                    connection.write((line + "\n").encode())
                except (serial.SerialException, serial.SerialTimeoutException) as error:
                    print(f"Serial disconnected: {error}", file=sys.stderr)
                    connection.close()
                    connection = None
                    continue

            sent += 1
            if args.samples and sent >= args.samples:
                break
    except KeyboardInterrupt:
        pass
    finally:
        if connection is not None:
            connection.close()
        if udp_socket is not None:
            udp_socket.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
