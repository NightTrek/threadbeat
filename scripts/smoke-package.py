#!/usr/bin/env python3
"""Launch the MCP server through the command recorded in a completed plugin archive."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

from release_common import ReleaseValidationError, require_semver

MAX_ARCHIVE_BYTES = 100 * 1024 * 1024
MAX_MEMBER_BYTES = 64 * 1024 * 1024


def fail(message: str) -> None:
    print(f"package smoke failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def member_bytes(archive: tarfile.TarFile, name: str) -> bytes:
    matches = archive.getmembers()
    names = [member.name for member in matches]
    if len(names) != len(set(names)):
        fail("archive contains duplicate member names")
    member = next((item for item in matches if item.name == name), None)
    if member is None or not member.isfile() or member.issym() or member.islnk():
        fail(f"missing regular archive member: {name}")
    if member.size > MAX_MEMBER_BYTES:
        fail(f"archive member is unexpectedly large: {name}")
    handle = archive.extractfile(member)
    if handle is None:
        fail(f"could not read archive member: {name}")
    return handle.read()


def json_object(data: bytes, name: str) -> dict[str, object]:
    try:
        value = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON in {name}: {error}")
    if not isinstance(value, dict):
        fail(f"JSON root must be an object: {name}")
    return value


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        require_semver(args.version)
    except ReleaseValidationError as error:
        fail(str(error))
    archive_path = args.archive
    if archive_path.is_symlink() or not archive_path.is_file():
        fail("archive must be a regular non-symlink file")
    if archive_path.stat().st_size > MAX_ARCHIVE_BYTES:
        fail("archive is unexpectedly large")

    with tarfile.open(archive_path, mode="r:gz") as archive:
        plugin = json_object(
            member_bytes(archive, "threadbeat/.codex-plugin/plugin.json"),
            ".codex-plugin/plugin.json",
        )
        mcp = json_object(
            member_bytes(archive, "threadbeat/.mcp.json"),
            ".mcp.json",
        )
        binary = member_bytes(archive, "threadbeat/bin/threadbeat")

    if (
        plugin.get("name") != "threadbeat"
        or plugin.get("version") != args.version
        or plugin.get("mcpServers") != "./.mcp.json"
        or plugin.get("skills") != "./skills/"
    ):
        fail("plugin manifest identity or entry points are invalid")
    servers = mcp.get("mcpServers")
    if not isinstance(servers, dict) or set(servers) != {"threadbeat"}:
        fail("MCP configuration must expose exactly the threadbeat server")
    server = servers["threadbeat"]
    if not isinstance(server, dict) or server != {
        "command": "./bin/threadbeat",
        "args": ["mcp", "--stdio"],
        "cwd": ".",
    }:
        fail("MCP command, arguments, or working directory differ from the reviewed contract")

    with tempfile.TemporaryDirectory(prefix="threadbeat-package-smoke-") as temporary:
        root = Path(temporary) / "threadbeat"
        command = root / str(server["command"])
        command.parent.mkdir(parents=True)
        command.write_bytes(binary)
        command.chmod(0o755)
        resolved_root = root.resolve()
        resolved_command = command.resolve()
        if resolved_command.parent != resolved_root / "bin":
            fail("packaged MCP command resolves outside the expected bin directory")
        subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve().parent / "smoke-mcp.py"),
                "--binary",
                str(resolved_command),
            ],
            cwd=resolved_root,
            check=True,
            env=os.environ.copy(),
        )
    print("packaged plugin manifest and MCP launch smoke passed")


if __name__ == "__main__":
    main()
