from __future__ import annotations

import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

from release_common import (  # noqa: E402
    ReleaseValidationError,
    normalize_spdx_expression,
    require_semver,
    safe_relative_path,
)


class ReleaseCommonTests(unittest.TestCase):
    def test_strict_semver_accepts_prerelease(self) -> None:
        self.assertEqual(require_semver("0.1.0-alpha.1"), "0.1.0-alpha.1")

    def test_strict_semver_rejects_leading_zero(self) -> None:
        with self.assertRaises(ReleaseValidationError):
            require_semver("0.01.0")

    def test_legacy_spdx_expression_is_normalized(self) -> None:
        self.assertEqual(
            normalize_spdx_expression("MIT/Apache-2.0"),
            "MIT OR Apache-2.0",
        )

    def test_unreviewed_spdx_expression_fails_closed(self) -> None:
        with self.assertRaises(ReleaseValidationError):
            normalize_spdx_expression("LicenseRef-Unknown")

    def test_release_path_rejects_parent_traversal(self) -> None:
        with self.assertRaises(ReleaseValidationError):
            safe_relative_path("../secret")


if __name__ == "__main__":
    unittest.main()
