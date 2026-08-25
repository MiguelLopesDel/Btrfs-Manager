# Contributing

## Setup

```sh
bash scripts/install-git-hooks.sh   # once per clone: wires .githooks/pre-commit into git
```

## Before every commit (enforced by the pre-commit hook)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
python3 scripts/quality-gate.py check --write-report
```

The hook runs these automatically once installed. They're fast (seconds), so they also run in
CI as a backstop for anyone who skipped the install step.

## Before opening a PR (not run by the hook — slower or environment-dependent)

```sh
cargo test --workspace --all-targets --no-default-features
cargo clippy -p btrfs-manager-app --features gui --all-targets -- -D warnings   # needs GTK4/libadwaita dev packages
cargo deny check                                                                 # needs network (advisory DB)
cargo llvm-cov --workspace --no-default-features --lcov --output-path lcov.info
python3 scripts/quality-gate.py check --write-report --lcov lcov.info
bash scripts/e2e-headless-smoke.sh                                               # needs Xvfb + GTK4/libadwaita
```

All of the above run in CI on every PR to `main` (`.github/workflows/ci.yml`), except
`e2e-headless-smoke.sh`, which is slower and more environment-dependent (real GTK windows under
Xvfb) — it runs nightly and on demand instead of blocking merges
(`.github/workflows/e2e-nightly.yml`). `scripts/dev-loopback-btrfs-test.sh` (real Btrfs loopback
operations) needs a host with loop device + Btrfs kernel support and is not run in CI at all; CI
only validates its shell syntax.

## The quality gate

`python3 scripts/quality-gate.py check` enforces two different kinds of limits:

- **Fixed ceilings** (Sonar-way style — every offender fails, not just the worst one):
  - a tracked file over 450 lines
  - a function over 100 lines, or with cyclomatic complexity over 25

  These are constants in `scripts/quality-gate.py` (`FILE_LINE_LIMIT`, `FUNCTION_LINE_LIMIT`,
  `FUNCTION_COMPLEXITY_LIMIT`), not stored in `quality/baseline.json` — running
  `quality-gate.py collect --output quality/baseline.json` can never loosen them. If your change
  needs to touch a large file, split it into cohesive modules first (see how `crates/helper/src/`
  and `crates/app/src/gui/` are organized — flat sibling files per concern, a subdirectory only
  when there are multiple sibling variants of the same kind of thing, e.g. `gui/dialogs/`).

- **Historical ratchet against `quality/baseline.json`** (can only get stricter over time):
  - test coverage (`line_coverage_percent`) must not regress below the recorded floor
  - duplicated code blocks (`duplication.duplicate_blocks`) must not exceed the recorded count

  After a deliberate improvement (e.g. added tests raised coverage), update the baseline in the
  same PR:

  ```sh
  python3 scripts/quality-gate.py collect --output quality/baseline.json
  ```

  Never edit `quality/baseline.json` by hand to hide a regression.

`quality-gate.py tdd-check` additionally fails a PR that changes `crates/**/*.rs` source without
touching a test file or setting `QUALITY_TDD_JUSTIFICATION` — see `.github/workflows/ci.yml`.

## Dev commands reference

See `CLAUDE.md` for the full list of build/lint/test commands, architecture notes, and the
privilege-boundary invariant (GUI never calls `btrfs`/`mount` directly — everything privileged
goes through `crates/helper` over D-Bus/Polkit).
