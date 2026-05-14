# Btrfs Manager Product Roadmap

This document is the working source of truth for what Btrfs Manager should become and the order we should implement it.

## Product Goal

Btrfs Manager is a modern Linux desktop app for managing Btrfs subvolumes, snapshots, browsing, comparison, scheduling, retention, and reversible rollback. It is meant to feel clearer and more capable than Timeshift/Btrfs Assistant while staying safe by default.

Target user for v1: technical Linux desktop users who understand Btrfs concepts but want a clean graphical tool with guardrails.

Primary stack:

- Rust workspace.
- GTK4/libadwaita desktop UI.
- Shared core crate for models, parsing, retention, comparison, and rollback planning.
- Privileged helper for Btrfs/system operations.
- Polkit/D-Bus as the final privilege boundary.
- Arch/AUR as the first packaging target.
- UI text in English with i18n structure and PT-BR translation.

## Core Principles

- The GUI must run unprivileged.
- Privileged actions must go through a narrow helper API.
- Snapshots created by the app are read-only by default.
- External snapshots from Snapper/Timeshift/grub-btrfs/refind-btrfs are detected but not modified by default.
- Destructive operations require clear confirmation and should be hidden from the main flow until implemented safely.
- Root rollback must be reversible: before activating a rollback target, preserve the currently active subvolume as a return snapshot.
- VM testing is useful for boot/rollback, but normal snapshot/subvolume tests should work through a disposable loopback Btrfs image.

## Current State

Already implemented:

- Rust workspace with `core`, `helper`, and `app`.
- Core models for filesystems, subvolumes, snapshots, retention, and rollback plans.
- Parser for `btrfs subvolume list -u`.
- Retention selection logic.
- Shallow directory comparison primitive.
- Helper operations for listing subvolumes, creating/deleting snapshots, toggling readonly, mounting/unmounting snapshots.
- CLI wrapper for helper operations.
- GTK/libadwaita UI shell.
- GUI refresh that lists real subvolumes/snapshot candidates.
- GUI search filtering over loaded inventory.
- GUI grouping for snapshots and subvolumes.
- GUI browse button for snapshot candidates, mounting read-only and opening with `xdg-open`.
- Helper discovery for mounted Btrfs filesystems using `findmnt`, including filesystem UUID, devices, mountpoints, active mounted subvolume, active root mount, and default subvolume when available.
- GUI filesystem selector that defaults to the active root filesystem and still supports `BTRFS_MANAGER_MOUNTPOINT` for loopback testing.
- Top-level mount fallback for browsing snapshots whose paths are relative to Btrfs subvolid 5.
- Managed temporary mount cleanup for browse/top-level mounts.
- Snapshot browsing UI with mounted state, explicit unmount button, and libadwaita toast errors.
- User-runtime browse mount path under `/run/user/<uid>/btrfs-manager/browse` when available, with `/tmp` fallback.
- Loopback Btrfs integration script.
- Continuous screenshot clipboard helper script for development feedback.
- Initial packaging files for Arch, Polkit, systemd timers, and desktop entry.

Known limitations:

- GUI uses the D-Bus helper service when installed, with temporary local/`pkexec` fallback for development runs from the repository.
- Snapshot path resolution has a top-level fallback, but still needs stronger production hardening and clearer visible errors.
- SQLite state is wired for snapshot policies, managed scheduled snapshots, and policy run logs.
- No real create-snapshot UI.
- Retention policy UI is early and focused on scheduled managed snapshots.
- No rollback implementation beyond models.
- No deep comparison UI.
- GUI error reporting is still incomplete outside snapshot browsing.

## Implementation Phases

### Phase 1: Discovery And Inventory

Goal: make the app reliably understand the Btrfs system before adding more actions.

Implement:

- [x] Discover Btrfs filesystems with UUID, devices, mountpoints, default subvolume, and top-level mount strategy.
- [x] Resolve subvolume paths correctly even when listed paths are relative to Btrfs top-level.
- [x] Distinguish normal subvolumes, snapshot containers, actual snapshots, and external tool snapshots.
- [x] Detect snapshot tools: Snapper, Timeshift, grub-btrfs, refind-btrfs where possible.
- [x] Add a compact filesystem selector to the UI instead of a hardcoded mountpoint.
- [x] Keep the UI default pointed at the active root filesystem, with an environment override for loopback tests.

Acceptance:

- [x] On the loopback image, the app lists `@data` and test snapshots correctly.
- [x] On the host root Btrfs, the app lists real subvolumes without requiring manual mountpoint entry.
- [x] `@snapshots` is shown as a subvolume/container, not as a snapshot.
- [x] Actual paths used for browse/mount actions are valid.

### Phase 2: Snapshot Browsing

Goal: make browsing snapshots a first-class, safe feature.

Implement:

- [x] Mount snapshot read-only under `/run/user/<uid>/btrfs-manager` for user-level browse mounts where possible, or `/run/btrfs-manager` through the helper when root is required.
- [x] Open mounted snapshots in the default file manager.
- [x] Track active browse mounts in memory and show mounted state in the row.
- [x] Add explicit unmount action.
- [x] Auto-clean stale browse mounts on startup where safe.
- [x] Replace stderr-only errors with visible libadwaita toasts/dialogs.

Acceptance:

- [x] Clicking browse opens a read-only view of the snapshot.
- [x] Writing into the mounted browse path fails.
- [x] User can unmount from the UI.
- [x] Repeated browse clicks do not create duplicate unmanaged mounts.

### Phase 3: Snapshot Creation

Goal: create managed snapshots safely from the UI.

Implement:

- [ ] Create snapshot action for a selected source subvolume.
- [ ] Destination naming convention under a configured snapshot root.
- [ ] Managed snapshot metadata persisted in SQLite.
- [ ] Tags/notes field.
- [x] Read-only by default.
- [x] Strong validation to prevent creating snapshots inside unsafe or recursive paths.
- [ ] Refresh inventory after creation.

Acceptance:

- [ ] User can create a snapshot of a selected subvolume.
- [ ] Snapshot appears under `Snapshots`.
- [x] Created snapshot is read-only.
- [ ] Managed state is persisted.

### Phase 4: Search, Timeline, And Filtering

Goal: make the main screen useful for large snapshot sets.

Implement:

- [ ] Timeline grouping by day/month.
- [ ] Filters: filesystem, subvolume, managed/external, readonly/unlocked, tags, date range.
- [ ] Search by path, tag, source subvolume, and note.
- [ ] Sort by creation time, path, or source subvolume.

Acceptance:

- [ ] Large lists remain scannable.
- [ ] Search and filters operate without re-running Btrfs commands.
- [ ] Empty states explain the filter result without generic placeholder text.

### Phase 5: Comparison

Goal: compare snapshots/subvolumes in a way that is useful but not too heavy.

Implement:

- [ ] Compare two snapshots or a snapshot and its source.
- [x] Default comparison: created, removed, modified by path, size, mtime, and file type.
- [ ] Folder-scoped comparison.
- [ ] Optional text diff for selected small text files.
- [ ] Binary files show metadata-only change.
- [ ] UI view for comparison results with search/filter.

Acceptance:

- [ ] User can select a snapshot and compare against another snapshot/source.
- [ ] Comparison works on the loopback test data.
- [ ] Large comparisons can be cancelled or bounded.

### Phase 6: Unlock/Lock Snapshot Editing

Goal: support Timeshift-like “edit snapshot” behavior explicitly and safely.

Implement:

- [x] Advanced action to set snapshot `ro=false`.
- [ ] Strong confirmation explaining that writable snapshots may break incremental send/receive assumptions.
- [ ] Mark unlocked snapshots as dirty/unlocked in state.
- [x] Action to set `ro=true` again while keeping dirty history.
- [ ] Do not allow editing external snapshots unless explicitly enabled in config.

Acceptance:

- [x] Managed read-only snapshot can be unlocked and locked again.
- [ ] UI clearly marks dirty/unlocked snapshots.
- [ ] External snapshots remain protected by default.

### Phase 7: Scheduling And Retention

Goal: automatic snapshots without a custom always-running daemon.

Implement:

- [x] Systemd timer generation for snapshot policies.
- [x] Policy presets: hourly, daily, weekly, monthly.
- [x] Retention by last N per frequency.
- [x] Retention preview before deletion.
- [x] Never delete external snapshots or rollback anchors.
- [x] Log policy runs.

Acceptance:

- [x] User can enable a policy for a subvolume.
- [x] Timer creates snapshots without the GUI running.
- [x] Retention removes only eligible managed snapshots.

### Phase 8: Reversible Rollback

Goal: provide the main differentiator: safe, clear rollback.

Implement:

- [ ] Stage rollback from a selected snapshot by cloning it to a new writable subvolume.
- [x] Before activation, create a read-only return snapshot of the currently active subvolume.
- [ ] Track rollback transaction in SQLite.
- [ ] Integrate with grub-btrfs when available.
- [ ] Integrate with refind-btrfs when available.
- [ ] For unsupported bootloaders, prepare and validate but show conservative manual instructions.
- [ ] Detect post-reboot state and offer commit/revert.

Acceptance:

- [ ] Rollback staging works in a VM.
- [ ] Current system state is preserved as a return snapshot.
- [ ] App can show pending/activated/reverted transaction state.

### Phase 9: Privilege Boundary

Goal: replace temporary `pkexec` CLI calls with a proper system service.

Implement:

- [x] D-Bus system service for helper.
- [x] Polkit actions per operation class: discovery, snapshot create, readonly toggle, delete, mount, rollback, policy management.
- [x] Helper installed under `/usr/lib/btrfs-manager`.
- [ ] GUI talks to D-Bus only, never shells out directly.
- [x] Structured errors from helper to UI.

Acceptance:

- [x] GUI runs as normal user.
- [x] Privileged actions trigger Polkit prompts.
- [x] Helper rejects unknown commands and unsafe paths.

### Phase 10: Packaging And Release

Goal: make the project installable and testable outside the repo.

Implement:

- [x] Arch PKGBUILD first.
- [ ] Install desktop file, icon, helper, Polkit policy, D-Bus service, systemd units, translations.
- [x] Loopback integration test in CI or documented local test.
- [ ] Optional AUR package.

Acceptance:

- [ ] Fresh Arch install can build and run the app.
- [ ] Installed app opens from desktop launcher.
- [ ] Helper and Polkit work after install.

## UI Direction

The app should feel like a quiet system utility, not a marketing page.

Main screen:

- Header with app title and refresh/action buttons.
- Snapshot-focused timeline/list as the default view.
- Search always visible.
- Filters and filesystem selector compact and functional.
- Rows should be readable and action-oriented: browse, compare, rollback, more.
- Subvolumes should be visible but secondary to snapshots.

Avoid:

- Explanatory paragraphs in the app.
- Placeholder text that looks like final product copy.
- Lab/test mountpoints visible in normal UI.
- Destructive actions in the main row before safety flows exist.

## Testing Strategy

Use three layers:

- Unit tests for parsers, retention, path safety, and comparison.
- Loopback Btrfs script for real subvolume/snapshot/mount behavior.
- VM tests only for root rollback and bootloader behavior.

Required recurring commands:

```sh
cargo test --workspace --all-targets --no-default-features
bash scripts/dev-loopback-btrfs-test.sh
```

For GUI development:

```sh
cargo run -p btrfs-manager-app --features gui
BTRFS_MANAGER_MOUNTPOINT=/mnt/btrfs-manager-test cargo run -p btrfs-manager-app --features gui
```

## Immediate Next Steps

Recommended order from here:

1. Implement robust filesystem discovery and path resolution.
2. Fix snapshot browsing to work on real root Btrfs layouts, not only loopback.
3. Add visible error dialogs/toasts for GUI actions.
4. Add create-snapshot UI for selected subvolumes.
5. Extend SQLite managed snapshot metadata to manual create-snapshot flows.
6. Harden the policy UI and run Phase 7 on a real systemd host.
