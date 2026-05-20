#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v xvfb-run >/dev/null 2>&1; then
  echo "xvfb-run is required for headless GUI smoke tests" >&2
  exit 127
fi

export BTRFS_MANAGER_DEV_LOCAL_HELPER=1
export G_MESSAGES_DEBUG=none
export GDK_BACKEND=x11
export GTK_A11Y=none

cargo build -p btrfs-manager-app --features gui

timeout 20s xvfb-run -a target/debug/btrfs-manager-app --check-gui
