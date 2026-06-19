#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/vm-root-rollback-e2e.sh"
HELPER="$ROOT/target/debug/btrfs-manager-helper"
TOP_MOUNT="${BTRFS_MANAGER_E2E_TOP_MOUNT:-/run/btrfs-manager-root-rollback-e2e/top}"
STATE_REL="@btrfs-manager/e2e-state/root-rollback-e2e.env"
MARKER="/etc/btrfs-manager-root-rollback-e2e-marker"
UNIT="btrfs-manager-root-rollback-e2e.service"
UNIT_PATH="/etc/systemd/system/$UNIT"

MODE="start"
YES=0
NO_REBOOT="${BTRFS_MANAGER_E2E_NO_REBOOT:-0}"

for arg in "$@"; do
  case "$arg" in
    --resume) MODE="resume" ;;
    --yes) YES=1 ;;
    --no-reboot) NO_REBOOT=1 ;;
    -h|--help)
      cat <<EOF
Usage:
  bash scripts/vm-root-rollback-e2e.sh --yes
  bash scripts/vm-root-rollback-e2e.sh --resume

This is a destructive VM-only root rollback test. It stages rollback for the
currently booted Btrfs root, reboots, validates the restored root, reverts, and
reboots again to validate the return anchor.

Safety:
  - Refuses to run outside a detected VM unless BTRFS_MANAGER_E2E_ALLOW_HOST=1.
  - Requires --yes for the initial start.
  - Stores its state outside the root subvolume under $STATE_REL.
EOF
      exit 0
      ;;
    *)
      echo "error: unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

log() {
  echo "==> $*"
}

fail() {
  echo "error: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

require_vm_safety() {
  if [ "${BTRFS_MANAGER_E2E_ALLOW_HOST:-0}" = "1" ]; then
    return
  fi
  if command -v systemd-detect-virt >/dev/null 2>&1 && systemd-detect-virt --vm --quiet; then
    return
  fi
  fail "refusing to run root rollback E2E outside a detected VM; set BTRFS_MANAGER_E2E_ALLOW_HOST=1 only if you really know this host is disposable"
}

require_root_btrfs() {
  local fstype options subvol
  fstype="$(findmnt -n -o FSTYPE --target /)"
  [ "$fstype" = "btrfs" ] || fail "/ must be mounted from Btrfs, got: ${fstype:-unknown}"
  options="$(findmnt -n -o OPTIONS --target /)"
  subvol="$(printf '%s\n' "$options" | tr ',' '\n' | sed -n 's/^subvol=//p' | head -1)"
  [ -n "$subvol" ] || fail "/ must be mounted with a subvol= option so rollback can restore the named subvolume"
}

build_helper_as_user() {
  if [ "$MODE" = "resume" ]; then
    [ -x "$HELPER" ] || fail "helper binary not found at $HELPER after reboot"
    return
  fi
  if [ "$(id -u)" -eq 0 ]; then
    [ -x "$HELPER" ] || fail "run the initial phase as a normal user so cargo can build $HELPER before sudo takes over"
    return
  fi
  require_command cargo
  log "Building helper"
  cargo build -p btrfs-manager-helper --no-default-features
}

reexec_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    return
  fi
  log "Re-running as root for Btrfs namespace changes"
  exec sudo env \
    BTRFS_MANAGER_E2E_ALLOW_HOST="${BTRFS_MANAGER_E2E_ALLOW_HOST:-0}" \
    BTRFS_MANAGER_E2E_NO_REBOOT="$NO_REBOOT" \
    BTRFS_MANAGER_E2E_TOP_MOUNT="$TOP_MOUNT" \
    bash "$SCRIPT" "$@"
}

root_source_device() {
  findmnt -n -o SOURCE --target / | sed 's/\[.*//'
}

mount_top_level() {
  mkdir -p "$TOP_MOUNT"
  if ! mountpoint -q "$TOP_MOUNT"; then
    mount -o subvolid=5 "$(root_source_device)" "$TOP_MOUNT"
  fi
  mkdir -p "$TOP_MOUNT/@btrfs-manager/e2e-state"
}

state_file() {
  printf '%s/%s\n' "$TOP_MOUNT" "$STATE_REL"
}

write_state() {
  local phase="$1"
  cat >"$(state_file)" <<EOF
PHASE=$phase
RUN_ID=$RUN_ID
PLAN_ID=$PLAN_ID
TARGET_PATH=$TARGET_PATH
RETURN_PATH=$RETURN_PATH
BASELINE_VALUE=$BASELINE_VALUE
CHANGED_VALUE=$CHANGED_VALUE
EOF
}

load_state() {
  mount_top_level
  local file
  file="$(state_file)"
  [ -f "$file" ] || fail "state file not found: $file"
  # shellcheck disable=SC1090
  source "$file"
}

install_resume_unit() {
  cat >"$UNIT_PATH" <<EOF
[Unit]
Description=Btrfs Manager root rollback E2E resume
After=local-fs.target multi-user.target
RequiresMountsFor=$ROOT

[Service]
Type=oneshot
Environment=BTRFS_MANAGER_E2E_ALLOW_HOST=${BTRFS_MANAGER_E2E_ALLOW_HOST:-0}
Environment=BTRFS_MANAGER_E2E_TOP_MOUNT=$TOP_MOUNT
ExecStart=/usr/bin/env bash $SCRIPT --resume

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable "$UNIT"
}

remove_resume_unit() {
  systemctl disable "$UNIT" >/dev/null 2>&1 || true
  rm -f "$UNIT_PATH"
  systemctl daemon-reload
}

reboot_or_stop() {
  local reason="$1"
  if [ "$NO_REBOOT" = "1" ]; then
    log "$reason; --no-reboot is set, reboot manually and then run: sudo bash $SCRIPT --resume"
    exit 0
  fi
  log "$reason; rebooting now"
  systemctl reboot
}

cleanup_target_snapshot() {
  if [ -n "${TARGET_PATH:-}" ] && [ -e "$TOP_MOUNT/$TARGET_PATH" ]; then
    btrfs subvolume delete "$TOP_MOUNT/$TARGET_PATH" >/dev/null || true
  fi
}

cleanup_failed_start() {
  cleanup_target_snapshot
  rm -f "$MARKER"
  if mountpoint -q "$TOP_MOUNT"; then
    rm -f "$(state_file)"
  fi
  remove_resume_unit
}

start_phase() {
  [ "$YES" = "1" ] || fail "initial root rollback E2E requires --yes"
  require_vm_safety
  require_root_btrfs
  mount_top_level

  RUN_ID="$(date +%Y%m%d-%H%M%S)"
  TARGET_PATH="@btrfs-manager/e2e-target-$RUN_ID"
  RETURN_PATH="@btrfs-manager/return-e2e-$RUN_ID"
  BASELINE_VALUE="baseline-$RUN_ID"
  CHANGED_VALUE="changed-$RUN_ID"
  PLAN_ID=""

  trap cleanup_failed_start ERR
  install_resume_unit

  log "Writing baseline marker"
  printf '%s\n' "$BASELINE_VALUE" >"$MARKER"

  log "Creating read-only rollback target at $TARGET_PATH"
  mkdir -p "$(dirname "$TOP_MOUNT/$TARGET_PATH")"
  btrfs subvolume snapshot -r / "$TOP_MOUNT/$TARGET_PATH" >/dev/null

  log "Changing root marker before staging rollback"
  printf '%s\n' "$CHANGED_VALUE" >"$MARKER"

  log "Staging rollback to $TARGET_PATH"
  local response
  response="$("$HELPER" stage-rollback / "$TARGET_PATH" "$RETURN_PATH")"
  PLAN_ID="$(printf '%s\n' "$response" | jq -r '.data.id')"
  if [ -z "$PLAN_ID" ] || [ "$PLAN_ID" = "null" ]; then
    fail "could not parse rollback plan id from helper response: $response"
  fi

  write_state "verify_rollback"
  trap - ERR
  reboot_or_stop "Rollback staged"
}

verify_rollback_phase() {
  load_state
  [ "${PHASE:-}" = "verify_rollback" ] || fail "expected phase verify_rollback, got: ${PHASE:-unset}"
  require_root_btrfs

  local actual pending_id rebooted
  actual="$(cat "$MARKER")"
  [ "$actual" = "$BASELINE_VALUE" ] || fail "rollback boot did not restore baseline marker; expected $BASELINE_VALUE got $actual"

  log "Checking pending rollback prompt state"
  local pending
  pending="$("$HELPER" get-pending-rollback)"
  pending_id="$(printf '%s\n' "$pending" | jq -r '.data.plan.id')"
  rebooted="$(printf '%s\n' "$pending" | jq -r '.data.rebooted_since_staging')"
  [ "$pending_id" = "$PLAN_ID" ] || fail "pending rollback id mismatch; expected $PLAN_ID got ${pending_id:-none}"
  [ "$rebooted" = "true" ] || fail "pending rollback should report rebooted_since_staging=true, got $rebooted"

  log "Reverting rollback to return anchor"
  "$HELPER" revert-rollback "$PLAN_ID" >/dev/null
  write_state "verify_revert"
  reboot_or_stop "Rollback reverted"
}

verify_revert_phase() {
  load_state
  [ "${PHASE:-}" = "verify_revert" ] || fail "expected phase verify_revert, got: ${PHASE:-unset}"
  require_root_btrfs

  local actual pending
  actual="$(cat "$MARKER")"
  [ "$actual" = "$CHANGED_VALUE" ] || fail "revert boot did not restore return anchor marker; expected $CHANGED_VALUE got $actual"

  pending="$("$HELPER" get-pending-rollback)"
  [ "$(printf '%s\n' "$pending" | jq -r '.data')" = "null" ] || fail "rollback should be resolved after revert: $pending"

  log "Cleaning up E2E unit and target snapshot"
  cleanup_target_snapshot
  rm -f "$MARKER" "$(state_file)"
  remove_resume_unit
  log "Root rollback E2E completed successfully"
}

require_command findmnt
require_command jq
require_command btrfs
require_command mount
require_command systemctl
build_helper_as_user

if [ "$(id -u)" -ne 0 ]; then
  if [ "$MODE" = "start" ]; then
    reexec_as_root "$@"
  else
    fail "--resume must run as root"
  fi
fi

case "$MODE" in
  start) start_phase ;;
  resume)
    load_state
    case "${PHASE:-}" in
      verify_rollback) verify_rollback_phase ;;
      verify_revert) verify_revert_phase ;;
      *) fail "unknown E2E phase: ${PHASE:-unset}" ;;
    esac
    ;;
esac
