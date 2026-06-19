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
LOOP_DEVICE=""
MOUNT_SOURCE="$IMAGE"
SKIP_UNSUPPORTED_LOOPBACK="${BTRFS_MANAGER_SKIP_UNSUPPORTED_LOOPBACK:-0}"

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
  if [ -n "$LOOP_DEVICE" ]; then
    sudo losetup -d "$LOOP_DEVICE" 2>/dev/null || true
  fi
  true
}

fail() {
  echo "error: $*" >&2
  exit 1
}

skip_unsupported_loopback() {
  if [ "$SKIP_UNSUPPORTED_LOOPBACK" = "1" ]; then
    echo "warning: skipping loopback Btrfs integration test: $*" >&2
    exit 0
  fi
  fail "$@"
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
command -v jq >/dev/null 2>&1 || fail "jq is required for JSON output verification"

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
sudo modprobe btrfs 2>/dev/null || true
sudo modprobe loop 2>/dev/null || true
if LOOP_DEVICE="$(sudo losetup --find --show "$IMAGE" 2>/dev/null)"; then
  MOUNT_SOURCE="$LOOP_DEVICE"
  sudo mount "$MOUNT_SOURCE" "$MOUNTPOINT" || skip_unsupported_loopback "failed to mount loopback device $MOUNT_SOURCE"
else
  LOOP_DEVICE=""
  MOUNT_SOURCE="$IMAGE"
  sudo mount -o loop "$IMAGE" "$MOUNTPOINT" || skip_unsupported_loopback "failed to mount loopback image. The container or runner may not expose loop devices."
fi
mountpoint -q "$MOUNTPOINT" || fail "loopback mountpoint is not mounted after mount command"
sudo chown "$(id -u):$(id -g)" "$MOUNTPOINT"

echo "==> Testing discover-filesystems"
DISCOVERY=$(sudo "$HELPER" discover-filesystems)
echo "$DISCOVERY" | jq . >/dev/null || fail "discover-filesystems did not return valid JSON"
FOUND_MOUNT=$(echo "$DISCOVERY" | jq -r ".data.filesystems[].mounts[] | select(.mountpoint == \"$MOUNTPOINT\") | .mountpoint" 2>/dev/null | head -1)
[ "$FOUND_MOUNT" = "$MOUNTPOINT" ] || fail "discover-filesystems did not find test mountpoint $MOUNTPOINT (found: '${FOUND_MOUNT:-none}')"
FS_UUID=$(echo "$DISCOVERY" | jq -r ".data.filesystems[] | select(.mounts[].mountpoint == \"$MOUNTPOINT\") | .id" 2>/dev/null | head -1)
[ -n "$FS_UUID" ] || fail "discover-filesystems found no UUID for test filesystem"

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

echo "==> Verifying initial subvolume classification"
INIT_INV=$(sudo "$HELPER" list-subvolumes "$MOUNTPOINT")
SNAP1_KIND=$(echo "$INIT_INV" | jq -r '.data.subvolumes[] | select(.path == "snapshots/snap-1") | .kind')
[ "$SNAP1_KIND" = "Snapshot" ] || fail "snapshots/snap-1 should be Snapshot, got: '${SNAP1_KIND}'"
SNAP2_KIND=$(echo "$INIT_INV" | jq -r '.data.subvolumes[] | select(.path == "snapshots/snap-2") | .kind')
[ "$SNAP2_KIND" = "Snapshot" ] || fail "snapshots/snap-2 should be Snapshot, got: '${SNAP2_KIND}'"
DATA_KIND=$(echo "$INIT_INV" | jq -r '.data.subvolumes[] | select(.path == "@data") | .kind')
[ "$DATA_KIND" = "Normal" ] || fail "@data should be Normal, got: '${DATA_KIND}'"

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
sudo chown "$(id -u):$(id -g)" "$MOUNTPOINT/@"
mkdir -p "$MOUNTPOINT/@/etc"
printf "root version one\n" > "$MOUNTPOINT/@/etc/example.conf"
sudo btrfs subvolume create "$MOUNTPOINT/@snapshots" >/dev/null
sudo chown -R "$(id -u):$(id -g)" "$MOUNTPOINT/@" "$MOUNTPOINT/@snapshots"
mkdir -p "$MOUNTPOINT/@snapshots/296"
sudo btrfs subvolume snapshot -r "$MOUNTPOINT/@" "$MOUNTPOINT/@snapshots/296/snapshot" >/dev/null

echo "==> Verifying @snapshots container and snapshot classification"
FULL_INV=$(sudo "$HELPER" list-subvolumes "$MOUNTPOINT")
CONTAINER_KIND=$(echo "$FULL_INV" | jq -r '.data.subvolumes[] | select(.path == "@snapshots") | .kind')
[ "$CONTAINER_KIND" = "SnapshotContainer" ] || fail "@snapshots should be SnapshotContainer, got: '${CONTAINER_KIND}'"
ROOT_SNAP_KIND=$(echo "$FULL_INV" | jq -r '.data.subvolumes[] | select(.path == "@snapshots/296/snapshot") | .kind')
[ "$ROOT_SNAP_KIND" = "Snapshot" ] || fail "@snapshots/296/snapshot should be Snapshot, got: '${ROOT_SNAP_KIND}'"
AT_KIND=$(echo "$FULL_INV" | jq -r '.data.subvolumes[] | select(.path == "@") | .kind')
[ "$AT_KIND" = "Normal" ] || fail "@ should be Normal, got: '${AT_KIND}'"

sudo umount "$MOUNTPOINT"
if [ -n "$LOOP_DEVICE" ]; then
  sudo mount -o subvol=@ "$MOUNT_SOURCE" "$MOUNTPOINT"
else
  sudo mount -o loop,subvol=@ "$IMAGE" "$MOUNTPOINT"
fi

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
