#!/usr/bin/env python3
"""Collect and enforce repository quality metrics.

The gate is intentionally dependency-free so it can run locally, in CI, and in
fresh test machines before optional tools such as cargo-llvm-cov or gitleaks are
installed. External tools still feed it artifacts such as lcov.info.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASELINE = ROOT / "quality" / "baseline.json"
DEFAULT_REPORT = ROOT / "quality" / "report.json"
RUST_FILE_RE = re.compile(r".*\.rs$")
FN_RE = re.compile(
    r"\b(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b"
)
COMPLEXITY_RE = re.compile(
    r"\b(if|else\s+if|match|while|for|loop|&&|\|\||\?)\b|&&|\|\||\?"
)
SKIP_DIRS = {
    ".git",
    ".cache",
    "target",
    "quality",
}
TEST_HINTS = (
    "/tests/",
    "_test.rs",
    "test_",
    "tests.rs",
    "dev-loopback-btrfs-test.sh",
    "e2e-headless-smoke.sh",
)


@dataclass(frozen=True)
class RustFunction:
    file: str
    name: str
    start_line: int
    end_line: int
    lines: int
    complexity: int


def repo_path(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def iter_files() -> Iterable[Path]:
    for path in ROOT.rglob("*"):
        if not path.is_file():
            continue
        parts = set(path.relative_to(ROOT).parts)
        if parts.intersection(SKIP_DIRS):
            continue
        yield path


def rust_files() -> list[Path]:
    return sorted(path for path in iter_files() if RUST_FILE_RE.match(path.name))


def source_lines(path: Path) -> list[str]:
    return path.read_text(encoding="utf-8", errors="replace").splitlines()


def count_non_blank_non_comment(lines: Iterable[str]) -> int:
    count = 0
    in_block = False
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        if in_block:
            if "*/" in stripped:
                in_block = False
            continue
        if stripped.startswith("/*"):
            in_block = "*/" not in stripped
            continue
        if stripped.startswith("//"):
            continue
        count += 1
    return count


def extract_functions(path: Path) -> list[RustFunction]:
    lines = source_lines(path)
    functions: list[RustFunction] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = FN_RE.search(line)
        if not match:
            index += 1
            continue

        start = index
        brace_depth = 0
        found_body = False
        end = index
        complexity = 1

        while end < len(lines):
            current = lines[end]
            complexity += len(COMPLEXITY_RE.findall(current))
            if "{" in current:
                found_body = True
            brace_depth += current.count("{")
            brace_depth -= current.count("}")
            if found_body and brace_depth <= 0:
                break
            end += 1

        functions.append(
            RustFunction(
                file=repo_path(path),
                name=match.group(1),
                start_line=start + 1,
                end_line=min(end + 1, len(lines)),
                lines=max(1, min(end + 1, len(lines)) - start),
                complexity=max(1, complexity),
            )
        )
        index = max(end + 1, index + 1)
    return functions


def normalize_for_duplication(line: str) -> str:
    stripped = line.strip()
    stripped = re.sub(r"//.*$", "", stripped)
    stripped = re.sub(r"\s+", " ", stripped)
    return stripped


def duplication_metrics(paths: list[Path], window: int = 8) -> dict[str, float | int]:
    windows: Counter[str] = Counter()
    total_windows = 0
    for path in paths:
        normalized = [
            normalize_for_duplication(line)
            for line in source_lines(path)
            if normalize_for_duplication(line)
        ]
        if len(normalized) < window:
            continue
        for index in range(0, len(normalized) - window + 1):
            chunk = "\n".join(normalized[index : index + window])
            windows[chunk] += 1
            total_windows += 1

    duplicate_windows = sum(count - 1 for count in windows.values() if count > 1)
    duplicate_blocks = sum(1 for count in windows.values() if count > 1)
    ratio = (duplicate_windows / total_windows * 100.0) if total_windows else 0.0
    return {
        "window_size": window,
        "total_windows": total_windows,
        "duplicate_blocks": duplicate_blocks,
        "duplicate_windows": duplicate_windows,
        "duplicate_ratio_percent": round(ratio, 4),
    }


def parse_lcov(path: Path) -> float | None:
    if not path.exists():
        return None
    found = 0
    hit = 0
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if raw.startswith("LF:"):
            found += int(raw.split(":", 1)[1])
        elif raw.startswith("LH:"):
            hit += int(raw.split(":", 1)[1])
    if found == 0:
        return None
    return round(hit / found * 100.0, 4)


def collect(lcov_path: Path | None = None) -> dict[str, object]:
    files = rust_files()
    functions = [function for path in files for function in extract_functions(path)]
    file_line_counts = {repo_path(path): len(source_lines(path)) for path in files}
    logical_lines = sum(count_non_blank_non_comment(source_lines(path)) for path in files)
    largest_files = sorted(
        (
            {"file": file, "lines": lines}
            for file, lines in file_line_counts.items()
        ),
        key=lambda item: item["lines"],
        reverse=True,
    )[:10]
    complex_functions = sorted(
        (
            {
                "file": function.file,
                "name": function.name,
                "line": function.start_line,
                "complexity": function.complexity,
                "lines": function.lines,
            }
            for function in functions
        ),
        key=lambda item: (item["complexity"], item["lines"]),
        reverse=True,
    )[:10]
    longest_functions = sorted(
        (
            {
                "file": function.file,
                "name": function.name,
                "line": function.start_line,
                "lines": function.lines,
                "complexity": function.complexity,
            }
            for function in functions
        ),
        key=lambda item: item["lines"],
        reverse=True,
    )[:10]

    coverage = parse_lcov(lcov_path or ROOT / "lcov.info")
    metrics = {
        "rust_file_count": len(files),
        "rust_logical_lines": logical_lines,
        "rust_total_lines": sum(file_line_counts.values()),
        "rust_function_count": len(functions),
        "max_file_lines": largest_files[0]["lines"] if largest_files else 0,
        "max_function_lines": longest_functions[0]["lines"] if longest_functions else 0,
        "max_function_complexity": complex_functions[0]["complexity"] if complex_functions else 0,
        "duplication": duplication_metrics(files),
        "line_coverage_percent": coverage,
    }
    return {
        "schema": 1,
        "metrics": metrics,
        "top": {
            "largest_files": largest_files,
            "most_complex_functions": complex_functions,
            "longest_functions": longest_functions,
        },
    }


def load_json(path: Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, data: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def metric_at(data: dict[str, object], dotted: str) -> float | int | None:
    current: object = data
    for part in dotted.split("."):
        if not isinstance(current, dict) or part not in current:
            return None
        current = current[part]
    if current is None:
        return None
    if isinstance(current, (int, float)):
        return current
    raise TypeError(f"metric {dotted} is not numeric")


def compare(current: dict[str, object], baseline: dict[str, object]) -> list[str]:
    failures: list[str] = []
    lower_or_equal = [
        "metrics.max_file_lines",
        "metrics.max_function_lines",
        "metrics.max_function_complexity",
        "metrics.duplication.duplicate_ratio_percent",
        "metrics.duplication.duplicate_blocks",
    ]
    greater_or_equal = [
        "metrics.line_coverage_percent",
    ]

    for metric in lower_or_equal:
        actual = metric_at(current, metric)
        allowed = metric_at(baseline, metric)
        if actual is None or allowed is None:
            continue
        if actual > allowed:
            failures.append(f"{metric}: {actual} > baseline {allowed}")

    for metric in greater_or_equal:
        actual = metric_at(current, metric)
        allowed = metric_at(baseline, metric)
        if actual is None or allowed is None:
            continue
        if actual < allowed:
            failures.append(f"{metric}: {actual} < baseline {allowed}")

    return failures


def git(args: list[str], check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=check,
    )


def changed_files(base: str) -> list[str]:
    merge_base = git(["merge-base", base, "HEAD"]).stdout.strip()
    result = git(["diff", "--name-only", f"{merge_base}...HEAD"])
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def tdd_check(base: str) -> int:
    try:
        files = changed_files(base)
    except (subprocess.CalledProcessError, FileNotFoundError) as error:
        print(f"quality-gate: unable to compute git diff for TDD gate: {error}", file=sys.stderr)
        return 0

    source_changes = [
        path
        for path in files
        if path.startswith("crates/")
        and path.endswith(".rs")
        and "/tests/" not in path
        and not path.endswith("_test.rs")
    ]
    test_changes = [
        path
        for path in files
        if any(hint in f"/{path}" for hint in TEST_HINTS)
        or path.startswith("scripts/dev-loopback")
        or path.startswith("quality/")
    ]
    justification = os.environ.get("QUALITY_TDD_JUSTIFICATION", "").strip()
    if source_changes and not test_changes and not justification:
        print("TDD gate failed: Rust source changed without tests or regression evidence.")
        print("Changed source files:")
        for path in source_changes:
            print(f"  - {path}")
        print("Add a focused test/regression script, or set QUALITY_TDD_JUSTIFICATION in CI.")
        return 1

    print("TDD gate passed.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    collect_parser = sub.add_parser("collect", help="collect metrics and write a report")
    collect_parser.add_argument("--output", type=Path, default=DEFAULT_REPORT)
    collect_parser.add_argument("--lcov", type=Path, default=ROOT / "lcov.info")

    check_parser = sub.add_parser("check", help="compare current metrics against a baseline")
    check_parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    check_parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    check_parser.add_argument("--lcov", type=Path, default=ROOT / "lcov.info")
    check_parser.add_argument("--write-report", action="store_true")

    tdd_parser = sub.add_parser("tdd-check", help="enforce test evidence for source changes")
    tdd_parser.add_argument("--base", default=os.environ.get("QUALITY_BASE_REF", "origin/main"))

    args = parser.parse_args()
    if args.command == "collect":
        report = collect(args.lcov)
        write_json(args.output, report)
        print(json.dumps(report["metrics"], indent=2, sort_keys=True))
        return 0

    if args.command == "check":
        report = collect(args.lcov) if args.write_report or not args.report.exists() else load_json(args.report)
        if args.write_report:
            write_json(args.report, report)
        baseline = load_json(args.baseline)
        failures = compare(report, baseline)
        if failures:
            print("Quality ratchet failed:")
            for failure in failures:
                print(f"  - {failure}")
            return 1
        print("Quality ratchet passed.")
        return 0

    if args.command == "tdd-check":
        return tdd_check(args.base)

    return 2


if __name__ == "__main__":
    raise SystemExit(main())
