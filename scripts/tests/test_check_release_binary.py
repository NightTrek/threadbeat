from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SCRIPT = SCRIPTS / "check-release-binary.py"
SPEC = importlib.util.spec_from_file_location("check_release_binary", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
check_release_binary = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_release_binary)


class CheckReleaseBinaryTests(unittest.TestCase):
    def test_linux_loader_allowlist_is_target_specific(self) -> None:
        x86_needed = check_release_binary.linux_needed_for_target(
            "x86_64-unknown-linux-gnu"
        )
        arm_needed = check_release_binary.linux_needed_for_target(
            "aarch64-unknown-linux-gnu"
        )

        self.assertIn("ld-linux-x86-64.so.2", x86_needed)
        self.assertNotIn("ld-linux-aarch64.so.1", x86_needed)
        self.assertIn("ld-linux-aarch64.so.1", arm_needed)
        self.assertNotIn("ld-linux-x86-64.so.2", arm_needed)

    def test_linux_dependency_policy_accepts_only_the_target_loader(self) -> None:
        dynamic = """
         0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
         0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
        """
        self.assertEqual(
            check_release_binary.unexpected_linux_needed(
                "aarch64-unknown-linux-gnu", dynamic
            ),
            set(),
        )
        self.assertEqual(
            check_release_binary.unexpected_linux_needed(
                "x86_64-unknown-linux-gnu", dynamic
            ),
            {"ld-linux-aarch64.so.1"},
        )

        x86_dynamic = """
         0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
         0x0000000000000001 (NEEDED) Shared library: [ld-linux-x86-64.so.2]
        """
        self.assertEqual(
            check_release_binary.unexpected_linux_needed(
                "x86_64-unknown-linux-gnu", x86_dynamic
            ),
            set(),
        )
        self.assertEqual(
            check_release_binary.unexpected_linux_needed(
                "aarch64-unknown-linux-gnu", x86_dynamic
            ),
            {"ld-linux-x86-64.so.2"},
        )

    def test_linux_dependency_policy_rejects_unknown_libraries(self) -> None:
        dynamic = """
         0x0000000000000001 (NEEDED) Shared library: [libsqlite3.so.0]
         0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.2]
        """
        self.assertEqual(
            check_release_binary.unexpected_linux_needed(
                "aarch64-unknown-linux-gnu", dynamic
            ),
            {"libsqlite3.so.0", "ld-linux-aarch64.so.2"},
        )

    def test_linux_interpreter_is_parsed_from_program_headers(self) -> None:
        headers = "[Requesting program interpreter: /lib/ld-linux-aarch64.so.1]"
        self.assertEqual(
            check_release_binary.linux_interpreters(headers),
            ["/lib/ld-linux-aarch64.so.1"],
        )

    def test_missing_and_duplicate_linux_interpreters_are_detectable(self) -> None:
        self.assertEqual(
            check_release_binary.linux_interpreters("no interpreter here"), []
        )
        headers = """
        [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]
        [Requesting program interpreter: /tmp/ld-linux-x86-64.so.2]
        """
        self.assertEqual(
            check_release_binary.linux_interpreters(headers),
            [
                "/lib64/ld-linux-x86-64.so.2",
                "/tmp/ld-linux-x86-64.so.2",
            ],
        )

    def test_interpreter_policy_paths_are_exact_and_target_specific(self) -> None:
        self.assertEqual(
            check_release_binary.LINUX_INTERPRETERS,
            {
                "x86_64-unknown-linux-gnu": "/lib64/ld-linux-x86-64.so.2",
                "aarch64-unknown-linux-gnu": "/lib/ld-linux-aarch64.so.1",
            },
        )


if __name__ == "__main__":
    unittest.main()
