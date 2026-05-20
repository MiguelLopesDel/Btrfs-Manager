#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GUI_FILE="$ROOT/crates/app/src/gui.rs"

if rg -n '"pkexec"|Command::new\("pkexec"\)' "$GUI_FILE"; then
  echo "error: GUI must not use pkexec. Use the system D-Bus helper service." >&2
  exit 1
fi

if rg -n 'Command::new\("(btrfs|mount|umount|systemctl)"\)' "$GUI_FILE"; then
  echo "error: GUI must not execute privileged system commands directly." >&2
  exit 1
fi

if rg -n 'btrfs-manager-helper|helper_binary_path|append_helper_cli_args' "$GUI_FILE"; then
  echo "error: GUI must not shell out to the helper CLI. Use D-Bus." >&2
  exit 1
fi

echo "GUI privilege boundary check passed."
