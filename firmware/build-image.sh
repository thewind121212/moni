#!/usr/bin/env bash
# Build the firmware and merge bootloader + partitions + app into the single
# image that host\update-firmware-windows.cmd and the Moni app flash at 0x0.
# Flashing PlatformIO's bare firmware.bin at 0x0 would overwrite the bootloader.
#   PIO=/path/to/pio ESPTOOL=/path/to/esptool ./firmware/build-image.sh
set -euo pipefail
cd "$(dirname "$0")"
PIO=${PIO:-pio}
ESPTOOL=${ESPTOOL:-esptool}
"$PIO" run
BUILD=.pio/build/si_hai
CORE=${PLATFORMIO_CORE_DIR:-$HOME/.platformio}
BOOT_APP0=$(find "$CORE/packages/framework-arduinoespressif32/tools/partitions" -name boot_app0.bin | head -1)
"$ESPTOOL" --chip esp32 merge-bin -o moni-si-hai.bin --flash-mode dio --flash-size 4MB \
  0x1000 "$BUILD/bootloader.bin" \
  0x8000 "$BUILD/partitions.bin" \
  0xe000 "$BOOT_APP0" \
  0x10000 "$BUILD/firmware.bin"
ls -l moni-si-hai.bin
