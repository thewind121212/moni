#!/bin/zsh
set -euo pipefail

cd "${0:A:h}/.."
exec .esp-tools/bin/python3 host/monitor.py "$@"
