"""Protocol contracts and provenance for the standalone Rust references."""
import pathlib
import tempfile
import unittest
from unittest import mock

from scripts import benchmark_process, rust_baselines


class RustBaselineTests(unittest.TestCase):
    def test_protocol_contracts_cover_every_reference(self):
        self.assertEqual(set(rust_baselines.CONTRACTS), {
            "fib30", "tasks_10000", "tcp_fanout", "retrying_worker",
            "int32_loop", "int64_loop", "startup", "float64_add", "float64_sum",
        })
        self.assertEqual(rust_baselines.CONTRACTS["int32_loop"], (b"", b"", b"10000000\n"))
        self.assertEqual(rust_baselines.CONTRACTS["retrying_worker"][2],
                         b"DONE release-performance retrying-worker 112 18112\n")

    def test_build_is_locked_pinned_and_hashes_sources_and_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory)
            (target / "release").mkdir()
            for name in rust_baselines.CONTRACTS:
                (target / "release" / name).write_bytes(name.encode())
            with mock.patch.object(benchmark_process, "run_process_group") as owned_build, mock.patch.object(rust_baselines.subprocess, "run") as run:
                run.return_value.stdout = "rustc 1.95.0"
                binaries, record = rust_baselines.build(target)
            owned_build.assert_called_once()
            self.assertEqual(owned_build.call_args.kwargs["timeout"], 1800)
            command = owned_build.call_args.args[0]
            self.assertEqual(command[:3], ["cargo", "+1.95.0", "build"])
            self.assertIn("--locked", command)
            self.assertEqual(set(binaries), set(rust_baselines.CONTRACTS))
            self.assertIn("Cargo.lock", record["sources"])
            self.assertEqual(len(record["binaries"]["fib30"]["sha256"]), 64)


if __name__ == "__main__":
    unittest.main()
