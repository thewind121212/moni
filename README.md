# Moni — macOS ESP32 system display

Moni turns an ESP32-connected LCD into a real-time Apple Silicon dashboard.

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
