#!/usr/bin/env python3
"""Focused tests for the Phase-7.3 numeric-array benchmark runner."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("bench-numeric-arrays.py")
SPEC = importlib.util.spec_from_file_location("bench_numeric_arrays", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class NumericArrayBenchmarkTests(unittest.TestCase):
    def test_protocol_is_exact_and_checksum_is_finite(self) -> None:
        self.assertEqual(
            bench.parse_ready_line(
                b"READY numeric-arrays add 1000000 512\n",
                workload="add",
                iterations=512,
            ),
            {"workload": "add", "elements": 1_000_000, "iterations": 512},
        )
        self.assertEqual(
            bench.parse_done_line(
                b"DONE numeric-arrays add 512 2048.0\n",
                workload="add",
                iterations=512,
                expected_checksum=2048.0,
            ),
            {"workload": "add", "iterations": 512, "checksum": 2048.0},
        )
        malformed = (
            b"READY numeric-arrays add 1000000 512 extra\n",
            b"READY numeric-arrays sum 1000000 512\n",
            b"READY numeric-arrays add 999999 512\n",
        )
        for line in malformed:
            with self.subTest(line=line):
                with self.assertRaises(bench.BenchmarkError):
                    bench.parse_ready_line(
                        line,
                        workload="add",
                        iterations=512,
                    )
        with self.assertRaisesRegex(bench.BenchmarkError, "checksum"):
            bench.parse_done_line(
                b"DONE numeric-arrays add 512 2047.0\n",
                workload="add",
                iterations=512,
                expected_checksum=2048.0,
            )
        with self.assertRaisesRegex(bench.BenchmarkError, "finite"):
            bench.parse_done_line(
                b"DONE numeric-arrays add 512 nan\n",
                workload="add",
                iterations=512,
                expected_checksum=2048.0,
            )

    def test_rust_array_lane_commands(self):
        commands = bench.lane_commands({name: Path('/tmp') / name for name in
            ('add', 'sum', 'rust_add', 'rust_sum')}, Path('/python'))
        self.assertEqual(commands['rust_add'], ['/tmp/rust_add'])
        self.assertEqual(bench.LANES['rust_sum']['expected_checksum'], 4096000000.0)

    def test_single_thread_environment_is_explicit(self) -> None:
        self.assertEqual(
            bench.SINGLE_THREAD_ENVIRONMENT,
            {
                "AURA_WORKERS": "1",
                "OMP_NUM_THREADS": "1",
                "VECLIB_MAXIMUM_THREADS": "1",
                "OPENBLAS_NUM_THREADS": "1",
                "MKL_NUM_THREADS": "1",
                "NUMEXPR_NUM_THREADS": "1",
            },
        )

    def test_run_order_reverses_every_pair(self) -> None:
        forward = ["aura_add", "numpy_add", "aura_sum", "numpy_sum", "rust_add", "rust_sum"]
        self.assertEqual(bench.pair_order(0), forward)
        self.assertEqual(bench.pair_order(1), list(reversed(forward)))
        self.assertEqual(bench.pair_order(10), forward)

    def test_process_classification_catches_repo_work_and_sustained_cpu_burners(
        self,
    ) -> None:
        first = {
            100: bench.ProcessSample(100, 50, 99.0, "python3", "benchmark runner"),
            101: bench.ProcessSample(101, 100, 99.0, "worker", "runner child"),
            102: bench.ProcessSample(102, 50, 99.0, "yes", "yes"),
            103: bench.ProcessSample(103, 50, 99.0, "ps", "ps -axo"),
            105: bench.ProcessSample(105, 50, 4.0, "rustc", "rustc elsewhere"),
            106: bench.ProcessSample(106, 50, 99.0, "python3", "brief work"),
            50: bench.ProcessSample(50, 1, 99.0, "zsh", "controlling parent"),
        }
        second = {
            **first,
            104: bench.ProcessSample(104, 50, 8.0, "cargo", "cargo build -p aura"),
            106: bench.ProcessSample(106, 50, 1.0, "python3", "brief work"),
        }
        inventory = bench.classify_competing_processes(
            first,
            second,
            cwd_by_pid={
                102: Path("/tmp"),
                104: bench.ROOT,
                105: Path("/tmp/other-checkout"),
            },
            runner_pid=100,
            parent_pid=50,
        )
        self.assertEqual([record["pid"] for record in inventory], [102, 104])
        self.assertEqual(
            inventory[0]["reasons"],
            ["sustained high CPU (>= 50.0%)"],
        )
        self.assertEqual(inventory[0]["cpu_percent_samples"], [99.0, 99.0])
        self.assertEqual(
            inventory[1]["reasons"],
            ["Aura repository cargo/rustc/aura process"],
        )
        self.assertEqual(inventory[1]["cwd"], str(bench.ROOT))

    def test_quiet_process_inventory_samples_twice_and_records_competitors(
        self,
    ) -> None:
        first_rows = "\n".join(
            [
                "99101 10 8.0 cargo cargo build -p aura",
                "99102 10 99.0 rustc rustc --crate-name elsewhere",
                "99103 10 98.0 yes yes",
            ]
        )
        second_rows = "\n".join(
            [
                "99101 10 7.0 cargo cargo build -p aura",
                "99102 10 98.0 rustc rustc --crate-name elsewhere",
                "99103 10 97.0 yes yes",
            ]
        )
        completed = [
            mock.Mock(stdout=first_rows, stderr="", returncode=0),
            mock.Mock(stdout=second_rows, stderr="", returncode=0),
        ]
        cwd_by_pid = {
            99101: bench.ROOT,
            99102: Path("/tmp/other-checkout"),
            99103: Path("/tmp"),
        }
        with mock.patch.object(bench.subprocess, "run", side_effect=completed):
            with mock.patch.object(bench.time, "sleep") as sleep:
                with mock.patch.object(
                    bench,
                    "process_cwd",
                    side_effect=lambda pid: cwd_by_pid[pid],
                ):
                    inventory = bench.quiet_process_inventory()
        sleep.assert_called_once_with(bench.QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS)
        self.assertEqual(
            [record["pid"] for record in inventory],
            [99101, 99102, 99103],
        )
        self.assertEqual(inventory[0]["cwd"], str(bench.ROOT))

    def test_input_provenance_hashes_the_process_ownership_helper(self) -> None:
        options = bench.Options(
            label="qualification",
            aura=Path("/tmp/aura"),
            python=Path("/tmp/python3"),
            pairs=11,
            raw_json=Path("/tmp/raw.json"),
            summary_json=Path("/tmp/summary.json"),
            allow_competing_processes=False,
        )
        identity = mock.Mock(
            returncode=0,
            stdout='{"float64_itemsize": 8, "numpy_version": "test"}',
            stderr="",
        )
        with mock.patch.object(bench.subprocess, "run", return_value=identity):
            with mock.patch.object(bench, "command_output", return_value="version"):
                with mock.patch.object(
                    bench,
                    "sha256_file",
                    side_effect=lambda path: "sha256:" + Path(path).name,
                ):
                    inputs = bench.qualify_inputs(options)
        helper = bench.ROOT / "scripts/benchmark_process.py"
        self.assertEqual(
            inputs["benchmark_process"],
            {
                "path": str(helper.resolve()),
                "sha256": "sha256:benchmark_process.py",
            },
        )

    def test_summary_retains_raw_samples_and_paired_ratios(self) -> None:
        pairs = [
            {
                "runs": {
                    "aura_add": {"elapsed_s": 2.0},
                    "numpy_add": {"elapsed_s": 1.0},
                    "aura_sum": {"elapsed_s": 3.0},
                    "numpy_sum": {"elapsed_s": 1.5},
                }
            },
            {
                "runs": {
                    "aura_add": {"elapsed_s": 4.0},
                    "numpy_add": {"elapsed_s": 2.0},
                    "aura_sum": {"elapsed_s": 6.0},
                    "numpy_sum": {"elapsed_s": 3.0},
                }
            },
            {
                "runs": {
                    "aura_add": {"elapsed_s": 3.0},
                    "numpy_add": {"elapsed_s": 1.5},
                    "aura_sum": {"elapsed_s": 4.5},
                    "numpy_sum": {"elapsed_s": 2.25},
                }
            },
        ]
        for pair in pairs:
            pair["runs"]["rust_add"] = {"elapsed_s": 0.5}
            pair["runs"]["rust_sum"] = {"elapsed_s": 0.75}
        summary = bench.summarize_pairs(pairs)
        self.assertEqual(summary["add"]["aura_vs_rust"]["paired_median_ratio"], 6.0)
        self.assertEqual(summary["add"]["aura"]["samples_s"], [2.0, 4.0, 3.0])
        self.assertEqual(summary["add"]["numpy"]["samples_s"], [1.0, 2.0, 1.5])
        self.assertEqual(summary["add"]["paired_ratios"], [2.0, 2.0, 2.0])
        self.assertEqual(summary["add"]["paired_median_ratio"], 2.0)
        self.assertEqual(summary["add"]["ratio_of_medians"], 2.0)
        self.assertEqual(summary["sum"]["paired_median_ratio"], 2.0)

    def test_contractual_evidence_requires_quiet_named_host_and_clean_detached_head(
        self,
    ) -> None:
        reasons = bench.evidence_noncontractual_reasons(
            allow_competing_processes=True,
            process_checks=([], [{"pid": 7}], []),
            host={"hardware_model": "other"},
            repository={"dirty_files": [" M source"], "detached": False},
        )
        self.assertEqual(
            reasons,
            [
                "the competing-process override was enabled",
                "competing host CPU consumers were observed",
                "host hardware model is not the contractual Mac14,9 baseline",
                "repository worktree was dirty",
                "repository HEAD was not detached",
            ],
        )

    def test_atomic_artifacts_link_summary_to_raw_sha256(self) -> None:
        raw = {"schema_version": 1, "pairs": []}
        summary = {"schema_version": 1, "benchmarks": {}}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw_path = root / "raw.json"
            summary_path = root / "summary.json"
            bench.write_artifacts(raw_path, summary_path, raw, summary)
            raw_bytes = raw_path.read_bytes()
            written_summary = json.loads(summary_path.read_text(encoding="utf-8"))
        self.assertTrue(raw_bytes.endswith(b"\n"))
        self.assertEqual(
            written_summary["raw_report"]["sha256"],
            bench.sha256_bytes(raw_bytes),
        )
        self.assertEqual(
            written_summary["raw_report"]["path"],
            str(raw_path.resolve()),
        )

    @mock.patch.object(bench.rust_baselines, "source_identity", return_value={})
    @mock.patch.object(bench.rust_baselines, "build", return_value=(
        {name: Path("/tmp/rust") / name for name in bench.rust_baselines.CONTRACTS}, {"sources": {}}))
    def test_execution_warms_then_runs_eleven_alternating_pairs(self, *_mocks) -> None:
        options = bench.Options(
            label="post-reboot",
            aura=Path("/tmp/aura"),
            python=Path("/tmp/python3"),
            pairs=11,
            raw_json=Path("/tmp/raw.json"),
            summary_json=Path("/tmp/summary.json"),
            allow_competing_processes=False,
        )
        seen: list[str] = []

        def fake_run(lane: str, _commands: dict[str, list[str]]) -> dict[str, object]:
            seen.append(lane)
            return {
                "elapsed_s": 1.0,
                "checksum": bench.LANES[lane]["expected_checksum"],
            }

        with mock.patch.object(bench, "validate_options"):
            with mock.patch.object(bench, "qualify_inputs", return_value={}):
                with mock.patch.object(
                    bench,
                    "repository_record",
                    return_value={
                        "commit": "0123456789abcdef",
                        "branch": None,
                        "detached": True,
                        "dirty_files": [],
                    },
                ):
                    with mock.patch.object(
                        bench,
                        "hardware_record",
                        return_value={"hardware_model": "Mac14,9"},
                    ):
                        with mock.patch.object(
                            bench,
                            "quiet_process_inventory",
                            side_effect=[[], [], []],
                        ):
                            with mock.patch.object(
                                bench,
                                "build_aura_workloads",
                                return_value=(
                                    {
                                        "add": Path("/tmp/aura-add"),
                                        "sum": Path("/tmp/aura-sum"),
                                    },
                                    [],
                                ),
                            ):
                                with mock.patch.object(
                                    bench, "run_lane", side_effect=fake_run
                                ):
                                    report = bench.execute(options)
        self.assertEqual(seen[:len(bench.LANES)], list(bench.LANES))
        expected_measured = [
            lane for repeat in range(11) for lane in bench.pair_order(repeat)
        ]
        self.assertEqual(seen[len(bench.LANES):], expected_measured)
        self.assertEqual(len(report["pairs"]), 11)
        self.assertTrue(report["contractual"])
        self.assertEqual(
            report["quiet_process_checks"],
            {"before_build": [], "before_timing": [], "after_timing": []},
        )
        self.assertIsNone(report["performance_gate"])


if __name__ == "__main__":
    unittest.main()
