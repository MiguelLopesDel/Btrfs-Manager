#!/usr/bin/env bash
set -euo pipefail

HELPER_BIN="${BTRFS_MANAGER_HELPER_BIN:-/usr/lib/btrfs-manager/btrfs-manager-helper}"
DBUS_CONF="${BTRFS_MANAGER_DBUS_CONF:-/usr/share/dbus-1/system.d/org.btrfsmanager.Helper.conf}"
DBUS_SERVICE="${BTRFS_MANAGER_DBUS_SERVICE:-/usr/share/dbus-1/system-services/org.btrfsmanager.Helper.service}"
POLKIT_POLICY="${BTRFS_MANAGER_POLKIT_POLICY:-/usr/share/polkit-1/actions/org.btrfsmanager.helper.policy}"

require_file() {
  local path="$1"
  if [ ! -f "$path" ]; then
    echo "missing: $path" >&2
    exit 1
  fi
}

require_executable() {
  local path="$1"
  if [ ! -x "$path" ]; then
    echo "missing or not executable: $path" >&2
    exit 1
  fi
}

require_command() {
  local command="$1"
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "missing command: $command" >&2
    exit 1
  fi
}

require_executable "$HELPER_BIN"
require_file "$DBUS_CONF"
require_file "$DBUS_SERVICE"
require_file "$POLKIT_POLICY"

require_command busctl
require_command xmllint
require_command pkaction

xmllint --noout "$DBUS_CONF"
xmllint --noout "$POLKIT_POLICY"

for action in \
  org.btrfsmanager.helper.discovery \
  org.btrfsmanager.helper.manage \
  org.btrfsmanager.helper.rollback \
  org.btrfsmanager.helper.policy.read
do
  pkaction --action-id "$action" >/dev/null
done

busctl --system introspect \
  org.btrfsmanager.Helper \
  /org/btrfsmanager/Helper \
  org.btrfsmanager.Helper >/dev/null

echo "Installed Btrfs Manager D-Bus/Polkit helper check passed."
