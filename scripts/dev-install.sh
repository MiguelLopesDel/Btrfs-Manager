#!/usr/bin/env bash
# Instala btrfs-manager do repo local no sistema host para testes de D-Bus/Polkit.
# Equivale ao que o PKGBUILD faz, mas sem precisar publicar no AUR.
#
# Uso (como usuário normal — sudo é pedido apenas para o install):
#   bash scripts/dev-install.sh          # build debug (rápido)
#   bash scripts/dev-install.sh release  # build release
#   sudo bash scripts/dev-install.sh remove   # desinstala

set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-debug}"

HELPER_BIN="target/debug/btrfs-manager-helper"
APP_BIN="target/debug/btrfs-manager-app"

fail() { echo "error: $*" >&2; exit 1; }

if [ "$MODE" = "remove" ]; then
  [ "$(id -u)" -ne 0 ] && fail "rode com sudo para remover: sudo bash scripts/dev-install.sh remove"
  echo "==> Desinstalando"
  systemctl stop btrfs-manager-helper.service 2>/dev/null || true
  systemctl disable btrfs-manager-helper.service 2>/dev/null || true
  rm -f /usr/bin/btrfs-manager-app
  rm -f /usr/lib/btrfs-manager/btrfs-manager-helper
  rmdir --ignore-fail-on-non-empty /usr/lib/btrfs-manager 2>/dev/null || true
  rm -f /usr/share/dbus-1/system.d/org.btrfsmanager.Helper.conf
  rm -f /usr/share/dbus-1/system-services/org.btrfsmanager.Helper.service
  rm -f /usr/share/polkit-1/actions/org.btrfsmanager.helper.policy
  rm -f /usr/lib/systemd/system/btrfs-manager-helper.service
  rm -f /usr/lib/systemd/system/btrfs-manager-snapshot@.service
  rm -f /usr/lib/systemd/system/btrfs-manager-snapshot@.timer
  systemctl daemon-reload
  echo "==> Desinstalado."
  exit 0
fi

# Build roda como usuário normal (cargo está no PATH do usuário, não do root)
[ "$(id -u)" -eq 0 ] && \
  fail "não rode o build como root — rode como usuário normal: bash scripts/dev-install.sh"

command -v cargo >/dev/null 2>&1 || fail "cargo não encontrado no PATH"

if [ "$MODE" = "release" ]; then
  echo "==> Build release"
  cargo build --release \
    -p btrfs-manager-helper \
    -p btrfs-manager-app \
    --features btrfs-manager-app/gui
  HELPER_BIN="target/release/btrfs-manager-helper"
  APP_BIN="target/release/btrfs-manager-app"
else
  echo "==> Build debug"
  cargo build \
    -p btrfs-manager-helper \
    -p btrfs-manager-app \
    --features btrfs-manager-app/gui
fi

echo "==> Instalando (requer sudo)"
sudo install -Dm755 "$APP_BIN"    /usr/bin/btrfs-manager-app
sudo install -Dm755 "$HELPER_BIN" /usr/lib/btrfs-manager/btrfs-manager-helper
sudo install -Dm644 packaging/dbus/org.btrfsmanager.Helper.conf    /usr/share/dbus-1/system.d/org.btrfsmanager.Helper.conf
sudo install -Dm644 packaging/dbus/org.btrfsmanager.Helper.service /usr/share/dbus-1/system-services/org.btrfsmanager.Helper.service
sudo install -Dm644 packaging/polkit/org.btrfsmanager.helper.policy /usr/share/polkit-1/actions/org.btrfsmanager.helper.policy
sudo install -Dm644 packaging/systemd/btrfs-manager-helper.service  /usr/lib/systemd/system/btrfs-manager-helper.service
sudo install -Dm644 packaging/systemd/btrfs-manager-snapshot@.service /usr/lib/systemd/system/btrfs-manager-snapshot@.service
sudo install -Dm644 packaging/systemd/btrfs-manager-snapshot@.timer   /usr/lib/systemd/system/btrfs-manager-snapshot@.timer

echo "==> Reiniciando serviço D-Bus"
sudo systemctl daemon-reload
sudo systemctl restart btrfs-manager-helper.service

echo ""
echo "==> Instalação concluída."
echo "    helper  : /usr/lib/btrfs-manager/btrfs-manager-helper"
echo "    app     : /usr/bin/btrfs-manager-app"
echo "    serviço : $(systemctl is-active btrfs-manager-helper.service 2>/dev/null || echo 'unknown')"
echo ""
echo "    Para ver logs em tempo real:"
echo "      journalctl -fu btrfs-manager-helper"
echo ""
echo "    Para testar D-Bus manualmente:"
echo "      bash scripts/check-installed-dbus-helper.sh"
echo ""
echo "    Para desinstalar:"
echo "      sudo bash scripts/dev-install.sh remove"
