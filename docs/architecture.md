# Architecture

Btrfs Manager is split around a strict privilege boundary.

The desktop app is unprivileged and owns presentation, timeline filters, search,
comparison views, and user confirmation flows. In production it must not shell
out to `btrfs`, `mount`, `umount`, `systemctl`, `pkexec`, or the helper CLI.

The privileged helper owns system operations only: discovery that needs Btrfs
privilege, creating/deleting snapshots, toggling the Btrfs `ro` property,
temporary read-only mounts, rollback staging, systemd timer management, and
policy execution.

Installed builds expose the helper as the `org.btrfsmanager.Helper` system
D-Bus service and authorize each request through Polkit before executing it.
That D-Bus service is the only production path from GUI to privileged work.

The helper CLI remains useful for administration, debugging, integration tests,
and systemd units. It is not the production IPC layer for the GUI.

Repository development keeps an explicit local fallback behind
`BTRFS_MANAGER_DEV_LOCAL_HELPER=1`. The normal GUI path fails with an actionable
"service not installed/running" error if the D-Bus helper is absent.

The core crate is shared by both sides and contains data models, command output
parsers, path validation, retention selection, comparison primitives, and
rollback transaction models.

## Safety defaults

- Managed snapshots are created read-only.
- External snapshots are detected but not mutated unless the operator explicitly enables that behavior later.
- Unlocking a snapshot sets `ro=false` and must mark the snapshot as dirty/unlocked in state.
- Rollback is staged as a reversible transaction: clone from snapshot, create a return snapshot from the currently active subvolume, then activate on reboot or via bootloader integration.

## Production IPC contract

- GUI -> system D-Bus -> root helper -> Polkit -> Btrfs/system command.
- CLI -> helper request handler, for admin/test/systemd use.
- Core -> pure domain logic, no privilege and no UI.

The GUI should not decide whether to use `pkexec`; Polkit authorization belongs
inside the D-Bus service boundary.
