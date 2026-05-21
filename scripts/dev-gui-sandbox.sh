#!/usr/bin/env bash
# Cria um filesystem Btrfs descartável em /tmp e abre a GUI contra ele.
# Limpa tudo quando a GUI fecha ou Ctrl+C.
#
# Uso:
#   bash scripts/dev-gui-sandbox.sh
#
# Requer (Arch):
#   sudo pacman -S btrfs-progs gtk4 libadwaita
#
# Em Wayland, sudo perde acesso ao compositor. Antes de rodar:
#   xhost +local:root      (libera root para conectar ao display)
# Ou rodar a GUI sem sudo (discovery funciona; mount/snapshot pede auth):
#   BTRFS_MANAGER_DEV_LOCAL_HELPER=1 BTRFS_MANAGER_MOUNTPOINT=/mnt/btrfs-manager-sandbox \
#     target/debug/btrfs-manager-app

set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="${BTRFS_MANAGER_SANDBOX_IMAGE:-/tmp/btrfs-manager-sandbox.img}"
MOUNTPOINT="${BTRFS_MANAGER_SANDBOX_MOUNT:-/mnt/btrfs-manager-sandbox}"
IMAGE_SIZE="${BTRFS_MANAGER_SANDBOX_SIZE:-256M}"

cleanup() {
  set +e
  echo ""
  echo "==> Limpando sandbox"
  sudo umount "$MOUNTPOINT" 2>/dev/null
  sudo rmdir  "$MOUNTPOINT" 2>/dev/null
  rm -f "$IMAGE"
  echo "==> Pronto."
}
trap cleanup EXIT

fail() { echo "error: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] && \
  fail "não rode com sudo — o script pede sudo apenas para operações Btrfs"

command -v mkfs.btrfs >/dev/null 2>&1 || \
  fail "btrfs-progs não encontrado (sudo pacman -S btrfs-progs)"
command -v cargo >/dev/null 2>&1 || \
  fail "cargo não encontrado no PATH"

echo "==> Compilando helper e GUI"
cargo build -p btrfs-manager-helper \
            -p btrfs-manager-app \
            --features btrfs-manager-app/gui

HELPER="$(pwd)/target/debug/btrfs-manager-helper"
APP="$(pwd)/target/debug/btrfs-manager-app"

echo "==> Criando imagem Btrfs ($IMAGE_SIZE)"
rm -f "$IMAGE"
truncate -s "$IMAGE_SIZE" "$IMAGE"
mkfs.btrfs -q -f "$IMAGE"

echo "==> Montando em $MOUNTPOINT"
sudo mkdir -p "$MOUNTPOINT"
sudo mount -o loop "$IMAGE" "$MOUNTPOINT"
sudo chown "$(id -u):$(id -g)" "$MOUNTPOINT"

echo "==> Criando subvolumes e snapshots de exemplo"

# Subvolumes normais
sudo btrfs subvolume create "$MOUNTPOINT/@data"      >/dev/null
sudo btrfs subvolume create "$MOUNTPOINT/@home"      >/dev/null
sudo btrfs subvolume create "$MOUNTPOINT/@snapshots" >/dev/null
sudo chown -R "$(id -u):$(id -g)" \
  "$MOUNTPOINT/@data" \
  "$MOUNTPOINT/@home" \
  "$MOUNTPOINT/@snapshots"

# Conteúdo em @data
mkdir -p "$MOUNTPOINT/@data/documentos" "$MOUNTPOINT/@data/projetos"
printf "Relatório — versão 1\nConteúdo inicial.\n" \
  > "$MOUNTPOINT/@data/documentos/relatorio.txt"
printf "config = True\n" \
  > "$MOUNTPOINT/@data/projetos/config.py"

# Conteúdo em @home
mkdir -p "$MOUNTPOINT/@home/downloads"
printf "# .bashrc\nexport EDITOR=vim\n" \
  > "$MOUNTPOINT/@home/.bashrc"

# Snapshot 1 de @data (snapper-style: @snapshots/N/snapshot)
mkdir -p "$MOUNTPOINT/@snapshots/1"
sudo btrfs subvolume snapshot -r \
  "$MOUNTPOINT/@data" \
  "$MOUNTPOINT/@snapshots/1/snapshot" >/dev/null

# Modificar @data para snapshot 2 mostrar diferença
printf "Relatório — versão 2\nAtualizado com novidades.\n" \
  > "$MOUNTPOINT/@data/documentos/relatorio.txt"
printf "arquivo_novo.md\n" \
  > "$MOUNTPOINT/@data/documentos/changelog.md"

# Snapshot 2 de @data
mkdir -p "$MOUNTPOINT/@snapshots/2"
sudo btrfs subvolume snapshot -r \
  "$MOUNTPOINT/@data" \
  "$MOUNTPOINT/@snapshots/2/snapshot" >/dev/null

# Snapshot de @home via helper (aparece como criado pelo app)
sudo "$HELPER" create-snapshot \
  "$MOUNTPOINT/@home" \
  "$MOUNTPOINT/@snapshots/home-backup" >/dev/null

echo ""
echo "==> Sandbox pronto"
echo "    mountpoint : $MOUNTPOINT"
echo "    subvolumes : @data, @home, @snapshots (container)"
echo "    snapshots  : @snapshots/1/snapshot"
echo "               : @snapshots/2/snapshot"
echo "               : @snapshots/home-backup"
echo ""
echo "==> Abrindo GUI — feche a janela ou Ctrl+C para limpar o sandbox"
echo ""

# Unset XDG_RUNTIME_DIR so the root process does not create dirs inside the
# user's /run/user/UID — those would be owned by root and break future non-root
# runs where the user can't write to their own runtime dir.
BTRFS_MANAGER_DEV_LOCAL_HELPER=1 \
BTRFS_MANAGER_MOUNTPOINT="$MOUNTPOINT" \
XDG_RUNTIME_DIR="" \
  sudo -E "$APP"
