#!/usr/bin/env python3
"""Create a normalized, commit-bound, whitelisted Threadbeat plugin archive."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

from release_common import (
    REPOSITORY,
    ReleaseValidationError,
    cargo_metadata,
    component_ref,
    dependency_graph,
    git_blob,
    git_tree,
    normalize_spdx_expression,
    package_purl,
    require_semver,
    safe_relative_path,
    selected_release_files,
    stable_package_identity,
)

TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}
SAFE_COMPONENT = re.compile(r"[^A-Za-z0-9._-]+")
LICENSE_NAMES = re.compile(r"^(?:licen[cs]e|copying|notice)(?:[._-].*)?$", re.IGNORECASE)
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
MAX_CRATE_BYTES = 100 * 1024 * 1024
MAX_LICENSE_BYTES = 4 * 1024 * 1024
REVIEWED_LICENSE_FALLBACKS = {
    (
        "rmcp",
        "3.1.4",
        CRATES_IO_SOURCE,
        "1a15bc53261a9dc37e105df006e4656c598379a8f9581f8950debb130f27a7cf",
    ): {
        "declared_license": "Apache-2.0",
        "license_path": "release/license-fallbacks/rmcp-3.1.4-LICENSE",
        "license_sha256": "0382b0057770ca05e9c350a50aa3b1c1fea84da0bc81d723bf00b9aa841be58a",
        "upstream_commit": "4a738b9dd99eaca418b614afa433a0cbdaf8d056",
        "upstream_tag": "rmcp-v3.1.4",
    },
    (
        "rmcp-macros",
        "3.1.4",
        CRATES_IO_SOURCE,
        "a85d45508e9b4ba024fe996c2638799635d75b6dd0ba8f32ccf08f8026f0c780",
    ): {
        "declared_license": "Apache-2.0",
        "license_path": "release/license-fallbacks/rmcp-3.1.4-LICENSE",
        "license_sha256": "0382b0057770ca05e9c350a50aa3b1c1fea84da0bc81d723bf00b9aa841be58a",
        "upstream_commit": "4a738b9dd99eaca418b614afa433a0cbdaf8d056",
        "upstream_tag": "rmcp-macros-v3.1.4",
    },
}


def fail(message: str) -> None:
    print(f"release packaging failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(command: list[str], repo: Path, *, text: bool = True) -> str | bytes:
    result = subprocess.run(command, cwd=repo, check=True, capture_output=True, text=text)
    return result.stdout


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def lock_checksums(repo: Path, tree: dict[str, object]) -> dict[tuple[str, str, str], str]:
    entry = tree.get("Cargo.lock")
    if entry is None:
        fail("Cargo.lock is not tracked at HEAD")
    text = git_blob(repo, entry).decode("utf-8")  # type: ignore[arg-type]
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


def crate_archive_path(package: dict[str, object]) -> Path:
    source_dir = Path(str(package["manifest_path"])).parent
    registry_hash = source_dir.parent.name
    registry_root = source_dir.parent.parent.parent
    return (
        registry_root
        / "cache"
        / registry_hash
        / f"{package['name']}-{package['version']}.crate"
    )


def license_files_from_crate(
    package: dict[str, object], expected_checksum: str
) -> list[tuple[str, bytes]]:
    if package.get("source") != CRATES_IO_SOURCE:
        fail(f"no immutable license extraction policy for source: {package.get('source')}")
    archive_path = crate_archive_path(package)
    if archive_path.is_symlink() or not archive_path.is_file():
        fail(f"missing regular Cargo crate archive: {archive_path.name}")
    if archive_path.stat().st_size > MAX_CRATE_BYTES:
        fail(f"Cargo crate archive is unexpectedly large: {archive_path.name}")
    if file_sha256(archive_path) != expected_checksum:
        fail(f"Cargo crate checksum differs from Cargo.lock: {archive_path.name}")

    root = f"{package['name']}-{package['version']}"
    payloads: list[tuple[str, bytes]] = []
    seen: set[str] = set()
    with tarfile.open(archive_path, mode="r:gz") as archive:
        for member in archive.getmembers():
            parts = PurePosixPath(member.name).parts
            if len(parts) != 2 or parts[0] != root or not LICENSE_NAMES.match(parts[1]):
                continue
            if not member.isfile() or member.issym() or member.islnk():
                fail(f"crate license entry is not a regular file: {member.name}")
            if member.size > MAX_LICENSE_BYTES:
                fail(f"crate license entry is unexpectedly large: {member.name}")
            if parts[1] in seen:
                fail(f"crate archive has duplicate license entry: {member.name}")
            handle = archive.extractfile(member)
            if handle is None:
                fail(f"could not read crate license entry: {member.name}")
            seen.add(parts[1])
            payloads.append((parts[1], handle.read()))
    return sorted(payloads)


def release_metadata(
    repo: Path,
    tree: dict[str, object],
    target: str,
    version: str,
    epoch: int,
    binary: bytes,
) -> dict[str, tuple[bytes, int]]:
    metadata = cargo_metadata(repo, target, offline=True)
    packages, reachable, edges, root_id = dependency_graph(metadata)
    checksums = lock_checksums(repo, tree)
    components: list[dict[str, object]] = []
    notices: list[tuple[str, str, str, str, list[str]]] = []
    license_packages: list[dict[str, object]] = []
    generated: dict[str, tuple[bytes, int]] = {}

    ordered_ids = sorted(
        reachable,
        key=lambda item: (
            str(packages[item]["name"]),
            str(packages[item]["version"]),
            stable_package_identity(packages[item]),
        ),
    )
    for package_id in ordered_ids:
        package = packages[package_id]
        name = str(package["name"])
        package_version = str(package["version"])
        raw_license = str(package.get("license") or "")
        try:
            license_expression = normalize_spdx_expression(raw_license)
        except ReleaseValidationError as error:
            fail(f"{name} {package_version}: {error}")
        component: dict[str, object] = {
            "type": "application" if package_id == root_id else "library",
            "bom-ref": component_ref(package),
            "name": name,
            "version": package_version,
            "purl": package_purl(package),
            "licenses": [{"expression": license_expression}],
            "properties": [
                {
                    "name": "threadbeat:cargoSource",
                    "value": str(package.get("source") or REPOSITORY),
                }
            ],
        }
        if package.get("repository"):
            component["externalReferences"] = [
                {"type": "vcs", "url": str(package["repository"])}
            ]
        components.append(component)

        if package_id == root_id:
            continue
        source = str(package.get("source") or "")
        checksum = checksums.get((name, package_version, source))
        if not checksum:
            fail(f"Cargo.lock has no checksum for {name} {package_version} from {source}")
        license_payloads = license_files_from_crate(package, checksum)
        package_key = SAFE_COMPONENT.sub("-", f"{name}-{package_version}").strip("-")
        package_key += "-" + sha256(stable_package_identity(package).encode("utf-8"))[:8]
        included: list[str] = []
        fallback_record: dict[str, object] | None = None

        if not license_payloads:
            fallback = REVIEWED_LICENSE_FALLBACKS.get(
                (name, package_version, source, checksum)
            )
            if fallback is None or raw_license != fallback["declared_license"]:
                fail(
                    f"{name} {package_version} has no crate license file and no exact "
                    f"reviewed fallback for {raw_license} / {checksum}"
                )
            fallback_path = str(fallback["license_path"])
            fallback_entry = tree.get(fallback_path)
            if fallback_entry is None:
                fail(f"reviewed license fallback is not tracked at HEAD: {fallback_path}")
            fallback_data = git_blob(repo, fallback_entry)  # type: ignore[arg-type]
            if sha256(fallback_data) != fallback["license_sha256"]:
                fail(f"reviewed license fallback digest changed: {fallback_path}")
            license_payloads = [("UPSTREAM-LICENSE", fallback_data)]
            upstream_commit = str(fallback["upstream_commit"])
            fallback_record = {
                "schemaVersion": 1,
                "crate": name,
                "version": package_version,
                "registrySource": source,
                "crateChecksumSha256": checksum,
                "declaredLicense": raw_license,
                "reason": "The published crate tarball omitted a top-level license file.",
                "upstreamRepository": "https://github.com/modelcontextprotocol/rust-sdk",
                "upstreamCommit": upstream_commit,
                "upstreamTag": fallback["upstream_tag"],
                "upstreamLicenseUrl": (
                    "https://github.com/modelcontextprotocol/rust-sdk/blob/"
                    f"{upstream_commit}/LICENSE"
                ),
                "licenseTextSha256": fallback["license_sha256"],
            }

        for license_name, license_data in license_payloads:
            destination = f"third-party-licenses/{package_key}/{license_name}"
            try:
                safe_relative_path(destination)
            except ReleaseValidationError as error:
                fail(str(error))
            generated[destination] = (license_data, 0o644)
            included.append(destination)
        if fallback_record is not None:
            destination = f"third-party-licenses/{package_key}/LICENSE-FALLBACK.json"
            generated[destination] = (
                (json.dumps(fallback_record, indent=2, sort_keys=True) + "\n").encode("utf-8"),
                0o644,
            )
            included.append(destination)

        license_packages.append(
            {
                "name": name,
                "version": package_version,
                "source": source,
                "displaySource": str(package.get("repository") or source),
                "crateChecksumSha256": checksum,
                "licenseExpression": license_expression,
                "files": [
                    {"path": path, "sha256": sha256(generated[path][0])}
                    for path in sorted(included)
                ],
            }
        )
        notices.append(
            (
                name,
                package_version,
                license_expression,
                str(package.get("repository") or source),
                sorted(included),
            )
        )

    timestamp = datetime.fromtimestamp(epoch, timezone.utc).isoformat().replace("+00:00", "Z")
    commit = str(run(["git", "rev-parse", "HEAD"], repo)).strip()
    root_package = packages[root_id]
    serial_seed = f"{REPOSITORY}@{commit}:{target}:{version}"
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, serial_seed)}",
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "component": next(
                component for component in components if component["bom-ref"] == component_ref(root_package)
            ),
        },
        "components": [
            component for component in components if component["bom-ref"] != component_ref(root_package)
        ],
        "dependencies": [
            {
                "ref": component_ref(packages[package_id]),
                "dependsOn": [
                    component_ref(packages[item])
                    for item in edges.get(package_id, [])
                    if item in reachable
                ],
            }
            for package_id in sorted(reachable, key=lambda item: component_ref(packages[item]))
        ],
    }
    notices_text = [
        "# Third-party licenses",
        "",
        "This archive includes license files for the normal/build dependencies in the locked, target-filtered Cargo graph. Exact checksum-pinned fallback records are included only when a reviewed published crate omitted its top-level license file.",
        "",
        "| Crate | Version | SPDX license | Source | Included files |",
        "|---|---:|---|---|---|",
    ]
    for name, package_version, expression, source, included in sorted(notices):
        escaped_source = source.replace("|", "%7C")
        files = "<br>".join(f"`{path}`" for path in included)
        notices_text.append(
            f"| `{name}` | `{package_version}` | `{expression}` | {escaped_source} | {files} |"
        )
    notices_text.append("")

    license_manifest = {
        "schemaVersion": 1,
        "target": target,
        "packages": sorted(
            license_packages,
            key=lambda item: (str(item["name"]), str(item["version"]), str(item["source"])),
        ),
    }
    notices_data = ("\n".join(notices_text)).encode("utf-8")
    build_metadata = {
        "schemaVersion": 1,
        "name": "threadbeat",
        "version": version,
        "target": target,
        "sourceRepository": REPOSITORY,
        "sourceCommit": commit,
        "sourceDateEpoch": epoch,
        "buildTimestamp": timestamp,
        "buildTimestampSemantics": "SOURCE_DATE_EPOCH from the source commit, not wall-clock build time",
        "rustc": str(
            run([os.environ.get("RUSTC", "rustc"), "--version", "--verbose"], repo)
        ).strip(),
        "cargo": str(run([os.environ.get("CARGO", "cargo"), "--version"], repo)).strip(),
        "binarySha256": sha256(binary),
        "thirdPartyLicensesMarkdownSha256": sha256(notices_data),
        "archiveMetadataNormalized": True,
        "binaryBitForBitReproducibleClaim": False,
        "rootPackagePurl": package_purl(root_package),
    }
    generated["SBOM.cdx.json"] = (
        (json.dumps(sbom, indent=2, sort_keys=True) + "\n").encode("utf-8"),
        0o644,
    )
    generated["THIRD_PARTY_LICENSES.md"] = (
        notices_data,
        0o644,
    )
    generated["THIRD_PARTY_LICENSES.json"] = (
        (json.dumps(license_manifest, indent=2, sort_keys=True) + "\n").encode("utf-8"),
        0o644,
    )
    generated["BUILD-METADATA.json"] = (
        (json.dumps(build_metadata, indent=2, sort_keys=True) + "\n").encode("utf-8"),
        0o644,
    )
    checkout = str(repo).encode("utf-8")
    for name in ("SBOM.cdx.json", "THIRD_PARTY_LICENSES.json", "BUILD-METADATA.json"):
        if checkout in generated[name][0]:
            fail(f"generated metadata leaks the checkout path: {name}")
    return generated


def add_directory(archive: tarfile.TarFile, name: str, epoch: int) -> None:
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = epoch
    archive.addfile(info)


def add_file(
    archive: tarfile.TarFile, name: str, data: bytes, mode: int, epoch: int
) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = epoch
    archive.addfile(info, fileobj=io.BytesIO(data))


def write_archive(output: Path, files: dict[str, tuple[bytes, int]], epoch: int) -> None:
    directories = {"threadbeat"}
    for relative in files:
        try:
            path = PurePosixPath("threadbeat") / safe_relative_path(relative)
        except ReleaseValidationError as error:
            fail(str(error))
        for parent in path.parents:
            if str(parent) != ".":
                directories.add(str(parent))
    with output.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for directory in sorted(directories, key=lambda value: (value.count("/"), value)):
                    add_directory(archive, directory, epoch)
                for relative, (data, mode) in sorted(files.items()):
                    add_file(archive, f"threadbeat/{relative}", data, mode, epoch)


def verify_archive(
    output: Path, expected: dict[str, tuple[bytes, int]], epoch: int
) -> None:
    expected_names = {"threadbeat"}
    for relative in expected:
        path = PurePosixPath("threadbeat") / relative
        expected_names.add(str(path))
        expected_names.update(str(parent) for parent in path.parents if str(parent) != ".")
    with tarfile.open(output, mode="r:gz") as archive:
        members = archive.getmembers()
        names = [member.name for member in members]
        if len(names) != len(set(names)) or set(names) != expected_names:
            fail("archive member set differs from the exact whitelist")
        for member in members:
            try:
                safe_relative_path(member.name)
            except ReleaseValidationError as error:
                fail(str(error))
            if member.issym() or member.islnk() or not (member.isdir() or member.isfile()):
                fail(f"archive contains an unsupported member type: {member.name}")
            if member.uid != 0 or member.gid != 0 or member.mtime != epoch:
                fail(f"archive metadata is not normalized: {member.name}")
            if member.isfile():
                relative = str(PurePosixPath(member.name).relative_to("threadbeat"))
                handle = archive.extractfile(member)
                if handle is None:
                    fail(f"could not read archive member: {member.name}")
                expected_data, expected_mode = expected[relative]
                if handle.read() != expected_data or member.mode != expected_mode:
                    fail(f"archive file content/mode mismatch: {member.name}")
            elif member.mode != 0o755:
                fail(f"archive directory mode mismatch: {member.name}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    args = parser.parse_args()

    try:
        require_semver(args.version)
    except ReleaseValidationError as error:
        fail(str(error))
    if args.source_date_epoch <= 0:
        fail("SOURCE_DATE_EPOCH must be positive")
    repo = Path(__file__).resolve().parent.parent
    subprocess.run(
        [
            sys.executable,
            str(repo / "scripts/check-release-version.py"),
            args.version,
            "--require-clean",
        ],
        cwd=repo,
        check=True,
    )
    expected_epoch = int(str(run(["git", "show", "-s", "--format=%ct", "HEAD"], repo)).strip())
    if args.source_date_epoch != expected_epoch:
        fail("SOURCE_DATE_EPOCH must equal the HEAD commit timestamp")

    binary_argument = args.binary
    if binary_argument.is_symlink() or not binary_argument.is_file():
        fail("release binary must be a regular non-symlink file")
    binary = binary_argument.resolve().read_bytes()

    try:
        tree = git_tree(repo)
        files = selected_release_files(repo, tree)
    except ReleaseValidationError as error:
        fail(str(error))
    try:
        files.update(
            release_metadata(
                repo,
                tree,  # type: ignore[arg-type]
                args.target,
                args.version,
                args.source_date_epoch,
                binary,
            )
        )
    except ReleaseValidationError as error:
        fail(str(error))
    files["bin/threadbeat"] = (binary, 0o755)

    output_dir_argument = args.output_dir
    if output_dir_argument.is_symlink():
        fail("release output directory must not be a symlink")
    output_dir_argument.mkdir(parents=True, exist_ok=True)
    output_dir = output_dir_argument.resolve()
    output = output_dir / f"threadbeat-plugin-v{args.version}-{args.target}.tar.gz"
    with tempfile.NamedTemporaryFile(
        prefix=f".{output.name}.", dir=output_dir, delete=False
    ) as handle:
        temporary = Path(handle.name)
    temporary.unlink()
    try:
        write_archive(temporary, files, args.source_date_epoch)
        verify_archive(temporary, files, args.source_date_epoch)
        try:
            os.link(temporary, output)
        except FileExistsError:
            fail(f"refusing to overwrite existing artifact: {output}")
    finally:
        temporary.unlink(missing_ok=True)
    print(f"{file_sha256(output)}  {output.name}")
    print(output)


if __name__ == "__main__":
    main()
