# Moni — macOS and Windows ESP32 system display

Moni turns a SI HAI ESP32 five-screen clock into a real-time computer dashboard.

The Mac sender currently collects:

- total, performance-core, efficiency-core, and GPU utilization
- P-core, E-core, and GPU frequencies
- CPU and GPU temperatures
- RAM, swap, and disk usage
- network and disk throughput
- CPU, GPU, and system power
- fan RPM, load average, free disk space, and uptime

## Preview the telemetry

```sh
./host/run-preview.sh --samples 5
```

## Send it to the ESP32

```sh
./host/run-display.sh
```

The `firmware/` directory contains the SI HAI IPS five-panel firmware. Its
physical slots 1–5 are CPU, memory, GPU, I/O, and system power; slot 6 is empty.

The original firmware is not erased until the new image has compiled and the
connected board has passed a final identity check.

## Windows gaming PC

The Windows sender is designed for an AMD Ryzen CPU and NVIDIA RTX GPU. It
collects CPU load/clock/temperature/power, CPU-fan RPM, NVIDIA GPU
load/clock/temperature/power, actual installed RAM, disk usage/free space,
network and disk throughput, and uptime.

Open PowerShell in the project directory and run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\host\setup-windows.ps1
```

The setup creates an isolated Python environment, installs the NVIDIA NVML and
Windows monitoring packages, offers LibreHardwareMonitor through WinGet, and
adds Moni to the current user's Startup folder. Run LibreHardwareMonitor as
Administrator and leave it minimized so Ryzen temperature, package power, and
CPU-fan RPM are exposed through WMI.

Plug the clock into the Windows PC and start the live sender:

```bat
host\run-display-windows.cmd
```

The CH340 COM port is detected automatically. An explicit port is also
supported, for example `host\run-display-windows.cmd --port COM5`.

If the B650 motherboard exposes a different fan header as the first fan, list
its sensor names:

```bat
host\list-sensors-windows.cmd
```

Then select the CPU-fan sensor using any unique part of its name or identifier:

```bat
host\run-display-windows.cmd --fan-sensor "CPU Fan"
```

If WMI returns no motherboard sensors, open LibreHardwareMonitor and enable
`Options > Remote Web Server > Run`; Moni automatically falls back to its local
`data.json` sensor feed. Run LibreHardwareMonitor as Administrator for Super I/O
access on B650 motherboards.

On Windows the power panel prefers a hardware-reported total-system/PSU sensor.
Most desktop PSUs do not expose one, so the fallback is explicitly labeled
`CPU + GPU / NOT WALL`. If either package-power sensor is unavailable, Moni
shows `N/A` instead of presenting the remaining component as total PC power.

## Update the clock from Windows

The repository includes a prebuilt SI HAI firmware image. Close any visible
Moni sender window, plug in the clock, and run:

```bat
host\update-firmware-windows.cmd
```

The updater finds the CH340 COM port, pauses only the Moni sender, flashes the
image, and restarts telemetry automatically. PlatformIO is not required on the
Windows PC.

The same ESP32 firmware is host-aware: Windows shows physical cores/threads,
DDR5 capacity, and CPU clock, while macOS retains the Apple P/E-core and unified
memory presentation.
