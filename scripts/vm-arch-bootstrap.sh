#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Btrfs Manager VM bootstrap for Arch Linux"

if ! command -v pacman >/dev/null 2>&1; then
  echo "error: this bootstrap script currently supports Arch Linux/pacman only." >&2
  exit 1
fi

echo "==> Installing system dependencies"
sudo pacman -Syu --needed \
  git \
  base-devel \
  rust \
  cargo \
  pkgconf \
  btrfs-progs \
  gtk4 \
  libadwaita \
  graphene \
  polkit \
  systemd \
  xdg-utils \
  dbus

echo "==> Optional boot integration packages"
if pacman -Si grub-btrfs >/dev/null 2>&1; then
  sudo pacman -S --needed grub-btrfs || true
else
  echo "warning: grub-btrfs not found in enabled pacman repositories; skipping."
fi

echo "==> Checking native GTK dependencies"
pkg-config --modversion gtk4
pkg-config --modversion libadwaita-1
pkg-config --modversion graphene-gobject-1.0

echo "==> Formatting code"
cargo fmt --all

echo "==> Running non-GUI tests"
cargo test --workspace --all-targets --no-default-features

echo "==> Checking GUI build"
cargo check -p btrfs-manager-app --features gui

echo "==> Building helper and app"
cargo build -p btrfs-manager-helper -p btrfs-manager-app --features btrfs-manager-app/gui

echo "==> Running app shell without GUI"
cargo run -p btrfs-manager-app --no-default-features

if [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
  echo "==> Starting GUI"
  cargo run -p btrfs-manager-app --features gui
else
  echo "==> No graphical session detected; skipping GUI launch."
  echo "Run this later inside the desktop session:"
  echo "    cargo run -p btrfs-manager-app --features gui"
fi

echo "==> Done"
