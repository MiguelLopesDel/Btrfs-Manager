#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE="${BTRFS_MANAGER_TEST_IMAGE:-/tmp/btrfs-manager-test.img}"
MOUNTPOINT="${BTRFS_MANAGER_TEST_MOUNT:-/mnt/btrfs-manager-test}"
IMAGE_SIZE="${BTRFS_MANAGER_TEST_SIZE:-512M}"
HELPER="target/debug/btrfs-manager-helper"
TOPLEVEL_MOUNT="${BTRFS_MANAGER_TOPLEVEL_MOUNT:-/tmp/btrfs-manager-test-top-level}"
TOPLEVEL_BROWSE="${BTRFS_MANAGER_TOPLEVEL_BROWSE:-/tmp/btrfs-manager-test-top-level-browse}"
MKFS_BTRFS="${MKFS_BTRFS:-}"

cleanup() {
  set +e
  if mountpoint -q "$TOPLEVEL_BROWSE"; then
    sudo umount "$TOPLEVEL_BROWSE"
  fi
  if mountpoint -q "$TOPLEVEL_MOUNT"; then
    sudo umount "$TOPLEVEL_MOUNT"
  fi
  if mountpoint -q "$MOUNTPOINT/snapshots/snap-1-browse"; then
    sudo umount "$MOUNTPOINT/snapshots/snap-1-browse"
  fi
  if mountpoint -q "$MOUNTPOINT"; then
    sudo umount "$MOUNTPOINT"
  fi
  true
}

fail() {
  echo "error: $*" >&2
  exit 1
}

trap cleanup EXIT

echo "==> Btrfs Manager loopback integration test"
echo "    image:      $IMAGE"
echo "    mountpoint: $MOUNTPOINT"

if [ "$(id -u)" -eq 0 ]; then
  fail "do not run this script with sudo. Run it as your normal user; it will ask sudo only for mount/Btrfs operations."
fi

command -v btrfs >/dev/null 2>&1 || fail "btrfs-progs is missing"
if [ -z "$MKFS_BTRFS" ]; then
  if command -v mkfs.btrfs >/dev/null 2>&1; then
    MKFS_BTRFS="$(command -v mkfs.btrfs)"
  elif [ -x /usr/sbin/mkfs.btrfs ]; then
    MKFS_BTRFS="/usr/sbin/mkfs.btrfs"
  else
    fail "mkfs.btrfs is missing"
  fi
fi
command -v sudo >/dev/null 2>&1 || fail "sudo is required for mount and btrfs operations"
command -v cargo >/dev/null 2>&1 || fail "cargo is missing from this user's PATH. Install Rust/Cargo or open a shell where cargo works."

echo "==> Building helper"
cargo build -p btrfs-manager-helper -p btrfs-manager-app --no-default-features

echo "==> Preparing clean loopback image"
cleanup
set -e
rm -f "$IMAGE"
truncate -s "$IMAGE_SIZE" "$IMAGE"
"$MKFS_BTRFS" -q -f "$IMAGE"

echo "==> Mounting image"
sudo mkdir -p "$MOUNTPOINT"
sudo mount -o loop "$IMAGE" "$MOUNTPOINT" || fail "failed to mount loopback image. The container may not expose loop devices."
mountpoint -q "$MOUNTPOINT" || fail "loopback mountpoint is not mounted after mount command"
sudo chown "$(id -u):$(id -g)" "$MOUNTPOINT"

echo "==> Creating test subvolumes and files"
mkdir -p "$MOUNTPOINT/snapshots"
sudo btrfs subvolume create "$MOUNTPOINT/@data" >/dev/null
sudo chown -R "$(id -u):$(id -g)" "$MOUNTPOINT/@data" "$MOUNTPOINT/snapshots"
mkdir -p "$MOUNTPOINT/@data/docs"
printf "version one\n" > "$MOUNTPOINT/@data/docs/example.txt"
printf "keep me\n" > "$MOUNTPOINT/@data/docs/unchanged.txt"

echo "==> Creating read-only snapshot through helper"
sudo "$HELPER" create-snapshot \
  "$MOUNTPOINT/@data" \
  "$MOUNTPOINT/snapshots/snap-1"

echo "==> Verifying snapshot readonly property"
SNAP_RO="$(sudo btrfs property get "$MOUNTPOINT/snapshots/snap-1" ro)"
case "$SNAP_RO" in
  *"ro=true"*) ;;
  *) fail "snapshot should be readonly, got: $SNAP_RO" ;;
esac

echo "==> Changing source subvolume"
printf "version two\nwith more content\n" > "$MOUNTPOINT/@data/docs/example.txt"
rm "$MOUNTPOINT/@data/docs/unchanged.txt"
printf "new file\n" > "$MOUNTPOINT/@data/docs/new.txt"

echo "==> Creating second snapshot through helper"
sudo "$HELPER" create-snapshot \
  "$MOUNTPOINT/@data" \
  "$MOUNTPOINT/snapshots/snap-2"

echo "==> Listing subvolumes through helper"
sudo "$HELPER" list-subvolumes "$MOUNTPOINT"

echo "==> Listing subvolumes through app shell"
sudo target/debug/btrfs-manager-app list --mountpoint "$MOUNTPOINT"

echo "==> Temporarily unlocking and locking first snapshot"
sudo "$HELPER" set-readonly "$MOUNTPOINT/snapshots/snap-1" false
SNAP_RW="$(sudo btrfs property get "$MOUNTPOINT/snapshots/snap-1" ro)"
case "$SNAP_RW" in
  *"ro=false"*) ;;
  *) fail "snapshot should be writable after unlock, got: $SNAP_RW" ;;
esac
sudo "$HELPER" set-readonly "$MOUNTPOINT/snapshots/snap-1" true

echo "==> Mounting snapshot read-only through helper"
mkdir -p "$MOUNTPOINT/snapshots/snap-1-browse"
sudo "$HELPER" mount-snapshot \
  "$MOUNTPOINT/snapshots/snap-1" \
  "$MOUNTPOINT/snapshots/snap-1-browse"
if printf "should fail\n" > "$MOUNTPOINT/snapshots/snap-1-browse/write-test.txt" 2>/dev/null; then
  fail "read-only browse mount unexpectedly allowed writing"
fi
sudo "$HELPER" unmount-snapshot "$MOUNTPOINT/snapshots/snap-1-browse"
rmdir "$MOUNTPOINT/snapshots/snap-1-browse"

echo "==> Reproducing root mounted as subvol=@ with snapshots in top-level"
sudo btrfs subvolume create "$MOUNTPOINT/@" >/dev/null
mkdir -p "$MOUNTPOINT/@/etc"
printf "root version one\n" > "$MOUNTPOINT/@/etc/example.conf"
sudo btrfs subvolume create "$MOUNTPOINT/@snapshots" >/dev/null
sudo chown -R "$(id -u):$(id -g)" "$MOUNTPOINT/@" "$MOUNTPOINT/@snapshots"
mkdir -p "$MOUNTPOINT/@snapshots/296"
sudo btrfs subvolume snapshot -r "$MOUNTPOINT/@" "$MOUNTPOINT/@snapshots/296/snapshot" >/dev/null

sudo umount "$MOUNTPOINT"
sudo mount -o loop,subvol=@ "$IMAGE" "$MOUNTPOINT"

if [ -e "$MOUNTPOINT/@snapshots/296/snapshot" ]; then
  fail "test setup is invalid: snapshot should not be directly visible from subvol=@ mount"
fi

echo "==> Mounting Btrfs top-level through helper"
mkdir -p "$TOPLEVEL_MOUNT"
sudo "$HELPER" mount-top-level "$MOUNTPOINT" "$TOPLEVEL_MOUNT"

if [ ! -e "$TOPLEVEL_MOUNT/@snapshots/296/snapshot" ]; then
  fail "top-level mount did not expose @snapshots/296/snapshot"
fi

echo "==> Browsing top-level snapshot read-only through helper"
mkdir -p "$TOPLEVEL_BROWSE"
sudo "$HELPER" mount-snapshot \
  "$TOPLEVEL_MOUNT/@snapshots/296/snapshot" \
  "$TOPLEVEL_BROWSE"
if printf "should fail\n" > "$TOPLEVEL_BROWSE/write-test.txt" 2>/dev/null; then
  fail "top-level read-only browse mount unexpectedly allowed writing"
fi
sudo "$HELPER" unmount-snapshot "$TOPLEVEL_BROWSE"
sudo "$HELPER" unmount-snapshot "$TOPLEVEL_MOUNT"

echo "==> Smoke checking app shell"
cargo run -p btrfs-manager-app --no-default-features

echo "==> Loopback Btrfs test completed successfully"
