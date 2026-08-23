"""Shared, side-effect-free validation helpers for Threadbeat release scripts."""

from __future__ import annotations

import json
import os
import re
import subprocess
import uuid
from collections import deque
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from urllib.parse import quote

REPOSITORY = "https://github.com/NightTrek/threadbeat"

SEMVER = re.compile(
    r"^(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)"
    r"(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)

LEGACY_LICENSE_EXPRESSIONS = {
    "MIT/Apache-2.0": "MIT OR Apache-2.0",
}

# Dependency changes that add a new expression must receive an explicit license
# policy review instead of silently expanding the release allowlist.
REVIEWED_SPDX_EXPRESSIONS = {
    "(MIT OR Apache-2.0) AND Unicode-3.0",
    "Apache-2.0",
    "Apache-2.0 OR MIT",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MIT",
    "MIT OR Apache-2.0",
    "MPL-2.0",
    "Unicode-3.0",
    "Unlicense OR MIT",
    "Zlib",
}


class ReleaseValidationError(ValueError):
    """A release input violated a fail-closed repository policy."""


@dataclass(frozen=True)
class GitBlob:
    mode: int
    object_id: str


def require_semver(value: str) -> str:
    if not SEMVER.fullmatch(value):
        raise ReleaseValidationError(f"{value!r} is not strict SemVer")
    return value


def normalize_spdx_expression(value: str) -> str:
    normalized = LEGACY_LICENSE_EXPRESSIONS.get(value, value)
    if normalized not in REVIEWED_SPDX_EXPRESSIONS:
        raise ReleaseValidationError(f"unreviewed or invalid SPDX license expression: {value!r}")
    return normalized


def cargo_metadata(repo: Path, target: str, *, offline: bool) -> dict[str, object]:
    command = [
        os.environ.get("CARGO", "cargo"),
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        target,
    ]
    if offline:
        command.insert(3, "--offline")
    encoded = subprocess.run(
        command,
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    value = json.loads(encoded)
    if not isinstance(value, dict):
        raise ReleaseValidationError("Cargo metadata root is not an object")
    return value


def dependency_graph(
    metadata: dict[str, object],
) -> tuple[dict[str, dict[str, object]], set[str], dict[str, list[str]], str]:
    raw_packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(raw_packages, list) or not isinstance(resolve, dict):
        raise ReleaseValidationError("Cargo metadata has no resolved package graph")
    root = resolve.get("root")
    raw_nodes = resolve.get("nodes")
    if not isinstance(root, str) or not isinstance(raw_nodes, list):
        raise ReleaseValidationError("Cargo metadata has no resolved root package")
    packages: dict[str, dict[str, object]] = {}
    for package in raw_packages:
        if not isinstance(package, dict) or not isinstance(package.get("id"), str):
            raise ReleaseValidationError("Cargo metadata contains an invalid package")
        packages[package["id"]] = package
    nodes: dict[str, dict[str, object]] = {}
    for node in raw_nodes:
        if not isinstance(node, dict) or not isinstance(node.get("id"), str):
            raise ReleaseValidationError("Cargo metadata contains an invalid resolve node")
        nodes[node["id"]] = node
    if root not in packages or root not in nodes:
        raise ReleaseValidationError("Cargo metadata root is absent from the graph")

    reachable = {root}
    edges: dict[str, list[str]] = {}
    queue: deque[str] = deque([root])
    while queue:
        package_id = queue.popleft()
        dependencies: list[str] = []
        raw_dependencies = nodes[package_id].get("deps", [])
        if not isinstance(raw_dependencies, list):
            raise ReleaseValidationError("Cargo metadata contains invalid dependencies")
        for dependency in raw_dependencies:
            if not isinstance(dependency, dict) or not isinstance(dependency.get("pkg"), str):
                raise ReleaseValidationError("Cargo metadata contains an invalid dependency")
            kinds = dependency.get("dep_kinds", [])
            if not isinstance(kinds, list):
                raise ReleaseValidationError("Cargo metadata contains invalid dependency kinds")
            if kinds and not any(
                isinstance(kind, dict) and kind.get("kind") != "dev" for kind in kinds
            ):
                continue
            dependency_id = dependency["pkg"]
            if dependency_id not in packages or dependency_id not in nodes:
                raise ReleaseValidationError("Cargo dependency is absent from the package graph")
            dependencies.append(dependency_id)
            if dependency_id not in reachable:
                reachable.add(dependency_id)
                queue.append(dependency_id)
        edges[package_id] = sorted(set(dependencies))
    return packages, reachable, edges, root


def package_purl(package: dict[str, object]) -> str:
    return f"pkg:cargo/{quote(str(package['name']), safe='')}@{quote(str(package['version']), safe='')}"


def stable_package_identity(package: dict[str, object]) -> str:
    source = package.get("source")
    if source is None:
        if package.get("name") != "threadbeat":
            raise ReleaseValidationError(
                f"unreviewed path dependency in release graph: {package.get('name')}"
            )
        source = REPOSITORY
    return f"{source}|{package_purl(package)}"


def component_ref(package: dict[str, object]) -> str:
    return f"urn:threadbeat:cargo:{uuid.uuid5(uuid.NAMESPACE_URL, stable_package_identity(package))}"


def safe_relative_path(value: str) -> PurePosixPath:
    candidate = PurePosixPath(value)
    if (
        candidate.is_absolute()
        or not candidate.parts
        or candidate == PurePosixPath(".")
        or ".." in candidate.parts
    ):
        raise ReleaseValidationError(f"unsafe release path: {value!r}")
    return candidate


def _git(repo: Path, arguments: list[str]) -> bytes:
    return subprocess.run(
        ["git", *arguments],
        cwd=repo,
        check=True,
        capture_output=True,
    ).stdout


def git_tree(repo: Path) -> dict[str, GitBlob]:
    raw = _git(repo, ["ls-tree", "-r", "-z", "--full-tree", "HEAD"])
    tracked: dict[str, GitBlob] = {}
    for record in raw.split(b"\0"):
        if not record:
            continue
        metadata, encoded_path = record.split(b"\t", 1)
        encoded_mode, object_type, encoded_id = metadata.split(b" ", 2)
        path = encoded_path.decode("utf-8")
        safe_relative_path(path)
        mode = int(encoded_mode, 8)
        if object_type != b"blob" or mode not in (0o100644, 0o100755):
            raise ReleaseValidationError(f"release source is not a regular Git file: {path}")
        tracked[path] = GitBlob(
            mode=0o755 if mode == 0o100755 else 0o644,
            object_id=encoded_id.decode("ascii"),
        )
    return tracked


def git_blob(repo: Path, entry: GitBlob) -> bytes:
    return _git(repo, ["cat-file", "blob", entry.object_id])


def selected_release_files(
    repo: Path, tree: dict[str, GitBlob] | None = None
) -> dict[str, tuple[bytes, int]]:
    tracked = tree if tree is not None else git_tree(repo)
    manifest_entry = tracked.get("release/package-files.txt")
    if manifest_entry is None:
        raise ReleaseValidationError("release/package-files.txt is not tracked at HEAD")
    manifest = git_blob(repo, manifest_entry).decode("utf-8")
    selected: dict[str, tuple[bytes, int]] = {}
    for raw_line in manifest.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.endswith("/"):
            safe_relative_path(line[:-1])
            matches = sorted(path for path in tracked if path.startswith(line))
            if not matches:
                raise ReleaseValidationError(f"package directory has no tracked files: {line}")
        else:
            safe_relative_path(line)
            matches = [line] if line in tracked else []
            if not matches:
                raise ReleaseValidationError(f"package file is not tracked at HEAD: {line}")
        for path in matches:
            entry = tracked[path]
            selected[path] = (git_blob(repo, entry), entry.mode)
    return selected
