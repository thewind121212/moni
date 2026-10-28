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
physical slots 1–6 are CPU, memory, GPU, I/O, system power, and thermals
(CPU / GPU / motherboard / VRM temperatures).

The original firmware is not erased until the new image has compiled and the
connected board has passed a final identity check.

## Windows app (recommended)

`Moni_x.y.z_x64-setup.exe` installs everything the clock needs on a Windows PC;
no Python and no separate LibreHardwareMonitor window:

- **Moni service** starts at boot, before anyone signs in, and feeds the clock
  over USB and/or WiFi. Over WiFi it broadcasts on the local network, so the
  clock is found whatever IP the router gives it (or set a fixed address).
- **Sensor helper** uses LibreHardwareMonitor's library internally for Ryzen
  temperature/power, CPU fan, motherboard and SSD temperatures. The installer
  adds the signed PawnIO driver it needs.
- **Moni app** (tray icon) for settings: on/off, Auto/WiFi/USB, clock address,
  CPU fan sensor, update rate, open at sign-in, live readings, one-click
  clock firmware updates, and **Restart clock** (reboots the ESP32 over the
  USB cable's reset line, like unplugging it).
- **Screens come back by themselves:** the firmware re-sends the screens'
  start-up settings every 5 s (no visible change on a working screen), so a
  module that was re-seated or lost power for a moment wakes up again.
- **Fan lights** (off until switched on in the app): the service colors the
  motherboard's ARGB headers with CPU load through OpenRGB, 30 times a second.
  More load gives faster comets and a head further along the chosen colors
  (12 combos, default Blue & Purple). It replaces `cpu_rgb.py` and, unlike
  it, starts with Windows. It needs OpenRGB open with **SDK Server** started
  (port 6742); start OpenRGB at sign-in with `--server --startminimized`.
  Until OpenRGB answers, Moni tries again every 5 s and the app says why.
  Addressable zones are sized to *LEDs per header* (120; OpenRGB guesses 10).
  Stop `cpu_rgb.py` and OpenRGB's own effects first, or they fight and
  flicker. Code: `app/core/src/lights.rs` (effect) and `openrgb.rs` (SDK
  client).

Get the installer from the latest **Windows app** workflow run (Actions tab →
artifact `Moni-windows-x64`) or from a GitHub release, run it, and approve the
admin prompt. Windows SmartScreen warns about unsigned installers: choose
*More info → Run anyway*. The installer stops and removes the old Python
sender's startup shortcut.

The clock remembers its WiFi/USB mode across power cuts (firmware from this
version on). Press its mode button once to choose WiFi; from then on it rejoins
WiFi by itself whenever it powers up.

Settings and logs live in `%ProgramData%\Moni` (`config.json`, `status.json`,
`service.log`). For troubleshooting, from an admin terminal in the install
folder: `moni-service.exe sensors` lists every sensor, `moni-service.exe run`
runs the sender in the console.

Build it yourself on Windows (Rust, .NET 8 SDK, Node 22):

```powershell
dotnet publish app/sensors -c Release -o app/sensors/publish
# put PawnIO_setup.exe in app/src-tauri/vendor/ (see the workflow)
cd app; cargo build --release -p moni-service; npm ci; npx tauri build
```

## Home page status card

`relay/` runs on the home server and exposes the PC's telemetry to
[gethomepage](https://gethomepage.dev): it listens to the same UDP 5005
broadcasts as the clock (never sending, so the clock is unaffected) and serves
the latest reading at `http://<server>:5007/status`, or `Offline` after 10 s of
silence.

```sh
cd relay && docker compose up -d
```

Homepage `services.yaml`:

```yaml
- Gaming PC:
    icon: mdi-desktop-tower-monitor
    widget:
      type: customapi
      url: http://10.10.0.217:5007/status
      refreshInterval: 5000
      mappings:
        - { field: status, label: Status }
        - { field: cpu, label: CPU }
        - { field: gpu, label: GPU }
        - { field: power, label: Power }
```

Other fields: `ram`, `temps`, `board`, `ssd`, `fan`, `net`, `uptime`, `n` (raw
numbers); `/raw` returns the last packet, `/health` is 200 online / 503 offline.

**Wake-on-LAN:** `POST /wake` with `Authorization: Bearer <MONI_WAKE_TOKEN>`
sends the magic packet to `MONI_WAKE_MAC` (status shows `Waking…` until the
PC reports). Put the token in `relay/.env` (git-ignored):
`printf 'MONI_WAKE_TOKEN=%s\n' "$(openssl rand -hex 24)" > relay/.env`. The
home page (home-lofi) calls it from its server, behind the Authelia login.

**Turn off:** `POST /shutdown` (same header) asks the Moni service on the PC
to shut Windows down at once (`shutdown /s /f /t 0`: open apps close without
saving, since nobody is at the PC to answer them). The relay
signs the request with `MONI_PC_KEY` (HMAC-SHA256 over time and a one-off
nonce, valid 60 s, single use) and sends it to TCP 5008 on the PC, which the
installer opens to the local subnet only. Add the key to `relay/.env`
(`printf 'MONI_PC_KEY=%s\n' "$(openssl rand -hex 24)" >> relay/.env`, then
`docker compose up -d`) and paste the same value into the Moni app under
Settings → Turn off from the home page. Status shows `can_shutdown` once the
PC announces its listener (within a minute) and `Turning off…` after a request.

## Windows gaming PC (Python sender)

The Windows sender is designed for an AMD Ryzen CPU and NVIDIA RTX GPU. It
collects CPU load/clock/temperature/power, motherboard and VRM temperatures,
CPU-fan RPM, NVIDIA GPU load/clock/temperature/power, actual installed RAM,
disk usage/free space, network and disk throughput, and uptime.

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
access on B650 motherboards. Also enable LibreHardwareMonitor's `Run On Windows
Startup` and `Start Minimized` options. Moni retries WMI every three seconds, so
the two programs may start in either order.

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
