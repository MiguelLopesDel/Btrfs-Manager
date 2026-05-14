# Btrfs Manager

Modern Linux desktop manager for Btrfs subvolumes and snapshots.

See [docs/product-roadmap.md](docs/product-roadmap.md) for the product requirements, implementation phases, and current priorities.
See [docs/continuous-quality.md](docs/continuous-quality.md) for CI quality gates and review expectations.

The project is intentionally split into:

- `crates/core`: Btrfs domain models, parsers, retention logic, rollback planning, and safe path handling.
- `crates/helper`: privileged-operation boundary exposed as a system D-Bus service and authorized through Polkit.
- `crates/app`: unprivileged desktop application shell. The GTK/libadwaita UI is gated behind the `gui` feature.

## Current status

This is an early implementation. It includes Btrfs discovery, subvolume/snapshot classification, read-only snapshot browsing, a conservative helper command boundary, D-Bus/Polkit packaging, i18n files, quality gates, and a GTK/libadwaita UI.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-targets --no-default-features
python3 scripts/quality-gate.py check --write-report
cargo run -p btrfs-manager-app
```

To configure the GitHub remote and required branch checks after creating the
repository, run `scripts/github-bootstrap.sh`. It reads `GH_TOKEN` or
`GITHUB_TOKEN` at runtime; `.env` is ignored and must not be committed.

On a fresh Arch VM, run the bootstrap script:

```sh
bash scripts/vm-arch-bootstrap.sh
```

For real Btrfs operations without a VM, run the loopback integration test. It creates a disposable image in `/tmp` and mounts it at `/mnt/btrfs-manager-test`:

```sh
bash scripts/dev-loopback-btrfs-test.sh
```

Do not run the script with `sudo bash`; it calls `sudo` internally only for the operations that require it.

For repeatedly copying the latest Caelestia screenshot to the chat clipboard:

```sh
scripts/copy-last-screenshot
```

It watches `/tmp/caelestia-picker-*.png`, serves the latest image as `image/png`, and keeps running after the image is consumed.

The GUI feature requires native GTK4/libadwaita development packages:

```sh
cargo run -p btrfs-manager-app --features gui
```

Repository runs fall back to local discovery and `pkexec` when the installed D-Bus service is not available. Set `BTRFS_MANAGER_REQUIRE_DBUS=1` to force the GUI to use only the D-Bus helper path while testing packaging.

Real Btrfs integration tests should be run in a VM or throwaway loopback filesystem.
