#!/usr/bin/env python3
"""Validate one SemVer across Cargo, Cargo.lock, and the plugin manifest."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

from release_common import ReleaseValidationError, require_semver


def fail(message: str) -> None:
    print(f"release version check failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def quoted_assignment(body: str, key: str) -> str | None:
    match = re.search(rf'(?m)^{re.escape(key)}\s*=\s*"([^"]+)"\s*(?:#.*)?$', body)
    return match.group(1) if match else None


def cargo_version(path: Path) -> str | None:
    text = path.read_text(encoding="utf-8")
    table = re.search(r"(?ms)^\[package\]\s*$\n(.*?)(?=^\[|\Z)", text)
    return quoted_assignment(table.group(1), "version") if table else None


def lock_versions(path: Path) -> list[str | None]:
    text = path.read_text(encoding="utf-8")
    blocks = re.split(r"(?m)^\[\[package\]\]\s*$", text)[1:]
    return [
        quoted_assignment(block, "version")
        for block in blocks
        if quoted_assignment(block, "name") == "threadbeat"
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version", nargs="?")
    parser.add_argument("--print-current", action="store_true")
    parser.add_argument("--require-clean", action="store_true")
    args = parser.parse_args()

    if args.print_current == (args.version is not None):
        fail("provide exactly one of VERSION or --print-current")

    repo = Path(__file__).resolve().parent.parent
    with (repo / ".codex-plugin/plugin.json").open(encoding="utf-8") as handle:
        plugin = json.load(handle)

    versions = {
        "Cargo.toml": cargo_version(repo / "Cargo.toml"),
        ".codex-plugin/plugin.json": plugin.get("version"),
    }
    lock_matches = lock_versions(repo / "Cargo.lock")
    if len(lock_matches) != 1:
        fail("Cargo.lock must contain exactly one Threadbeat package")
    versions["Cargo.lock"] = lock_matches[0]

    expected = args.version
    if args.print_current:
        unique = set(versions.values())
        if len(unique) != 1:
            fail("version files disagree: " + ", ".join(f"{path}={value!r}" for path, value in versions.items()))
        expected = unique.pop()
    if not isinstance(expected, str):
        fail(f"{expected!r} is not a string version")
    try:
        require_semver(expected)
    except ReleaseValidationError as error:
        fail(str(error))

    mismatches = [f"{path}={version!r}" for path, version in versions.items() if version != expected]
    if mismatches:
        fail(f"expected {expected!r}; " + ", ".join(mismatches))

    if args.require_clean:
        status = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=all"],
            cwd=repo,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        if status:
            fail("Git worktree is not clean")

    print(expected)


if __name__ == "__main__":
    main()
