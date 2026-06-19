# Continuous Quality

Btrfs Manager uses CI as a merge gate. The goal is not only "tests pass"; every
pull request must preserve or improve the current health baseline.

## Required local checks

Run the fast checks before opening a PR:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-targets --no-default-features
cargo deny check
python3 scripts/quality-gate.py check --write-report
```

GUI checks require GTK4/libadwaita development packages:

```sh
cargo clippy -p btrfs-manager-app --features gui --all-targets -- -D warnings
cargo check -p btrfs-manager-app --features gui
bash scripts/e2e-headless-smoke.sh
```

The privileged Btrfs integration test needs `btrfs-progs` and mount permission:

```sh
bash scripts/dev-loopback-btrfs-test.sh
```

GitHub-hosted runners are not reliable for privileged loop/Btrfs mounts. CI
validates the script syntax, while the real integration test must run on a VM
or host that exposes loop and Btrfs kernel support.

The destructive root rollback E2E must run only inside a disposable Btrfs VM.
It stages rollback for the currently booted root, reboots, verifies the booted
rollback prompt state, reverts to the return anchor, and reboots again:

```sh
printf '%s\n' 'BTRFS_MANAGER_DISPOSABLE_VM=1' | sudo tee /etc/btrfs-manager-disposable-vm
bash scripts/vm-root-rollback-e2e.sh --yes
```

## Blocking quality gates

The required GitHub checks are:

- `Rustfmt`: formatting is deterministic and mandatory.
- `Clippy`: Rust static analysis for non-GUI code with warnings denied.
- `Clippy GUI`: GUI static analysis with GTK/libadwaita enabled.
- `Unit Tests`: unit and module tests across the workspace.
- `GUI Check`: compile-time GUI validation.
- `Headless GUI E2E`: starts the GUI under Xvfb with `--check-gui`.
- `Loopback Btrfs Integration`: real Btrfs loopback snapshot workflow.
- `Dependency Security`: `cargo deny check` for advisories, licenses, bans, and sources.
- `Secret Scan`: Gitleaks prevents tokens, keys, and credentials from entering history.
- `Coverage Ratchet`: `cargo llvm-cov` must stay at or above `quality/baseline.json`.
- `Complexity And Duplication Ratchet`: static metrics cannot regress from baseline.
- `TDD Regression Evidence`: source changes must include tests or an explicit justification.
- `Documentation` and `GUI Documentation`: rustdoc and Markdown validation.
- `Shell And Packaging`: shellcheck, Polkit XML, and desktop file validation.
- `AUR Package`: validates the Arch `PKGBUILD`, checks committed `.SRCINFO`,
  and runs `namcap` in an Arch container.
- `Analyze`: CodeQL scans Rust and shell-relevant changes.

## Quality ratchet

`scripts/quality-gate.py` collects dependency-free metrics:

- Rust file count, logical lines, and total lines.
- Maximum file size.
- Maximum function size.
- Approximate function cyclomatic complexity.
- Duplicate code windows.
- Line coverage when `lcov.info` exists.

The baseline lives in `quality/baseline.json`. CI compares the current report
against that file and fails if any blocking metric gets worse. This means a PR
cannot make the largest function larger, increase maximum complexity, increase
duplication, or reduce coverage.

Generate a local report:

```sh
python3 scripts/quality-gate.py collect --output quality/report.json
```

Check against the committed baseline:

```sh
python3 scripts/quality-gate.py check --write-report
```

After a deliberate refactor that improves the code, update the baseline in the
same PR:

```sh
python3 scripts/quality-gate.py collect --output quality/baseline.json
```

Do not relax the baseline to hide a regression. If a temporary exception is
unavoidable, document the reason in the PR and add a follow-up issue.

## Test strategy

Unit tests cover pure logic in `crates/core` and deterministic helper behavior.
They are the preferred place for parser, retention, rollback, and path tests.

Integration tests cover module boundaries and real Btrfs behavior. The loopback
script creates an isolated filesystem image, exercises helper commands, checks
snapshot properties, and verifies cleanup behavior.

Regression tests are required for bug fixes. The TDD gate checks whether Rust
source changes also changed a test, fixture, integration script, or quality
artifact. When the gate cannot infer the evidence automatically, use the
`QUALITY_TDD_JUSTIFICATION` environment variable in the PR workflow only with a
clear explanation.

E2E tests are intentionally small. `scripts/e2e-headless-smoke.sh` validates that
the GTK application starts in a headless display and exits cleanly.

## Security gates

Security is split into three layers:

- `cargo deny`: dependency advisories, yanked crates, licenses, duplicate
  versions, and registry/source policy.
- `gitleaks`: accidental secrets in commits and PRs.
- `CodeQL`: semantic static analysis on Rust and workflow-sensitive changes.

Privileged behavior must remain inside `crates/helper`. GUI code should call the
helper through the D-Bus/polkit boundary and must not shell out to privileged
Btrfs commands directly.

## Mutation testing

Mutation testing is expensive, so it is scheduled weekly and available through
manual workflow dispatch in `.github/workflows/mutation.yml`.

The mutation report is not a PR blocker yet. Use it to find weak assertions and
promote important survivors into regular unit or integration tests.

## GitHub setup

The repository bootstrap script configures the remote and branch protection:

```sh
scripts/github-bootstrap.sh
```

It reads `GH_TOKEN` or `GITHUB_TOKEN` from the environment or `.env` at runtime.
The script does not print the token, and `.env` is ignored by Git.

Required tools for the bootstrap step:

- `gh`
- `jq`
- GitHub token with repository administration permission

## Review focus

Review architecture before syntax:

- Keep the GUI unprivileged.
- Keep helper commands narrow, auditable, and covered by regression tests.
- Validate every path before command execution.
- Preserve rollback reversibility and read-only snapshot defaults.
- Keep D-Bus methods stable and explicit.
- Avoid growing `handle`-style dispatch functions further without extracting
  focused units.
- Raise the quality baseline only when metrics improve.
