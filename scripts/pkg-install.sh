#!/usr/bin/env bash
# Build a pacman-managed package from the LOCAL repository (committed state on
# the current branch) and install it. Unlike dev-install.sh -- which drops
# unmanaged files under /usr -- this produces a real pacman package: clean
# upgrades, and clean removal via `pacman -R`.
#
# Usage (run as your normal user; sudo is requested only for pacman):
#   bash scripts/pkg-install.sh          # build current branch HEAD + install/upgrade
#   bash scripts/pkg-install.sh remove   # uninstall the pacman package
#
# To update: commit your changes (or `git pull`), then run this script again.
# It reuses packaging/arch/PKGBUILD, only overriding the source to point at the
# local checkout instead of GitHub, so the build/package logic stays in one place.

set -euo pipefail

PKGNAME="btrfs-manager-git"

fail() { echo "error: $*" >&2; exit 1; }

if [ "${1:-}" = "remove" ]; then
  command -v pacman >/dev/null 2>&1 || fail "pacman não encontrado"
  echo "==> Removendo pacote $PKGNAME"
  sudo pacman -Rns "$PKGNAME"
  exit 0
fi

[ "$(id -u)" -eq 0 ] && \
  fail "não rode como root — rode como usuário normal (o sudo é pedido só na instalação)"
command -v makepkg >/dev/null 2>&1 || fail "makepkg não encontrado (instale o grupo base-devel)"

REPO_ROOT="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)" \
  || fail "não parece um repositório git"
BRANCH="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" = "HEAD" ] && fail "HEAD desanexado — faça checkout de um branch antes de empacotar"

# dev-install.sh leaves unmanaged files at the same paths; pacman refuses to
# overwrite files it does not own. Guide the user to clean them up first.
if [ -e /usr/bin/btrfs-manager-app ] && \
   ! pacman -Qo /usr/bin/btrfs-manager-app >/dev/null 2>&1; then
  fail "há arquivos não gerenciados de dev-install.sh. Remova antes: sudo bash scripts/dev-install.sh remove"
fi

BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

cp "$REPO_ROOT/packaging/arch/PKGBUILD" "$BUILD_DIR/PKGBUILD"
cp "$REPO_ROOT/packaging/arch/${PKGNAME}.install" "$BUILD_DIR/"

# Override source/sums to clone the LOCAL repo (committed state on this branch)
# instead of GitHub main. makepkg sources the whole PKGBUILD before reading
# these variables, so an appended assignment wins over the in-file one.
cat >> "$BUILD_DIR/PKGBUILD" <<EOF

# --- injected by scripts/pkg-install.sh: build from the local checkout ---
source=("${PKGNAME}::git+file://${REPO_ROOT}#branch=${BRANCH}")
sha256sums=('SKIP')
EOF

echo "==> Empacotando $PKGNAME a partir de $REPO_ROOT (branch: $BRANCH)"
cd "$BUILD_DIR"
# -s: install build deps  -i: install the package  -c: clean workdir  -f: overwrite
makepkg -sicf

echo ""
echo "==> Instalado como pacote pacman '$PKGNAME'."
echo "    Atualizar : commite/pull e rode 'bash scripts/pkg-install.sh' de novo"
echo "    Remover   : bash scripts/pkg-install.sh remove   (ou: sudo pacman -Rns $PKGNAME)"
echo "    Serviço   : $(systemctl is-active btrfs-manager-helper.service 2>/dev/null || echo desconhecido)"
echo "    Abrir     : btrfs-manager-app"
