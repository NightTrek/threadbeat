from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "smoke-mcp.py"
SPEC = importlib.util.spec_from_file_location("threadbeat_smoke_mcp", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class JsonLineReaderTests(unittest.TestCase):
    def run_reader(self, source: str, timeout: float = 1.0) -> dict[str, object]:
        with tempfile.TemporaryFile(mode="w+b") as stderr:
            process = subprocess.Popen(
                [sys.executable, "-c", source],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=stderr,
            )
            reader = SMOKE.JsonLineReader(process, stderr)
            try:
                return reader.read(timeout)
            finally:
                reader.close()
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
                if process.stdout is not None:
                    process.stdout.close()

    def test_normal_response_can_arrive_in_partial_writes(self) -> None:
        response = self.run_reader(
            "import os,time; os.write(1,b'{\"jsonrpc\":'); "
            "time.sleep(0.02); os.write(1,b'\"2.0\",\"id\":1}\\n')"
        )
        self.assertEqual(response["id"], 1)

    def test_partial_frame_times_out_without_blocking(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.run_reader(
                "import os,time; os.write(1,b'{\"jsonrpc\":'); time.sleep(2)",
                timeout=0.1,
            )

    def test_stderr_flood_does_not_deadlock_stdout(self) -> None:
        response = self.run_reader(
            "import os; os.write(2,b'x'*(2*1024*1024)); "
            "os.write(1,b'{\"jsonrpc\":\"2.0\",\"id\":2}\\n')"
        )
        self.assertEqual(response["id"], 2)

    def test_oversized_unterminated_frame_is_rejected(self) -> None:
        source = "import os; os.write(1,b'x'*(%d+1))" % SMOKE.MAX_FRAME_BYTES
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.run_reader(source)

    def test_early_exit_reports_failure(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.run_reader("import sys; print('boom',file=sys.stderr)")


if __name__ == "__main__":
    unittest.main()
