"""Caller-target staging, including Linux to Windows cross staging."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("staging", Path(__file__).resolve().parents[2] / "scripts/stage-workers.py")
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)


class StagingTests(unittest.TestCase):
    def test_caller_target_names_hashes_and_explicit_test_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir()
            for suffix in ("", ".exe"):
                (runtime / f"publisher-font-worker{suffix}").write_bytes(b"exact worker" + suffix.encode())
            for triple, suffix, magic in (("x86_64-pc-windows-msvc", ".exe", b"MZ"),
                                         ("x86_64-unknown-linux-gnu", "", b"\x7fELF")):
                directory = root / triple / "deps"
                directory.mkdir(parents=True)
                caller = directory / ("client" + suffix)
                caller.write_bytes(magic)
                receipt = staging.stage(runtime, caller, ["font"], True, triple)
                self.assertEqual(len(receipt["workers"]), 2)
                for row in receipt["workers"]:
                    self.assertEqual(row["name"], "publisher-font-worker" + suffix)
                    self.assertEqual(row["sha256"], staging.digest(runtime / row["name"]))
                adjacent = directory / ("publisher-font-worker" + suffix)
                if not suffix and os.name == "posix":
                    self.assertEqual(adjacent.stat().st_mode & 0o777, 0o500)
                self.assertEqual(staging.target_platform(caller), "windows" if suffix else "linux")
                (runtime / ("publisher-font-worker" + suffix)).write_bytes(b"changed exact bytes")
                repeated = staging.stage(runtime, caller, ["font"], target=triple)
                self.assertEqual(adjacent.read_bytes(), b"changed exact bytes")
                self.assertEqual(repeated["workers"][0]["sha256"], staging.digest(adjacent))

    def test_ambiguous_target_and_missing_worker_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            caller = root / "caller"
            caller.write_bytes(b"unknown")
            with self.assertRaisesRegex(ValueError, "provide --target"):
                staging.stage(root, caller, ["font"])
            with self.assertRaises(FileNotFoundError):
                staging.stage(root, caller, ["font"], target="x86_64-unknown-linux-gnu")
            with self.assertRaisesRegex(ValueError, "Windows or Linux"):
                staging.stage(root, caller, ["font"], target="aarch64-apple-darwin")


if __name__ == "__main__":
    unittest.main()
