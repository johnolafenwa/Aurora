#!/usr/bin/env python3
"""Focused tests for the consolidated Batch-6 release benchmark runner."""

from __future__ import annotations

import importlib.util
import contextlib
import json
import io
import sys
import time
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("bench-release-performance.py")
SPEC = importlib.util.spec_from_file_location("bench_release_performance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class ReleasePerformanceBenchmarkTests(unittest.TestCase):
    def test_manifest_pins_every_release_workload_protocol(self) -> None:
        self.assertEqual(
            list(bench.PROTOCOL_WORKLOADS),
            ["fib30", "tasks_10000", "tcp_fanout", "retrying_worker"],
        )
        self.assertEqual(
            bench.PROTOCOL_WORKLOADS["fib30"].ready,
            b"READY release-performance fib30 30\n",
        )
        self.assertEqual(
            bench.PROTOCOL_WORKLOADS["tasks_10000"].done,
            b"DONE release-performance tasks 10000 49995000\n",
        )
        self.assertEqual(
            bench.PROTOCOL_WORKLOADS["tcp_fanout"].done,
            b"DONE release-performance tcp-fanout 20 80\n",
        )
        self.assertEqual(
            bench.PROTOCOL_WORKLOADS["retrying_worker"].done,
            b"DONE release-performance retrying-worker 112 18112\n",
        )
        self.assertEqual(
            list(bench.V6_LANES),
            [
                "aura_startup",
                "aura_int32",
                "aura_int64",
                "cpython_startup",
                "cpython_int",
            ],
        )
        self.assertIn("20 ephemeral loopback listeners", bench.METHODOLOGY_NOTES["tcp_fanout"])

    def test_rust_protocol_lane_and_paired_summary(self):
        binaries = {name: Path('/tmp') / name for name in bench.PROTOCOL_WORKLOADS}
        binaries.update({'rust_' + name: Path('/tmp/rust') / name for name in bench.PROTOCOL_WORKLOADS})
        commands = bench.protocol_commands(binaries, Path('/python'))
        self.assertEqual(commands['fib30']['rust'], ['/tmp/rust/fib30'])
        pairs = [{'workloads': {'fib30': {'runs': {
            lane: {'protocol_elapsed_s': duration, 'whole_process_elapsed_s': duration}
            for lane, duration in [('aura', 6), ('cpython', 12), ('rust', 2)]
        }}}}]
        result = bench.summarize_protocol_workload(pairs, 'fib30')
        self.assertEqual(result['protocol_vs_rust']['paired_median_ratio'], 3)
        self.assertIn('rust', result['protocol_vs_rust'])

    def test_measurement_plan_rotates_workloads_and_lane_order(self) -> None:
        first = bench.measurement_plan(0)
        second = bench.measurement_plan(1)
        self.assertEqual(
            first[0][0],
            "fib30",
        )
        self.assertEqual(second[0][0], "tasks_10000")
        self.assertEqual(first[0][1], ["aura", "cpython", "rust"])
        self.assertEqual(second[0][1], ["rust", "cpython", "aura"])
        self.assertEqual(first[-1][0], "v6")
        self.assertNotEqual(
            dict(first)["v6"],
            dict(bench.measurement_plan(5))["v6"],
        )

    def test_protocol_lane_uses_exact_ready_go_done_and_owned_process(self) -> None:
        contract = bench.ProtocolWorkload(
            name="fake",
            aura_source=Path("unused.au"),
            cpython_source=Path("unused.py"),
            ready=b"READY release-performance fake 1\n",
            go=b"GO release-performance fake\n",
            done=b"DONE release-performance fake 7\n",
        )
        with tempfile.TemporaryDirectory() as directory:
            program = Path(directory) / "fake.py"
            program.write_text(
                "import sys\n"
                "print('READY release-performance fake 1', flush=True)\n"
                "if sys.stdin.readline() != 'GO release-performance fake\\n':\n"
                "    raise SystemExit(4)\n"
                "print('DONE release-performance fake 7', flush=True)\n",
                encoding="utf-8",
            )
            result = bench.run_protocol_lane(
                contract,
                "cpython",
                [sys.executable, str(program)],
            )
        self.assertEqual(result["ready"], "READY release-performance fake 1\n")
        self.assertEqual(result["done"], "DONE release-performance fake 7\n")
        self.assertGreater(result["protocol_elapsed_s"], 0.0)
        self.assertGreaterEqual(
            result["whole_process_elapsed_s"], result["protocol_elapsed_s"]
        )
        self.assertEqual(result["returncode"], 0)

    def test_protocol_lane_rejects_wrong_done_and_trailing_output(self) -> None:
        contract = bench.ProtocolWorkload(
            name="fake",
            aura_source=Path("unused.au"),
            cpython_source=Path("unused.py"),
            ready=b"READY release-performance fake\n",
            go=b"GO release-performance fake\n",
            done=b"DONE release-performance fake 7\n",
        )
        programs = {
            "wrong": "DONE release-performance fake 8",
            "trailing": "DONE release-performance fake 7\\nextra",
        }
        for label, final_output in programs.items():
            with self.subTest(label=label):
                with tempfile.TemporaryDirectory() as directory:
                    program = Path(directory) / "fake.py"
                    program.write_text(
                        "import sys\n"
                        "print('READY release-performance fake', flush=True)\n"
                        "sys.stdin.readline()\n"
                        f"print({final_output!r}, flush=True)\n",
                        encoding="utf-8",
                    )
                    with self.assertRaises(bench.BenchmarkError):
                        bench.run_protocol_lane(
                            contract,
                            "cpython",
                            [sys.executable, str(program)],
                        )

    def test_protocol_lane_rejects_wrong_ready_and_bounded_ready_timeout(self) -> None:
        contract = bench.ProtocolWorkload(
            name="fake",
            aura_source=Path("unused.au"),
            cpython_source=Path("unused.py"),
            ready=b"READY release-performance fake\n",
            go=b"GO release-performance fake\n",
            done=b"DONE release-performance fake 7\n",
        )
        with tempfile.TemporaryDirectory() as directory:
            wrong = Path(directory) / "wrong.py"
            wrong.write_text("print('READY wrong', flush=True)\n", encoding="utf-8")
            with self.assertRaisesRegex(bench.BenchmarkError, "READY"):
                bench.run_protocol_lane(
                    contract, "cpython", [sys.executable, str(wrong)]
                )

            stalled = Path(directory) / "stalled.py"
            stalled.write_text("import time\ntime.sleep(30)\n", encoding="utf-8")
            started = time.monotonic()
            with mock.patch.object(bench, "READY_TIMEOUT_SECONDS", 0.02):
                with self.assertRaisesRegex(bench.BenchmarkError, "timed out"):
                    bench.run_protocol_lane(
                        contract, "cpython", [sys.executable, str(stalled)]
                    )
            self.assertLess(time.monotonic() - started, 2.0)

    def test_whole_process_lane_requires_exact_stdout_and_empty_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            valid = Path(directory) / "valid.py"
            valid.write_text("print(10000000)\n", encoding="utf-8")
            result = bench.run_whole_process_lane(
                "cpython_int",
                [sys.executable, str(valid)],
                b"10000000\n",
            )
            self.assertGreater(result["whole_process_elapsed_s"], 0.0)
            invalid = Path(directory) / "invalid.py"
            invalid.write_text(
                "import sys\nprint(10000000)\nprint('noise', file=sys.stderr)\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(bench.BenchmarkError, "stderr"):
                bench.run_whole_process_lane(
                    "cpython_int",
                    [sys.executable, str(invalid)],
                    b"10000000\n",
                )

    def test_controlled_environment_clears_runtime_and_cache_overrides(self) -> None:
        inherited = {
            "AURA_WORKERS": "99",
            "AURA_BLOCKING_WORKERS": "88",
            "AURA_BLOCKING_QUEUE_CAPACITY": "77",
            "AURA_CACHE_DIR": "/tmp/cache",
            "AURA_NATIVE_CACHE_DIR": "/tmp/old-cache",
        }
        with mock.patch.dict(bench.os.environ, inherited, clear=False):
            environment = bench.controlled_environment()
        for name in inherited:
            self.assertNotIn(name, environment)
        self.assertEqual(environment["PYTHONHASHSEED"], "0")

    def test_duration_and_paired_summaries_retain_raw_observations(self) -> None:
        pairs = [
            {
                "workloads": {
                    "fib30": {
                        "runs": {
                            "aura": {
                                "protocol_elapsed_s": aura,
                                "whole_process_elapsed_s": aura + 0.5,
                            },
                            "cpython": {
                                "protocol_elapsed_s": cpython,
                                "whole_process_elapsed_s": cpython + 0.5,
                            },
                        }
                    }
                }
            }
            for aura, cpython in ((2.0, 1.0), (4.0, 2.0), (3.0, 1.5))
        ]
        for pair in pairs:
            pair["workloads"]["fib30"]["runs"]["rust"] = {"protocol_elapsed_s": 0.5, "whole_process_elapsed_s": 0.75}
        summary = bench.summarize_protocol_workload(pairs, "fib30")
        primary = summary["protocol"]
        self.assertEqual(primary["aura"]["samples_s"], [2.0, 4.0, 3.0])
        self.assertEqual(primary["cpython"]["samples_s"], [1.0, 2.0, 1.5])
        self.assertEqual(primary["paired_ratios"], [2.0, 2.0, 2.0])
        self.assertEqual(primary["paired_median_ratio"], 2.0)
        self.assertEqual(primary["ratio_of_medians"], 2.0)
        self.assertIn("whole_process", summary)

    def test_duration_statistics_are_exact_and_invalid_pairs_are_rejected(self) -> None:
        summary = bench.duration_summary([1.0, 2.0, 4.0, 8.0, 16.0])
        self.assertEqual(summary["median_s"], 4.0)
        self.assertEqual(summary["mad_s"], 3.0)
        self.assertEqual(summary["p95_s"], 16.0)
        self.assertEqual(summary["best_s"], 1.0)
        with self.assertRaisesRegex(bench.BenchmarkError, "aligned"):
            bench.paired_duration_summary([1.0], [1.0, 2.0])
        with self.assertRaisesRegex(bench.BenchmarkError, "finite"):
            bench.duration_summary([float("nan")])

    def test_v6_summary_reports_startup_and_adjusted_pairs(self) -> None:
        pairs = []
        for multiplier in (1.0, 2.0, 3.0):
            samples = {
                "aura_startup": 1.0 * multiplier,
                "aura_int32": 5.0 * multiplier,
                "aura_int64": 3.0 * multiplier,
                "cpython_startup": 2.0 * multiplier,
                "cpython_int": 10.0 * multiplier,
            }
            pairs.append(
                {
                    "workloads": {
                        "v6": {
                            "runs": {
                                lane: {"whole_process_elapsed_s": value}
                                for lane, value in samples.items()
                            }
                        }
                    }
                }
            )
        summary = bench.summarize_v6(pairs)
        self.assertEqual(
            summary["startup_adjusted"]["aura_int32"]["samples_s"],
            [4.0, 8.0, 12.0],
        )
        self.assertEqual(
            summary["startup_adjusted"]["cpython_int"]["samples_s"],
            [8.0, 16.0, 24.0],
        )
        self.assertEqual(
            summary["comparisons"]["aura_int32_vs_cpython"][
                "paired_median_ratio"
            ],
            0.5,
        )
        pairs[0]["workloads"]["v6"]["runs"]["aura_int32"][
            "whole_process_elapsed_s"
        ] = 0.5
        revised = bench.summarize_v6(pairs)
        self.assertEqual(
            revised["startup_adjusted"]["aura_int32"][
                "invalid_nonpositive_pair_repetitions"
            ],
            [0],
        )
        self.assertEqual(
            revised["comparisons"]["aura_int32_vs_cpython_startup_adjusted"][
                "excluded_pair_repetitions"
            ],
            [0],
        )

    def test_process_classification_catches_repo_work_and_sustained_cpu(self) -> None:
        first = {
            100: bench.ProcessSample(100, 50, 99.0, "python3", "runner"),
            101: bench.ProcessSample(101, 100, 99.0, "worker", "child"),
            102: bench.ProcessSample(102, 50, 99.0, "yes", "yes"),
            103: bench.ProcessSample(103, 50, 2.0, "cargo", "cargo build"),
            50: bench.ProcessSample(50, 1, 99.0, "zsh", "parent"),
        }
        second = dict(first)
        inventory = bench.classify_competing_processes(
            first,
            second,
            cwd_by_pid={102: Path("/tmp"), 103: bench.ROOT},
            runner_pid=100,
            parent_pid=50,
        )
        self.assertEqual([record["pid"] for record in inventory], [102, 103])
        self.assertEqual(
            inventory[0]["reasons"],
            ["sustained high CPU (>= 50.0%)"],
        )
        self.assertEqual(
            inventory[1]["reasons"],
            ["Aura repository cargo/rustc/aura process"],
        )
        unknown = bench.classify_competing_processes(
            {104: bench.ProcessSample(104, 50, 1.0, "cargo", "cargo build")},
            {104: bench.ProcessSample(104, 50, 1.0, "cargo", "cargo build")},
            cwd_by_pid={104: None},
            runner_pid=100,
            parent_pid=50,
        )
        self.assertEqual(
            unknown[0]["reasons"],
            ["cargo/rustc/aura process with unknown cwd"],
        )

    def test_process_snapshot_accepts_empty_arguments_and_rejects_malformed_rows(
        self,
    ) -> None:
        valid = mock.Mock(returncode=0, stdout="99 1 0.0 launchd\n", stderr="")
        with mock.patch.object(
            bench.benchmark_process, "run_process_group", return_value=valid
        ):
            snapshot = bench.process_snapshot()
        self.assertEqual(snapshot[99].arguments, "")

        malformed = mock.Mock(returncode=0, stdout="not-a-row\n", stderr="")
        with mock.patch.object(
            bench.benchmark_process, "run_process_group", return_value=malformed
        ):
            with self.assertRaisesRegex(bench.BenchmarkError, "malformed"):
                bench.process_snapshot()

    def test_quiet_inventory_wires_two_snapshots_into_classifier(self) -> None:
        first = {
            77: bench.ProcessSample(77, 1, 99.0, "yes", "yes"),
        }
        second = {
            77: bench.ProcessSample(77, 1, 98.0, "yes", "yes"),
        }
        with mock.patch.object(
            bench, "process_snapshot", side_effect=[first, second]
        ):
            with mock.patch.object(bench, "process_cwd", return_value=Path("/tmp")):
                with mock.patch.object(bench.time, "sleep") as sleep:
                    with mock.patch.object(bench.os, "getpid", return_value=100):
                        with mock.patch.object(bench.os, "getppid", return_value=50):
                            inventory = bench.quiet_process_inventory()
        sleep.assert_called_once_with(bench.QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS)
        self.assertEqual([record["pid"] for record in inventory], [77])
        self.assertEqual(
            inventory[0]["reasons"],
            ["sustained high CPU (>= 50.0%)"],
        )

    def test_repository_contract_requires_clean_detached_head(self) -> None:
        with self.assertRaisesRegex(bench.BenchmarkError, "dirty"):
            bench.require_measurement_repository(
                {"dirty_files": [" M source"], "detached": True}
            )
        with self.assertRaisesRegex(bench.BenchmarkError, "detached"):
            bench.require_measurement_repository(
                {"dirty_files": [], "detached": False}
            )
        bench.require_measurement_repository(
            {"dirty_files": [], "detached": True, "commit": "abc123"}
        )

    def test_release_options_require_exactly_eleven_pairs(self) -> None:
        base = bench.Options(
            label="release",
            aura=Path("/tmp/aura"),
            python=Path("/tmp/python3"),
            pairs=5,
            raw_json=Path("/tmp/raw.json"),
            summary_json=Path("/tmp/summary.json"),
            allow_competing_processes=False,
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "exactly 11"):
            bench.validate_options(base)

    def test_host_contract_requires_named_hardware_and_boot_identity(self) -> None:
        bench.require_measurement_host(
            {"hardware_model": "Mac14,9", "boot_time": "Thu Jul 30"}
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "Mac14,9"):
            bench.require_measurement_host(
                {"hardware_model": "other", "boot_time": "boot"}
            )
        with self.assertRaisesRegex(bench.BenchmarkError, "boot"):
            bench.require_measurement_host(
                {"hardware_model": "Mac14,9", "boot_time": ""}
            )

    def test_quiet_check_rejects_after_timing_competitor_without_override(self) -> None:
        competitor = [{"pid": 77, "reasons": ["sustained high CPU"]}]
        with self.assertRaisesRegex(bench.BenchmarkError, "after timing"):
            bench.require_quiet_process_check(
                competitor,
                phase="after timing",
                allow_competing_processes=False,
            )
        bench.require_quiet_process_check(
            competitor,
            phase="after timing",
            allow_competing_processes=True,
        )

    def test_input_provenance_hashes_every_helper_and_source(self) -> None:
        options = bench.Options(
            label="qualification",
            aura=Path("/tmp/aura"),
            python=Path("/tmp/python3"),
            pairs=11,
            raw_json=Path("/tmp/raw.json"),
            summary_json=Path("/tmp/summary.json"),
            allow_competing_processes=False,
        )
        with mock.patch.object(bench, "command_output", return_value="version"):
            with mock.patch.object(
                bench,
                "python_identity",
                return_value={"implementation": "CPython", "version": "3.9.6"},
            ):
                with mock.patch.object(
                    bench,
                    "sha256_file",
                    side_effect=lambda path: "sha256:" + Path(path).name,
                ):
                    inputs = bench.qualify_inputs(options)
        self.assertEqual(
            inputs["benchmark_process"]["sha256"],
            "sha256:benchmark_process.py",
        )
        self.assertEqual(inputs["runner"]["sha256"], "sha256:bench-release-performance.py")
        self.assertIn("fib30.au", inputs["sources"])
        self.assertIn("python_int_loop.py", inputs["sources"])

    def test_python_identity_requires_cpython(self) -> None:
        pypy = mock.Mock(
            returncode=0,
            stdout='{"implementation":"PyPy","version":"3.10"}\n',
            stderr="",
        )
        with mock.patch.object(
            bench.benchmark_process, "run_process_group", return_value=pypy
        ):
            with self.assertRaisesRegex(bench.BenchmarkError, "CPython"):
                bench.python_identity(Path("/tmp/python"))

    def test_aura_qualification_is_a_fresh_locked_release_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            aura = Path(directory) / "aura"
            aura.write_bytes(b"release-aura")
            aura.chmod(0o755)
            completed = mock.Mock(
                returncode=0,
                stdout=b"",
                stderr=b"Finished release build\n",
            )
            with mock.patch.object(
                bench.benchmark_process,
                "run_process_group",
                return_value=completed,
            ) as run:
                record = bench.qualify_aura_binary(aura)
        command = run.call_args.args[0]
        self.assertEqual(
            command[:7],
            ["cargo", "build", "--release", "--locked", "-p", "aura", "--target-dir"],
        )
        self.assertTrue(record["fresh_locked_release_build"])

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
            written_summary["raw_report"]["path"], str(raw_path.resolve())
        )

    def test_execute_builds_before_warmup_and_runs_eleven_plans(self) -> None:
        options = bench.Options(
            label="post-reboot",
            aura=Path("/tmp/aura"),
            python=Path("/tmp/python3"),
            pairs=11,
            raw_json=Path("/tmp/raw.json"),
            summary_json=Path("/tmp/summary.json"),
            allow_competing_processes=False,
        )
        events: list[str] = []

        def fake_build(*_args: object) -> tuple[dict[str, Path], list[dict[str, object]]]:
            events.append("build")
            return ({name: Path("/tmp") / name for name in bench.AURA_BUILD_SOURCES}, [])

        def fake_protocol(
            contract: object, lane: str, _command: list[str]
        ) -> dict[str, object]:
            events.append("run")
            return {
                "protocol_elapsed_s": 1.0,
                "whole_process_elapsed_s": 1.1,
                "returncode": 0,
            }

        def fake_whole(
            lane: str, _command: list[str], _stdout: bytes
        ) -> dict[str, object]:
            events.append("run")
            duration = 0.5 if lane.endswith("startup") else 2.0
            return {"whole_process_elapsed_s": duration, "returncode": 0}

        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(bench.rust_baselines, "build", return_value=(
                {name: Path("/tmp/rust") / name for name in bench.rust_baselines.CONTRACTS}, {"sources": {}})))
            stack.enter_context(mock.patch.object(bench, "validate_options"))
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "qualify_inputs",
                    return_value={"sources": {"fake": {"sha256": "same"}}},
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "qualify_aura_binary",
                    return_value={"fresh_locked_release_build": True},
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "repository_record",
                    return_value={
                        "commit": "0123456789abcdef",
                        "branch": None,
                        "detached": True,
                        "dirty_files": [],
                    },
                )
            )
            stack.enter_context(
                mock.patch.object(bench, "require_measurement_repository")
            )
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "hardware_record",
                    return_value={
                        "hardware_model": "Mac14,9",
                        "boot_time": "boot",
                    },
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "quiet_process_inventory",
                    side_effect=[[], [], []],
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench,
                    "build_aura_workloads",
                    side_effect=fake_build,
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench, "run_protocol_lane", side_effect=fake_protocol
                )
            )
            stack.enter_context(
                mock.patch.object(
                    bench, "run_whole_process_lane", side_effect=fake_whole
                )
            )
            report = bench.execute(options)
        self.assertEqual(events[0], "build")
        self.assertEqual(len(report["pairs"]), 11)
        self.assertEqual(
            set(report["warmups"]),
            {*bench.PROTOCOL_WORKLOADS, "v6"},
        )
        self.assertTrue(report["contractual"])
        self.assertIsNone(report["performance_gate"])
        self.assertEqual(
            report["quiet_process_checks"],
            {"before_build": [], "before_timing": [], "after_timing": []},
        )

    def test_main_reports_cleanup_failure_and_interrupt_without_traceback(self) -> None:
        cleanup_error = bench.benchmark_process.ProcessGroupCleanupError("not reaped")
        with mock.patch.object(bench, "parse_options", return_value=mock.Mock()):
            with mock.patch.object(bench, "execute", side_effect=cleanup_error):
                stderr = io.StringIO()
                with mock.patch("sys.stderr", stderr):
                    self.assertEqual(bench.main([]), 2)
                self.assertIn("not reaped", stderr.getvalue())
        with mock.patch.object(bench, "parse_options", return_value=mock.Mock()):
            with mock.patch.object(bench, "execute", side_effect=KeyboardInterrupt):
                stderr = io.StringIO()
                with mock.patch("sys.stderr", stderr):
                    self.assertEqual(bench.main([]), 130)
                self.assertIn("interrupted", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
