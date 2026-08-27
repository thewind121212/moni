#!/usr/bin/env python3
"""Stream Windows x86 hardware telemetry to the Moni ESP32 display."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import socket
import sys
import time
from urllib import error as urlerror
from urllib import request as urlrequest
from dataclasses import dataclass
from typing import Any

import psutil
import serial
from serial.tools import list_ports

try:
    import pynvml
except ImportError:  # Preview still works without NVIDIA support.
    pynvml = None

try:
    import wmi
except ImportError:  # CPU temperature/fan/power become optional.
    wmi = None


PROTOCOL_VERSION = 1
CH340_VIDS = {0x1A86, 0x1A2C}


def clamp_percent(value: float | int | None) -> float:
    return round(max(0.0, min(100.0, float(value or 0.0))), 1)


def rounded(value: float | int | None, digits: int = 1) -> float:
    return round(float(value or 0.0), digits)


def positive_rate(value: int, previous: int, elapsed: float) -> float:
    return max(0, value - previous) / elapsed / (1024 * 1024)


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
    values = {
        "nr": rounded(positive_rate(current.rx, previous.rx, elapsed), 2),
        "nt": rounded(positive_rate(current.tx, previous.tx, elapsed), 2),
        "dr": rounded(positive_rate(current.disk_read, previous.disk_read, elapsed), 2),
        "dw": rounded(positive_rate(current.disk_write, previous.disk_write, elapsed), 2),
    }
    return values, current


class NvidiaGpu:
    def __init__(self) -> None:
        self.handle: Any | None = None
        self.name = "NVIDIA GPU"
        self.error: str | None = None
        if pynvml is None:
            self.error = "nvidia-ml-py is not installed"
            return
        try:
            pynvml.nvmlInit()
            if pynvml.nvmlDeviceGetCount() < 1:
                self.error = "no NVIDIA GPU found"
                return
            self.handle = pynvml.nvmlDeviceGetHandleByIndex(0)
            name = pynvml.nvmlDeviceGetName(self.handle)
            self.name = name.decode(errors="replace") if isinstance(name, bytes) else str(name)
        except Exception as error:  # NVML errors vary with driver versions.
            self.error = str(error)
            self.handle = None

    def _read(self, function: Any, default: float = 0.0) -> float:
        if self.handle is None:
            return default
        try:
            return float(function())
        except Exception:
            return default

    def sample(self) -> dict[str, float]:
        if self.handle is None or pynvml is None:
            return {"gpu": 0, "gt": 0, "gf": 0, "gpuw": 0}
        try:
            utilization = pynvml.nvmlDeviceGetUtilizationRates(self.handle).gpu
        except Exception:
            utilization = 0
        return {
            "gpu": clamp_percent(utilization),
            "gt": rounded(self._read(lambda: pynvml.nvmlDeviceGetTemperature(
                self.handle, pynvml.NVML_TEMPERATURE_GPU))),
            "gf": int(self._read(lambda: pynvml.nvmlDeviceGetClockInfo(
                self.handle, pynvml.NVML_CLOCK_GRAPHICS))),
            "gpuw": rounded(self._read(
                lambda: pynvml.nvmlDeviceGetPowerUsage(self.handle)) / 1000.0, 2),
        }

    def close(self) -> None:
        if pynvml is not None and self.handle is not None:
            try:
                pynvml.nvmlShutdown()
            except Exception:
                pass


class LibreSensors:
    """Read CPU package temperature, power, and CPU-fan RPM from LHM WMI."""

    def __init__(self, fan_selector: str = "", rest_url: str = "http://127.0.0.1:8085") -> None:
        self.client: Any | None = None
        self.error: str | None = None
        self.fan_selector = fan_selector.casefold()
        self.rest_url = rest_url.rstrip("/")
        self.last_connect_attempt = 0.0
        if wmi is None:
            self.error = "WMI package is not installed"
            return
        self._connect_wmi(force=True)

    def _connect_wmi(self, force: bool = False) -> None:
        if wmi is None or self.client is not None:
            return
        now = time.monotonic()
        if not force and now - self.last_connect_attempt < 3.0:
            return
        self.last_connect_attempt = now
        try:
            self.client = wmi.WMI(namespace=r"root\LibreHardwareMonitor")
            rows = list(self.client.Sensor())
            if not rows:
                self.client = None
                self.error = "LibreHardwareMonitor WMI namespace has no sensors"
            else:
                self.error = None
        except Exception as error:
            self.client = None
            self.error = str(error)

    def _wmi_rows(self) -> list[dict[str, Any]]:
        self._connect_wmi()
        if self.client is not None:
            try:
                rows = [
                    {
                        "name": str(sensor.Name or ""),
                        "type": str(sensor.SensorType or ""),
                        "id": str(sensor.Identifier or ""),
                        "parent": str(getattr(sensor, "Parent", "") or ""),
                        "value": float(sensor.Value or 0),
                    }
                    for sensor in self.client.Sensor()
                ]
                if rows:
                    return rows
            except Exception as error:
                self.client = None
                self.error = str(error)
        return []

    @staticmethod
    def _number(value: Any) -> float:
        match = re.search(r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)", str(value or ""))
        return float(match.group(0)) if match else 0.0

    def _rest_rows(self) -> list[dict[str, Any]]:
        try:
            with urlrequest.urlopen(f"{self.rest_url}/data.json", timeout=0.35) as response:
                root = json.loads(response.read().decode("utf-8"))
        except (OSError, ValueError, urlerror.URLError):
            return []

        rows: list[dict[str, Any]] = []

        def visit(node: dict[str, Any], hardware: str = "") -> None:
            current_hardware = hardware
            if "SensorId" not in node and node.get("Text") not in (None, "Sensor"):
                current_hardware = str(node.get("Text", hardware))
            if "SensorId" in node:
                sensor_id = str(node.get("SensorId", ""))
                rows.append({
                    "name": str(node.get("Text", "")),
                    "type": str(node.get("Type", "")),
                    "id": sensor_id,
                    "parent": current_hardware,
                    "value": self._number(node.get("RawValue", node.get("Value", 0))),
                })
            for child in node.get("Children", []):
                if isinstance(child, dict):
                    visit(child, current_hardware)

        visit(root)
        return rows

    def rows(self) -> list[dict[str, Any]]:
        rows = self._wmi_rows()
        if rows:
            return rows
        rows = self._rest_rows()
        if rows:
            self.error = None
        return rows

    @staticmethod
    def _is_cpu(row: dict[str, Any]) -> bool:
        identity = (row["id"] + " " + row["parent"]).casefold()
        return "/amdcpu/" in identity or "/intelcpu/" in identity or "cpu" in identity

    @staticmethod
    def _best(rows: list[dict[str, Any]], score: Any) -> float:
        candidates = [(score(row), row["value"]) for row in rows]
        candidates = [candidate for candidate in candidates if candidate[0] >= 0]
        return max(candidates, default=(-1, 0))[1]

    def sample(self) -> dict[str, float | int]:
        rows = self.rows()
        temperatures = [row for row in rows if row["type"].casefold() == "temperature"]
        powers = [row for row in rows if row["type"].casefold() == "power"]
        fans = [row for row in rows if row["type"].casefold() == "fan" and row["value"] > 0]
        clocks = [row for row in rows if row["type"].casefold() == "clock" and row["value"] > 0]

        def temperature_score(row: dict[str, Any]) -> int:
            if not self._is_cpu(row):
                return -1
            name = row["name"].casefold()
            if "tctl" in name or "tdie" in name:
                return 100
            if "package" in name:
                return 95
            if "core max" in name:
                return 90
            if "core average" in name:
                return 80
            return 10

        def power_score(row: dict[str, Any]) -> int:
            if not self._is_cpu(row):
                return -1
            name = row["name"].casefold()
            return 100 if "package" in name else 10

        def fan_score(row: dict[str, Any]) -> int:
            text = (row["name"] + " " + row["id"] + " " + row["parent"]).casefold()
            if "/gpu" in text or "gpu fan" in text:
                return -1
            if self.fan_selector:
                return 200 if self.fan_selector in text else -1
            if "cpu_fan" in text or "cpu fan" in text:
                return 150
            if "cpu" in text:
                return 120
            if "fan #1" in text or "fan 1" in text or "/fan/0" in text:
                return 50
            return 1

        core_clocks = [
            row["value"] for row in clocks
            if self._is_cpu(row)
            and "bus" not in row["name"].casefold()
            and ("core" in row["name"].casefold() or "/clock/" in row["id"].casefold())
            and row["value"] > 200
        ]

        def total_power_score(row: dict[str, Any]) -> int:
            text = (row["name"] + " " + row["id"]).casefold()
            if "total system power" in text:
                return 200
            if "psu" in text and ("input" in text or "total" in text):
                return 150
            return -1

        return {
            "ct": rounded(self._best(temperatures, temperature_score)),
            "cpuw": rounded(self._best(powers, power_score), 2),
            "fan": int(self._best(fans, fan_score)),
            "pf": int(round(sum(core_clocks) / len(core_clocks))) if core_clocks else 0,
            "wallw": rounded(self._best(powers, total_power_score), 2),
        }

    def print_sensors(self) -> None:
        rows = self.rows()
        if not rows:
            print(f"LibreHardwareMonitor sensors unavailable: {self.error or 'no sensors'}")
            print("In LibreHardwareMonitor enable Options > Remote Web Server > Run for REST fallback.")
            return
        for row in rows:
            if row["type"].casefold() in {"temperature", "power", "fan", "clock"}:
                print(f"{row['type']:12} {row['value']:8.1f}  {row['name']:<28} {row['id']}")


class WindowsSampler:
    def __init__(self, fan_selector: str = "", rest_url: str = "http://127.0.0.1:8085") -> None:
        self.gpu = NvidiaGpu()
        self.sensors = LibreSensors(fan_selector, rest_url)
        self.previous = rate_state()
        psutil.cpu_percent(interval=None)

    def sample(self) -> dict[str, Any]:
        rate_values, self.previous = rates(self.previous)
        cpu = clamp_percent(psutil.cpu_percent(interval=None))
        memory = psutil.virtual_memory()
        swap = psutil.swap_memory()
        cpu_frequency = psutil.cpu_freq()
        system_drive = os.environ.get("SystemDrive", "C:") + "\\"
        disk = psutil.disk_usage(system_drive)
        gpu = self.gpu.sample()
        hardware = self.sensors.sample()
        cpu_w = float(hardware["cpuw"])
        gpu_w = float(gpu["gpuw"])
        measured_total_w = float(hardware["wallw"])
        component_w = cpu_w + gpu_w if cpu_w > 0 and gpu_w > 0 else 0
        system_w = measured_total_w if measured_total_w > 0 else component_w
        power_mode = "total" if measured_total_w > 0 else "parts" if component_w > 0 else "none"
        live_clock = int(hardware["pf"])

        return {
            "v": PROTOCOL_VERSION,
            "t": int(time.time()),
            "os": "win",
            "cores": psutil.cpu_count(logical=False) or 0,
            "threads": psutil.cpu_count(logical=True) or 0,
            "ramgb": rounded(memory.total / (1024**3), 1),
            "cpu": cpu,
            "pc": cpu,
            "ec": 0,
            "ram": clamp_percent(memory.percent),
            "swap": clamp_percent(swap.percent),
            "ct": hardware["ct"],
            "pf": live_clock or int(cpu_frequency.current if cpu_frequency else 0),
            "ef": 0,
            "fan": hardware["fan"],
            "cpuw": rounded(cpu_w, 2),
            "sysw": rounded(system_w, 2),
            "pwrmode": power_mode,
            "disk": rounded(disk.percent),
            "free": rounded(disk.free / (1024**3)),
            "load": rounded(cpu * (psutil.cpu_count() or 1) / 100.0, 2),
            "up": int(time.time() - psutil.boot_time()),
            **gpu,
            **rate_values,
        }

    def metadata(self) -> dict[str, Any]:
        return {
            "v": PROTOCOL_VERSION,
            "type": "hello",
            "host": platform.node(),
            "chip": platform.processor() or "AMD Ryzen",
            "gpu_name": self.gpu.name,
            "cores": psutil.cpu_count(logical=True),
            "ram_gb": round(psutil.virtual_memory().total / (1024**3)),
        }

    def close(self) -> None:
        self.gpu.close()


def resolve_port(explicit: str | None) -> str | None:
    if explicit:
        return explicit
    ranked: list[tuple[int, str]] = []
    for port in list_ports.comports():
        description = f"{port.description} {port.manufacturer or ''}".casefold()
        score = 0
        if port.vid in CH340_VIDS or port.pid == 0x7523:
            score += 100
        if any(label in description for label in ("ch340", "usb-serial", "usb serial", "cp210")):
            score += 50
        if port.device.upper().startswith("COM"):
            score += 1
        ranked.append((score, port.device))
    ranked.sort(reverse=True)
    if ranked and (ranked[0][0] > 1 or len(ranked) == 1):
        return ranked[0][1]
    return None


def open_serial(port: str, baud: int, metadata: dict[str, Any]) -> serial.Serial:
    connection = serial.Serial(port=None, baudrate=baud, timeout=0, write_timeout=5)
    connection.dtr = False
    connection.rts = False
    connection.port = port
    connection.open()
    time.sleep(0.1)
    connection.reset_input_buffer()
    connection.write((json.dumps(metadata, separators=(",", ":")) + "\n").encode())
    return connection


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", help="COM port; the CH340 is auto-detected by default")
    parser.add_argument("--udp", default="",
                        help="stream over WiFi instead of USB: device IP or IP:port (default port 5005)")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--interval", type=int, default=1000, help="sample interval in milliseconds")
    parser.add_argument("--preview", action="store_true", help="print JSON instead of opening serial")
    parser.add_argument("--samples", type=int, default=0, help="stop after N samples (0 = forever)")
    parser.add_argument("--list-sensors", action="store_true", help="list LHM temperature, power, and fan sensors")
    parser.add_argument("--fan-sensor", default="", help="substring selecting the CPU-fan sensor name or ID")
    parser.add_argument("--lhm-url", default="http://127.0.0.1:8085", help="LibreHardwareMonitor REST URL fallback")
    args = parser.parse_args()

    sampler = WindowsSampler(args.fan_sensor, args.lhm_url)
    if args.list_sensors:
        print(f"NVIDIA: {sampler.gpu.name}" + (f" ({sampler.gpu.error})" if sampler.gpu.error else ""))
        sampler.sensors.print_sensors()
        sampler.close()
        return 0

    if sampler.gpu.error:
        print(f"NVIDIA telemetry unavailable: {sampler.gpu.error}", file=sys.stderr)
    if sampler.sensors.error:
        print("CPU temperature/power/fan unavailable. Start LibreHardwareMonitor as Administrator.", file=sys.stderr)

    udp_target: tuple[str, int] | None = None
    udp_socket: socket.socket | None = None
    if args.udp:
        host_part, _, port_part = args.udp.partition(":")
        udp_target = (host_part, int(port_part) if port_part else 5005)
        udp_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        print(f"Streaming UDP telemetry to {udp_target[0]}:{udp_target[1]}", file=sys.stderr)

    connection: serial.Serial | None = None
    sent = 0
    try:
        while not args.samples or sent < args.samples:
            started = time.monotonic()
            payload = sampler.sample()
            line = json.dumps(payload, separators=(",", ":"))
            if args.preview:
                print(line, flush=True)
            elif udp_socket is not None and udp_target is not None:
                try:
                    # Re-announce periodically so a rebooted display gets metadata.
                    if sent % 60 == 0:
                        hello = json.dumps(sampler.metadata(), separators=(",", ":"))
                        udp_socket.sendto((hello + "\n").encode(), udp_target)
                    udp_socket.sendto((line + "\n").encode(), udp_target)
                except OSError as error:
                    print(f"UDP send failed: {error}", file=sys.stderr)
            else:
                if connection is None or not connection.is_open:
                    port = resolve_port(args.port)
                    if port:
                        try:
                            connection = open_serial(port, args.baud, sampler.metadata())
                            print(f"Connected to {port} at {args.baud} baud", file=sys.stderr)
                        except serial.SerialException as error:
                            print(f"Serial unavailable: {error}", file=sys.stderr)
                            connection = None
                    else:
                        print("Waiting for the ESP32 COM port...", file=sys.stderr)
                if connection is not None:
                    try:
                        connection.write((line + "\n").encode())
                    except (serial.SerialException, serial.SerialTimeoutException) as error:
                        print(f"Serial disconnected: {error}", file=sys.stderr)
                        connection.close()
                        connection = None
            sent += 1
            remaining = args.interval / 1000.0 - (time.monotonic() - started)
            if remaining > 0:
                time.sleep(remaining)
    except KeyboardInterrupt:
        pass
    finally:
        if connection is not None:
            connection.close()
        if udp_socket is not None:
            udp_socket.close()
        sampler.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
