#!/usr/bin/env python3
"""Check native release binary identity and platform compatibility claims."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

from release_common import ReleaseValidationError, require_semver

EXPECTED_FILE_TEXT = {
    "x86_64-unknown-linux-gnu": ("ELF 64-bit", "x86-64"),
    "aarch64-unknown-linux-gnu": ("ELF 64-bit", "ARM aarch64"),
    "x86_64-apple-darwin": ("Mach-O 64-bit", "x86_64"),
    "aarch64-apple-darwin": ("Mach-O 64-bit", "arm64"),
}
LINUX_NEEDED = {
    "libc.so.6",
    "libdl.so.2",
    "libgcc_s.so.1",
    "libm.so.6",
    "libpthread.so.0",
    "librt.so.1",
}
MACOS_NEEDED = {
    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "/usr/lib/libSystem.B.dylib",
    "/usr/lib/libiconv.2.dylib",
}


def fail(message: str) -> None:
    print(f"release binary check failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def output(command: list[str]) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--target", required=True, choices=sorted(EXPECTED_FILE_TEXT))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()

    try:
        require_semver(args.version)
    except ReleaseValidationError as error:
        fail(str(error))
    binary_argument = args.binary
    if binary_argument.is_symlink() or not binary_argument.is_file():
        fail(f"missing regular non-symlink binary: {binary_argument}")
    binary = binary_argument.resolve()

    identity = output([str(binary), "--version"]).strip()
    if identity != f"threadbeat {args.version}":
        fail(f"unexpected --version output: {identity!r}")

    file_text = output(["file", str(binary)])
    for expected in EXPECTED_FILE_TEXT[args.target]:
        if expected not in file_text:
            fail(f"file(1) output does not contain {expected!r}: {file_text.strip()}")

    if args.target.endswith("linux-gnu"):
        dynamic = output(["readelf", "-d", str(binary)])
        needed = set(re.findall(r"\(NEEDED\).*?\[([^\]]+)\]", dynamic))
        unexpected = needed - LINUX_NEEDED
        if unexpected:
            fail(f"binary has unexpected dynamic dependencies: {sorted(unexpected)}")
        if "RPATH" in dynamic or "RUNPATH" in dynamic:
            fail("binary contains a dynamic loader search path")
        symbols = output(["objdump", "-T", str(binary)])
        versions = [tuple(map(int, match)) for match in re.findall(r"GLIBC_(\d+)\.(\d+)", symbols)]
        if not versions:
            fail("no GLIBC symbol versions were found")
        maximum = max(versions)
        if maximum > (2, 35):
            fail(f"maximum GLIBC symbol is {maximum[0]}.{maximum[1]}, expected <= 2.35")
        print(f"maximum GLIBC symbol: {maximum[0]}.{maximum[1]}")
    else:
        dynamic = output(["otool", "-L", str(binary)])
        dependencies = {
            line.strip().split(" (", 1)[0]
            for line in dynamic.splitlines()[1:]
            if line.strip()
        }
        unexpected = dependencies - MACOS_NEEDED
        if unexpected:
            fail(f"binary has unexpected dynamic dependencies: {sorted(unexpected)}")
        load_commands = output(["otool", "-l", str(binary)])
        if "LC_RPATH" in load_commands:
            fail("binary contains LC_RPATH")
        versions = re.findall(r"\n\s*minos\s+(\d+)\.(\d+)(?:\.(\d+))?", load_commands)
        if not versions:
            fail("LC_BUILD_VERSION minimum macOS version was not found")
        maximum = max(tuple(int(piece or 0) for piece in version) for version in versions)
        if maximum > (11, 0, 0):
            fail(f"minimum macOS version is {maximum}, expected <= 11.0")
        print(f"minimum macOS version: {maximum[0]}.{maximum[1]}")

    print(file_text.strip())


if __name__ == "__main__":
    main()
