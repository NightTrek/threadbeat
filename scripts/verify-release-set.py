#!/usr/bin/env python3
"""Verify the exact, commit-bound four-platform release set without extraction."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import stat
import subprocess
import sys
import tarfile
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

from release_common import (
    REPOSITORY,
    ReleaseValidationError,
    cargo_metadata,
    component_ref,
    dependency_graph,
    normalize_spdx_expression,
    package_purl,
    require_semver,
    safe_relative_path,
    selected_release_files,
)

TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
)
GENERATED_MODES = {
    "threadbeat/BUILD-METADATA.json": 0o644,
    "threadbeat/SBOM.cdx.json": 0o644,
    "threadbeat/THIRD_PARTY_LICENSES.json": 0o644,
    "threadbeat/THIRD_PARTY_LICENSES.md": 0o644,
    "threadbeat/bin/threadbeat": 0o755,
}
MAX_ARCHIVE_BYTES = 100 * 1024 * 1024
MAX_MEMBER_COUNT = 4096
MAX_MEMBER_BYTES = 64 * 1024 * 1024
MAX_EXPANDED_BYTES = 256 * 1024 * 1024
MAX_JSON_BYTES = 16 * 1024 * 1024
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def fail(message: str) -> None:
    print(f"release set verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def archive_names(version: str) -> list[str]:
    return [f"threadbeat-plugin-v{version}-{target}.tar.gz" for target in TARGETS]


def regular_files(directory: Path) -> set[str]:
    names: set[str] = set()
    for path in directory.iterdir():
        if path.is_symlink() or not path.is_file():
            fail(f"release directory contains a non-regular entry: {path.name}")
        names.add(path.name)
    return names


def read_member(
    archive: tarfile.TarFile, members: dict[str, tarfile.TarInfo], name: str
) -> bytes:
    member = members.get(name)
    if member is None or not member.isfile():
        fail(f"missing regular archive member: {name}")
    if member.size > MAX_JSON_BYTES and name.endswith(".json"):
        fail(f"JSON archive member is unexpectedly large: {name}")
    handle = archive.extractfile(member)
    if handle is None:
        fail(f"could not read archive member: {name}")
    return handle.read()


def read_json(
    archive: tarfile.TarFile, members: dict[str, tarfile.TarInfo], name: str
) -> dict[str, object]:
    try:
        value = json.loads(read_member(archive, members, name))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON in {name}: {error}")
    if not isinstance(value, dict):
        fail(f"JSON root must be an object: {name}")
    return value


def lock_checksums(lock_data: bytes) -> dict[tuple[str, str, str], str]:
    text = lock_data.decode("utf-8")
    blocks = re.split(r"(?m)^\[\[package\]\]\s*$", text)[1:]
    checksums: dict[tuple[str, str, str], str] = {}
    for block in blocks:
        values: dict[str, str] = {}
        for key in ("name", "version", "source", "checksum"):
            match = re.search(rf'(?m)^{key}\s*=\s*"([^"]+)"\s*$', block)
            if match:
                values[key] = match.group(1)
        if all(key in values for key in ("name", "version", "source", "checksum")):
            identity = (values["name"], values["version"], values["source"])
            if identity in checksums:
                fail(f"Cargo.lock has a duplicate package identity: {identity}")
            checksums[identity] = values["checksum"]
    return checksums


def expected_cargo_graph(
    repo: Path, target: str
) -> tuple[
    dict[str, dict[str, str]],
    dict[str, list[str]],
    str,
    dict[tuple[str, str, str], str],
]:
    try:
        packages, reachable, edges, root_id = dependency_graph(
            cargo_metadata(repo, target, offline=False)
        )
    except (ReleaseValidationError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        fail(f"could not reconstruct locked Cargo graph for {target}: {error}")
    expected_components: dict[str, dict[str, str]] = {}
    expected_libraries: dict[tuple[str, str, str], str] = {}
    for package_id in reachable:
        package = packages[package_id]
        try:
            reference = component_ref(package)
            expression = normalize_spdx_expression(str(package.get("license") or ""))
        except ReleaseValidationError as error:
            fail(str(error))
        source = str(package.get("source") or REPOSITORY)
        expected_components[reference] = {
            "name": str(package["name"]),
            "version": str(package["version"]),
            "purl": package_purl(package),
            "source": source,
            "license": expression,
            "type": "application" if package_id == root_id else "library",
        }
        if package_id != root_id:
            identity = (str(package["name"]), str(package["version"]), source)
            if identity in expected_libraries:
                fail(f"locked Cargo graph has a duplicate package identity: {identity}")
            expected_libraries[identity] = expression
    expected_edges = {
        component_ref(packages[package_id]): sorted(
            component_ref(packages[dependency_id])
            for dependency_id in edges.get(package_id, [])
            if dependency_id in reachable
        )
        for package_id in reachable
    }
    return (
        expected_components,
        expected_edges,
        component_ref(packages[root_id]),
        expected_libraries,
    )


def license_paths(
    manifest: dict[str, object],
    target: str,
    expected_libraries: dict[tuple[str, str, str], str],
    expected_checksums: dict[tuple[str, str, str], str],
) -> dict[str, str]:
    if manifest.get("schemaVersion") != 1 or manifest.get("target") != target:
        fail("third-party license manifest identity mismatch")
    packages = manifest.get("packages")
    if not isinstance(packages, list) or not packages:
        fail("third-party license manifest has no packages")
    paths: dict[str, str] = {}
    identities: set[tuple[str, str, str]] = set()
    for package in packages:
        if not isinstance(package, dict):
            fail("third-party license package entry must be an object")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        display_source = package.get("displaySource")
        if not all(isinstance(value, str) and value for value in (name, version, source)):
            fail("third-party package has an invalid identity")
        if not isinstance(display_source, str) or not display_source:
            fail("third-party package has no display source")
        identity = (name, version, source)
        if identity in identities:
            fail(f"duplicate third-party package identity: {identity}")
        identities.add(identity)
        expression = package.get("licenseExpression")
        if expression != expected_libraries.get(identity):
            fail(f"third-party package differs from the locked graph: {identity}")
        checksum = package.get("crateChecksumSha256")
        if (
            checksum != expected_checksums.get(identity)
            or not isinstance(checksum, str)
            or not SHA256.fullmatch(checksum)
        ):
            fail(f"third-party checksum differs from Cargo.lock: {identity}")
        files = package.get("files")
        if not isinstance(files, list) or not files:
            fail("third-party package has no license files")
        for file_entry in files:
            if not isinstance(file_entry, dict):
                fail("third-party license file entry must be an object")
            path = file_entry.get("path")
            file_digest = file_entry.get("sha256")
            if not isinstance(path, str) or not path.startswith("third-party-licenses/"):
                fail(f"invalid third-party license path: {path!r}")
            try:
                safe_relative_path(path)
            except ReleaseValidationError as error:
                fail(str(error))
            if not isinstance(file_digest, str) or not SHA256.fullmatch(file_digest):
                fail(f"invalid third-party license digest: {path}")
            full_path = f"threadbeat/{path}"
            if full_path in paths:
                fail(f"duplicate third-party license path: {path}")
            paths[full_path] = file_digest
    if identities != set(expected_libraries):
        fail("third-party license package set differs from the locked Cargo graph")
    return paths


def validate_sbom(
    sbom: dict[str, object],
    version: str,
    expected_timestamp: str,
    expected_components: dict[str, dict[str, str]],
    expected_edges: dict[str, list[str]],
    expected_root_ref: str,
) -> None:
    if sbom.get("bomFormat") != "CycloneDX" or sbom.get("specVersion") != "1.5":
        fail("invalid CycloneDX SBOM identity")
    metadata = sbom.get("metadata")
    if not isinstance(metadata, dict) or metadata.get("timestamp") != expected_timestamp:
        fail("CycloneDX metadata timestamp mismatch")
    root = metadata.get("component")
    if not isinstance(root, dict):
        fail("CycloneDX root component is missing")
    if root.get("name") != "threadbeat" or root.get("version") != version:
        fail("CycloneDX root component identity mismatch")
    if root.get("bom-ref") != expected_root_ref:
        fail("CycloneDX root component reference mismatch")
    components = sbom.get("components")
    if not isinstance(components, list):
        fail("CycloneDX components must be an array")
    all_components = [root, *components]
    actual_components: dict[str, dict[str, object]] = {}
    for component in all_components:
        if not isinstance(component, dict):
            fail("CycloneDX component must be an object")
        reference = component.get("bom-ref")
        if not isinstance(reference, str) or reference in actual_components:
            fail("CycloneDX component has a missing or duplicate bom-ref")
        actual_components[reference] = component
    if set(actual_components) != set(expected_components):
        fail("CycloneDX component set differs from the locked Cargo graph")
    for reference, component in actual_components.items():
        expected = expected_components[reference]
        for field in ("name", "version", "purl", "type"):
            if component.get(field) != expected[field]:
                fail(f"CycloneDX component {field} mismatch: {reference}")
        licenses = component.get("licenses")
        if not isinstance(licenses, list) or len(licenses) != 1:
            fail(f"CycloneDX component has invalid licenses: {component.get('name')}")
        license_entry = licenses[0]
        if (
            not isinstance(license_entry, dict)
            or license_entry.get("expression") != expected["license"]
        ):
            fail(f"CycloneDX component has invalid SPDX expression: {component.get('name')}")
        properties = component.get("properties")
        if properties != [
            {"name": "threadbeat:cargoSource", "value": expected["source"]}
        ]:
            fail(f"CycloneDX component source mismatch: {reference}")

    dependencies = sbom.get("dependencies")
    if not isinstance(dependencies, list):
        fail("CycloneDX dependencies must be an array")
    actual_edges: dict[str, list[str]] = {}
    for edge in dependencies:
        if not isinstance(edge, dict) or not isinstance(edge.get("ref"), str):
            fail("CycloneDX dependency edge is invalid")
        reference = edge["ref"]
        depends_on = edge.get("dependsOn")
        if reference in actual_edges or not isinstance(depends_on, list):
            fail("CycloneDX dependency edge is duplicate or malformed")
        if not all(isinstance(item, str) for item in depends_on):
            fail("CycloneDX dependency target is invalid")
        actual_edges[reference] = sorted(depends_on)
    if actual_edges != expected_edges:
        fail("CycloneDX dependency graph differs from the locked Cargo graph")
    encoded = json.dumps(sbom, sort_keys=True)
    if "file://" in encoded or "/Users/" in encoded or "/home/runner/" in encoded:
        fail("CycloneDX SBOM contains a checkout-path reference")


def expected_member_names(file_modes: dict[str, int]) -> set[str]:
    names = set(file_modes)
    for name in file_modes:
        path = PurePosixPath(name)
        names.update(str(parent) for parent in path.parents if str(parent) != ".")
    return names


def verify_archive(
    repo: Path,
    path: Path,
    version: str,
    target: str,
    expected_commit: str,
    expected_epoch: int,
    source_files: dict[str, tuple[bytes, int]],
) -> None:
    if path.stat().st_size > MAX_ARCHIVE_BYTES:
        fail(f"archive is unexpectedly large: {path.name}")
    expected_timestamp = (
        datetime.fromtimestamp(expected_epoch, timezone.utc)
        .isoformat()
        .replace("+00:00", "Z")
    )
    expected_components, expected_edges, expected_root_ref, expected_libraries = (
        expected_cargo_graph(repo, target)
    )
    lock_data = source_files.get("Cargo.lock")
    if lock_data is None:
        fail("Cargo.lock is absent from the release source set")
    expected_checksums = lock_checksums(lock_data[0])
    with tarfile.open(path, mode="r:gz") as archive:
        member_list = archive.getmembers()
        if len(member_list) > MAX_MEMBER_COUNT:
            fail(f"archive has too many members: {path.name}")
        names = [member.name for member in member_list]
        if len(names) != len(set(names)):
            fail(f"archive has duplicate member names: {path.name}")
        members = {member.name: member for member in member_list}
        expanded = 0
        for member in member_list:
            try:
                candidate = safe_relative_path(member.name)
            except ReleaseValidationError as error:
                fail(str(error))
            if candidate.parts[0] != "threadbeat":
                fail(f"member is outside the threadbeat root: {member.name}")
            if member.issym() or member.islnk() or not (member.isdir() or member.isfile()):
                fail(f"unsupported member type in {path.name}: {member.name}")
            if member.size > MAX_MEMBER_BYTES:
                fail(f"archive member is unexpectedly large: {member.name}")
            expanded += member.size
            if expanded > MAX_EXPANDED_BYTES:
                fail(f"archive expanded size exceeds its bound: {path.name}")
            if (
                member.uid != 0
                or member.gid != 0
                or member.uname != ""
                or member.gname != ""
                or member.mtime != expected_epoch
            ):
                fail(f"non-normalized archive metadata: {member.name}")

        license_manifest = read_json(
            archive, members, "threadbeat/THIRD_PARTY_LICENSES.json"
        )
        third_party = license_paths(
            license_manifest,
            target,
            expected_libraries,
            expected_checksums,
        )
        file_modes = {
            f"threadbeat/{name}": mode for name, (_, mode) in source_files.items()
        }
        file_modes.update(GENERATED_MODES)
        file_modes.update({name: 0o644 for name in third_party})
        expected_names = expected_member_names(file_modes)
        if set(names) != expected_names:
            missing = sorted(expected_names - set(names))[:10]
            extra = sorted(set(names) - expected_names)[:10]
            fail(f"archive member set mismatch: missing={missing} extra={extra}")

        for name, expected_mode in file_modes.items():
            member = members[name]
            if not member.isfile() or stat.S_IMODE(member.mode) != expected_mode:
                fail(f"archive file mode/type mismatch: {name}")
        for name in expected_names - set(file_modes):
            member = members[name]
            if not member.isdir() or stat.S_IMODE(member.mode) != 0o755:
                fail(f"archive directory mode/type mismatch: {name}")

        for relative, (expected_data, _) in source_files.items():
            name = f"threadbeat/{relative}"
            if read_member(archive, members, name) != expected_data:
                fail(f"static release payload differs from expected commit: {name}")
        for name, expected_digest in third_party.items():
            if hashlib.sha256(read_member(archive, members, name)).hexdigest() != expected_digest:
                fail(f"third-party license digest mismatch: {name}")

        binary_data = read_member(archive, members, "threadbeat/bin/threadbeat")
        binary_hash = hashlib.sha256(binary_data).hexdigest()
        build = read_json(archive, members, "threadbeat/BUILD-METADATA.json")
        license_markdown = read_member(
            archive, members, "threadbeat/THIRD_PARTY_LICENSES.md"
        )
        plugin = read_json(archive, members, "threadbeat/.codex-plugin/plugin.json")
        sbom = read_json(archive, members, "threadbeat/SBOM.cdx.json")
        if build.get("version") != version or plugin.get("version") != version:
            fail(f"embedded version mismatch in {path.name}")
        if (
            build.get("target") != target
            or build.get("binarySha256") != binary_hash
            or build.get("sourceCommit") != expected_commit
            or build.get("sourceDateEpoch") != expected_epoch
            or build.get("buildTimestamp") != expected_timestamp
            or build.get("archiveMetadataNormalized") is not True
            or build.get("rootPackagePurl") != f"pkg:cargo/threadbeat@{version}"
            or build.get("thirdPartyLicensesMarkdownSha256")
            != hashlib.sha256(license_markdown).hexdigest()
        ):
            fail(f"embedded build metadata mismatch in {path.name}")
        encoded_build = json.dumps(build, sort_keys=True)
        if "file://" in encoded_build or "/Users/" in encoded_build or "/home/runner/" in encoded_build:
            fail("build metadata contains a checkout-path reference")
        validate_sbom(
            sbom,
            version,
            expected_timestamp,
            expected_components,
            expected_edges,
            expected_root_ref,
        )


def expected_checksums(directory: Path, names: list[str]) -> str:
    return "".join(f"{digest(directory / name)}  {name}\n" for name in sorted(names))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("directory", type=Path)
    parser.add_argument("--expected-commit", required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write-checksums", action="store_true")
    mode.add_argument("--verify-checksums", action="store_true")
    args = parser.parse_args()

    try:
        require_semver(args.version)
    except ReleaseValidationError as error:
        fail(str(error))
    if not re.fullmatch(r"[0-9a-f]{40}", args.expected_commit):
        fail("--expected-commit must be a full lowercase Git SHA-1")
    repo = Path(__file__).resolve().parent.parent
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if head != args.expected_commit:
        fail("verifier checkout does not match --expected-commit")
    expected_epoch = int(
        subprocess.run(
            ["git", "show", "-s", "--format=%ct", "HEAD"],
            cwd=repo,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    try:
        source_files = selected_release_files(repo)
    except ReleaseValidationError as error:
        fail(str(error))

    directory_argument = args.directory
    if directory_argument.is_symlink() or not directory_argument.is_dir():
        fail(f"release directory must be a regular directory: {directory_argument}")
    directory = directory_argument.resolve()
    names = archive_names(args.version)
    expected = set(names)
    if args.verify_checksums:
        expected.add("SHA256SUMS")
    actual = regular_files(directory)
    if actual != expected:
        fail(f"release filenames differ: expected={sorted(expected)} actual={sorted(actual)}")

    for target, name in zip(TARGETS, names, strict=True):
        verify_archive(
            repo,
            directory / name,
            args.version,
            target,
            args.expected_commit,
            expected_epoch,
            source_files,
        )

    checksums = expected_checksums(directory, names)
    checksum_path = directory / "SHA256SUMS"
    if args.write_checksums:
        checksum_path.write_text(checksums, encoding="utf-8")
    elif checksum_path.read_text(encoding="utf-8") != checksums:
        fail("SHA256SUMS does not exactly match the release archives")
    print(f"verified {len(names)} release archives for v{args.version} at {args.expected_commit}")


if __name__ == "__main__":
    main()
