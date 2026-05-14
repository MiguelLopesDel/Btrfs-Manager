# Architecture

Btrfs Manager is split around the privilege boundary.

The desktop app is unprivileged and owns presentation, timeline filters, search, comparison views, and user confirmation flows. It must not shell out to privileged Btrfs commands directly.

The helper owns privileged operations only: creating/deleting snapshots, toggling the Btrfs `ro` property, temporary read-only mounts, rollback staging, and systemd timer execution. Installed builds expose the helper as the `org.btrfsmanager.Helper` system D-Bus service and authorize each request through Polkit before executing it.

During repository development, the GUI still has a fallback to local read-only discovery and `pkexec` for privileged operations when the D-Bus service is not installed. Set `BTRFS_MANAGER_REQUIRE_DBUS=1` to test the installed-service path strictly.

The core crate is shared by both sides and contains data models, command output parsers, path validation, retention selection, comparison primitives, and rollback transaction models.

## Safety defaults

- Managed snapshots are created read-only.
- External snapshots are detected but not mutated unless the operator explicitly enables that behavior later.
- Unlocking a snapshot sets `ro=false` and must mark the snapshot as dirty/unlocked in state.
- Rollback is staged as a reversible transaction: clone from snapshot, create a return snapshot from the currently active subvolume, then activate on reboot or via bootloader integration.
