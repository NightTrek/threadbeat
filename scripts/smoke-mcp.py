#!/usr/bin/env python3
"""Perform a strictly bounded MCP handshake and assert the public tool surface."""

from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import BinaryIO

EXPECTED_TOOLS = {
    "automation_doctor",
    "automation_get",
    "automation_list",
    "automation_run",
    "automation_runs",
    "automation_update",
}
MAX_FRAME_BYTES = 1024 * 1024
MAX_STDERR_TAIL_BYTES = 16 * 1024


def fail(message: str) -> None:
    print(f"MCP smoke failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def stderr_tail(handle: BinaryIO) -> str:
    handle.flush()
    end = handle.seek(0, os.SEEK_END)
    handle.seek(max(0, end - MAX_STDERR_TAIL_BYTES))
    return handle.read(MAX_STDERR_TAIL_BYTES).decode("utf-8", errors="replace").strip()


class JsonLineReader:
    def __init__(self, process: subprocess.Popen[bytes], stderr: BinaryIO) -> None:
        if process.stdout is None:
            fail("server stdout pipe was not created")
        self.process = process
        self.stderr = stderr
        self.fd = process.stdout.fileno()
        os.set_blocking(self.fd, False)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.fd, selectors.EVENT_READ)
        self.buffer = bytearray()

    def close(self) -> None:
        self.selector.close()

    def read(self, timeout: float) -> dict[str, object]:
        deadline = time.monotonic() + timeout
        while True:
            newline = self.buffer.find(b"\n")
            if newline >= 0:
                frame = bytes(self.buffer[:newline])
                del self.buffer[: newline + 1]
                try:
                    decoded = json.loads(frame)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    fail(f"invalid JSON response: {error}")
                if not isinstance(decoded, dict):
                    fail("MCP response must be a JSON object")
                return decoded

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                tail = stderr_tail(self.stderr)
                detail = f"; stderr tail: {tail}" if tail else ""
                fail(f"timed out waiting for a complete MCP response{detail}")
            events = self.selector.select(remaining)
            if not events:
                continue
            try:
                chunk = os.read(self.fd, 64 * 1024)
            except BlockingIOError:
                continue
            if not chunk:
                return_code = self.process.poll()
                tail = stderr_tail(self.stderr)
                detail = f"; stderr tail: {tail}" if tail else ""
                fail(f"server closed stdout (exit={return_code}) before a response{detail}")
            self.buffer.extend(chunk)
            if len(self.buffer) > MAX_FRAME_BYTES:
                fail(f"MCP response exceeded {MAX_FRAME_BYTES} bytes")


def send(process: subprocess.Popen[bytes], message: dict[str, object]) -> None:
    if process.stdin is None:
        fail("server stdin pipe was not created")
    encoded = (json.dumps(message, separators=(",", ":")) + "\n").encode("utf-8")
    process.stdin.write(encoded)
    process.stdin.flush()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary_argument = args.binary
    if binary_argument.is_symlink() or not binary_argument.is_file():
        fail("binary must be a regular non-symlink file")
    binary = binary_argument.resolve()

    with tempfile.TemporaryDirectory(prefix="threadbeat-mcp-smoke-") as temporary:
        root = Path(temporary)
        state = root / "state"
        runtime = root / "runtime"
        config = root / "config.toml"
        config.write_text(
            f'stateDir = "{state}"\nruntimeDir = "{runtime}"\n\n'
            "[delivery]\nenabled = false\nstrictCompatibility = true\n",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env["THREADBEAT_CONFIG"] = str(config)
        with tempfile.TemporaryFile(mode="w+b") as stderr:
            process = subprocess.Popen(
                [str(binary), "mcp", "--stdio"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                env=env,
            )
            reader = JsonLineReader(process, stderr)
            try:
                send(
                    process,
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-06-18",
                            "capabilities": {},
                            "clientInfo": {"name": "release-smoke", "version": "1"},
                        },
                    },
                )
                initialized = reader.read(10.0)
                if initialized.get("id") != 1 or not isinstance(initialized.get("result"), dict):
                    fail(f"unexpected initialize response: {initialized}")
                send(process, {"jsonrpc": "2.0", "method": "notifications/initialized"})
                send(process, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
                listed = reader.read(10.0)
                result = listed.get("result")
                if listed.get("id") != 2 or not isinstance(result, dict):
                    fail(f"unexpected tools/list response: {listed}")
                tools = result.get("tools")
                if not isinstance(tools, list) or not all(isinstance(tool, dict) for tool in tools):
                    fail("tools/list did not return an array of tool objects")
                names = {tool.get("name") for tool in tools}
                if names != EXPECTED_TOOLS:
                    fail(f"unexpected tool names: {sorted(str(name) for name in names)}")
                encoded = json.dumps(tools, sort_keys=True)
                if "automation_preview" in encoded or "intentId" in encoded:
                    fail("removed preview/intent surface is still agent-visible")
                for tool in tools:
                    if not tool.get("description"):
                        fail(f"{tool.get('name')} has no description")
                    input_schema = tool.get("inputSchema")
                    if not isinstance(input_schema, dict) or input_schema.get("type") != "object":
                        fail(f"{tool.get('name')} has an invalid input schema")
                    if not isinstance(tool.get("outputSchema"), dict) or not isinstance(
                        tool.get("annotations"), dict
                    ):
                        fail(f"{tool.get('name')} lacks output schema or annotations")
                print("MCP smoke passed with exactly six tools")
            finally:
                reader.close()
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    main()
