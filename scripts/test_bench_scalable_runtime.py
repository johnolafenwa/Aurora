#!/usr/bin/env python3
"""Focused tests for the scalable-runtime benchmark host runner."""

from __future__ import annotations

import importlib.util
import io
import os
import signal
import stat
import struct
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("bench-scalable-runtime.py")
SPEC = importlib.util.spec_from_file_location("bench_scalable_runtime", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)

DIRECT_SCRIPT = Path(__file__).with_name("bench-direct-integer-loops.py")
DIRECT_SPEC = importlib.util.spec_from_file_location(
    "bench_direct_integer_loops", DIRECT_SCRIPT
)
assert DIRECT_SPEC is not None and DIRECT_SPEC.loader is not None
direct_bench = importlib.util.module_from_spec(DIRECT_SPEC)
DIRECT_SPEC.loader.exec_module(direct_bench)


class ProtocolTests(unittest.TestCase):
    def test_multicore_protocol_is_exact_and_checksum_is_mathematical(self) -> None:
        self.assertEqual(bench.REPORT_SCHEMA_VERSION, 4)
        expected = bench.park_miller_checksum(
            tasks=4,
            iterations=7,
            multiplier=48_271,
            modulus=2_147_483_647,
        )
        factor = pow(48_271, 7, 2_147_483_647)
        self.assertEqual(
            expected,
            sum((seed * factor) % 2_147_483_647 for seed in range(1, 5)),
        )
        self.assertEqual(
            bench.parse_multicore_ready_line(
                b"READY multicore 4 7 48271 2147483647\n",
                expected_tasks=4,
                expected_iterations=7,
                expected_multiplier=48_271,
                expected_modulus=2_147_483_647,
            ),
            {
                "tasks": 4,
                "iterations": 7,
                "multiplier": 48_271,
                "modulus": 2_147_483_647,
            },
        )
        self.assertEqual(
            bench.parse_multicore_done_line(
                ("DONE multicore 4 " + str(expected) + "\n").encode("ascii"),
                expected_tasks=4,
                expected_checksum=expected,
            ),
            {"tasks": 4, "checksum": expected},
        )

        malformed = [
            b"READY multicore 1 7 48271 2147483647 extra\n",
            b"READY multicore 4 8 48271 2147483647\n",
            b"READY multicore 4 7 0 2147483647\n",
        ]
        for line in malformed:
            with self.subTest(line=line):
                with self.assertRaises(bench.BenchmarkError):
                    bench.parse_multicore_ready_line(
                        line,
                        expected_tasks=4,
                        expected_iterations=7,
                        expected_multiplier=48_271,
                        expected_modulus=2_147_483_647,
                    )
        with self.assertRaisesRegex(bench.BenchmarkError, "checksum"):
            bench.parse_multicore_done_line(
                b"DONE multicore 4 123\n",
                expected_tasks=4,
                expected_checksum=expected,
            )

    def test_phase_lines_are_exact_and_phase_specific(self) -> None:
        self.assertEqual(
            bench.parse_phase_line(
                b"BASELINE sleepers 10000\n",
                phase="BASELINE",
                benchmark="sleepers",
                expected_fields=("10000",),
            ),
            ("10000",),
        )
        self.assertEqual(
            bench.parse_phase_line(
                b"READY sleepers 10000\n",
                phase="READY",
                benchmark="sleepers",
                expected_fields=("10000",),
            ),
            ("10000",),
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "unexpected BASELINE"):
            bench.parse_phase_line(
                b"READY sleepers 10000\n",
                phase="BASELINE",
                benchmark="sleepers",
                expected_fields=("10000",),
            )

    def test_ready_line_is_exact_and_bounded(self) -> None:
        self.assertEqual(
            bench.parse_ready_line(
                b"READY sleepers 10000\n",
                benchmark="sleepers",
                expected_fields=("10000",),
            ),
            ("10000",),
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "unexpected READY"):
            bench.parse_ready_line(
                b"note\nREADY sleepers 10000\n",
                benchmark="sleepers",
                expected_fields=("10000",),
            )
        with self.assertRaisesRegex(bench.BenchmarkError, "exceeded"):
            bench.read_bounded_line(io.BytesIO(b"x" * 33 + b"\n"), 32)

    def test_timer_protocol_requires_all_unique_samples_and_done(self) -> None:
        ready = bench.parse_timer_ready_line(
            b"READY timers 3 10 100.0 101.2\n",
            expected_count=3,
            expected_duration_ms=10,
        )
        self.assertEqual(ready["count"], 3)
        self.assertEqual(ready["duration_ms"], 10)
        self.assertEqual(ready["min_start_ms"], 100.0)
        self.assertEqual(ready["max_start_ms"], 101.2)
        self.assertAlmostEqual(ready["arm_span_ms"], 1.2)
        output = io.BytesIO(
            b"SAMPLE timer 2 0.30\n"
            b"SAMPLE timer 0 0.10\n"
            b"SAMPLE timer 1 0.20\n"
            b"DONE timers 3\n"
        )
        samples = bench.parse_timer_samples(output, expected_count=3)
        self.assertEqual([sample["index"] for sample in samples], [0, 1, 2])
        self.assertEqual(
            [sample["overshoot_ms"] for sample in samples],
            [0.10, 0.20, 0.30],
        )

        duplicate = io.BytesIO(
            b"SAMPLE timer 0 0.1\n"
            b"SAMPLE timer 0 0.2\n"
            b"DONE timers 2\n"
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "duplicate"):
            bench.parse_timer_samples(duplicate, expected_count=2)
        negative = io.BytesIO(
            b"SAMPLE timer 0 -0.1\n"
            b"DONE timers 1\n"
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "nonnegative"):
            bench.parse_timer_samples(negative, expected_count=1)

    def test_timer_ready_protocol_is_specialized_and_strict(self) -> None:
        cases = [
            (b"READY timers 2 10 1 2\n", "count"),
            (b"READY timers 3 11 1 2\n", "duration"),
            (b"READY timers 3 10 nan 2\n", "min_start_ms"),
            (b"READY timers 3 10 -1 2\n", "nonnegative"),
            (b"READY timers 3 10 2 1\n", "before"),
            (b"READY timers 3 10 1 2 extra\n", "READY"),
        ]
        for line, expected in cases:
            with self.subTest(line=line):
                with self.assertRaisesRegex(bench.BenchmarkError, expected):
                    bench.parse_timer_ready_line(
                        line,
                        expected_count=3,
                        expected_duration_ms=10,
                    )

    def test_timer_protocol_rejects_extra_output(self) -> None:
        output = io.BytesIO(
            b"SAMPLE timer 0 0.1\n"
            b"DONE timers 1\n"
            b"unexpected\n"
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "trailing"):
            bench.parse_timer_samples(output, expected_count=1)

    def test_massive_protocol_requires_all_timer_samples_and_exact_done(self) -> None:
        ready = bench.parse_massive_ready_line(
            b"READY massive 100000 1000 10 500 507\n",
            expected_sleepers=100000,
            expected_timer_count=1000,
            expected_duration_ms=10,
        )
        self.assertEqual(ready["sleepers"], 100000)
        self.assertEqual(ready["timer_count"], 1000)
        self.assertEqual(ready["arm_span_ms"], 7.0)

        output = io.BytesIO(
            b"SAMPLE massive_timer 1 0.20\n"
            b"SAMPLE massive_timer 0 0.10\n"
            b"DONE massive 100000 2\n"
        )
        samples = bench.parse_massive_samples(
            output, expected_sleepers=100000, expected_timer_count=2
        )
        self.assertEqual(
            [sample["overshoot_ms"] for sample in samples], [0.10, 0.20]
        )

        duplicate = io.BytesIO(
            b"SAMPLE massive_timer 0 0.1\n"
            b"SAMPLE massive_timer 0 0.2\n"
            b"DONE massive 100000 2\n"
        )
        with self.assertRaisesRegex(bench.BenchmarkError, "duplicate"):
            bench.parse_massive_samples(
                duplicate, expected_sleepers=100000, expected_timer_count=2
            )

    def test_starvation_protocol_is_exact_and_bounded(self) -> None:
        self.assertEqual(
            bench.parse_starvation_output(
                b"SAMPLE starvation 10 17\nDONE starvation\n",
                expected_sleep_ms=10,
            ),
            {"sleep_ms": 10, "elapsed_ms": 17},
        )
        cases = [
            (b"SAMPLE starvation 11 17\nDONE starvation\n", "sleep duration"),
            (b"SAMPLE starvation 10 -1\nDONE starvation\n", "nonnegative"),
            (b"SAMPLE starvation 10 17\nWRONG\n", "DONE"),
            (
                b"SAMPLE starvation 10 17\nDONE starvation\nunexpected\n",
                "trailing",
            ),
        ]
        for output, expected in cases:
            with self.subTest(output=output):
                with self.assertRaisesRegex(bench.BenchmarkError, expected):
                    bench.parse_starvation_output(output, expected_sleep_ms=10)


class StatisticsTests(unittest.TestCase):
    def multicore_pairs(
        self,
        one_task: list[float],
        four_task: list[float],
        *,
        four_cpu_percent: float = 200.0,
    ) -> list[dict[str, object]]:
        return [
            {
                "repeat": index,
                "order": [1, 4] if index % 2 == 0 else [4, 1],
                "runs": {
                    "1": {
                        "elapsed_s": one,
                        "process_cpu_percent": 100.0,
                    },
                    "4": {
                        "elapsed_s": four,
                        "process_cpu_percent": four_cpu_percent,
                    },
                },
            }
            for index, (one, four) in enumerate(zip(one_task, four_task))
        ]

    def test_multicore_gate_uses_paired_median_and_inclusive_boundaries(self) -> None:
        pairs = self.multicore_pairs(
            [0.50, 0.50, 0.50, 0.50, 0.50],
            [0.80, 0.80, 0.80, 0.80, 0.80],
            four_cpu_percent=150.0,
        )
        gate = bench.multicore_gate_summary(
            pairs,
            host={"physical_cores": 4, "logical_cpus": 4},
        )
        self.assertEqual(gate["paired_median_ratio"], 1.6)
        self.assertEqual(gate["ratio_of_medians"], 1.6)
        self.assertEqual(gate["pass_pair_indexes"], [0, 1, 2, 3, 4])
        self.assertEqual(gate["invalid_reasons"], [])
        self.assertTrue(gate["passed"])

    def test_multicore_gate_invalidates_short_noisy_underprovisioned_evidence(
        self,
    ) -> None:
        short = bench.multicore_gate_summary(
            self.multicore_pairs([0.24] * 5, [0.30] * 5),
            host={"physical_cores": 4, "logical_cpus": 4},
        )
        self.assertIn("one-task median", short["invalid_reasons"][0])
        self.assertFalse(short["passed"])

        noisy = bench.multicore_gate_summary(
            self.multicore_pairs(
                [0.40, 0.40, 0.50, 0.60, 0.60],
                [0.48, 0.48, 0.60, 0.72, 0.72],
            ),
            host={"physical_cores": 4, "logical_cpus": 4},
        )
        self.assertTrue(
            any("MAD/median" in reason for reason in noisy["invalid_reasons"])
        )
        self.assertFalse(noisy["passed"])

        cores = bench.multicore_gate_summary(
            self.multicore_pairs([0.50] * 5, [0.60] * 5),
            host={"physical_cores": 3, "logical_cpus": 8},
        )
        self.assertTrue(
            any("physical cores" in reason for reason in cores["invalid_reasons"])
        )
        self.assertFalse(cores["passed"])

        affinity = bench.multicore_gate_summary(
            self.multicore_pairs([0.50] * 5, [0.60] * 5),
            host={
                "affinity_cpus": 2,
                "physical_cores": 8,
                "logical_cpus": 8,
            },
        )
        self.assertEqual(
            affinity["core_qualification"]["source"],
            "affinity_cpus",
        )
        self.assertTrue(
            any("affinity" in reason for reason in affinity["invalid_reasons"])
        )
        self.assertFalse(affinity["passed"])

        cpu = bench.multicore_gate_summary(
            self.multicore_pairs(
                [0.50] * 5,
                [0.60] * 5,
                four_cpu_percent=149.99,
            ),
            host={"physical_cores": 4, "logical_cpus": 4},
        )
        self.assertTrue(
            any("process CPU" in reason for reason in cpu["invalid_reasons"])
        )
        self.assertFalse(cpu["passed"])

    def test_multicore_gate_requires_odd_alternating_five_pair_minimum(self) -> None:
        too_few = self.multicore_pairs([0.50] * 3, [0.60] * 3)
        with self.assertRaisesRegex(bench.BenchmarkError, "at least 5"):
            bench.multicore_gate_summary(
                too_few,
                host={"physical_cores": 4, "logical_cpus": 4},
            )

        wrong_order = self.multicore_pairs([0.50] * 5, [0.60] * 5)
        wrong_order[1]["order"] = [1, 4]
        with self.assertRaisesRegex(bench.BenchmarkError, "alternat"):
            bench.multicore_gate_summary(
                wrong_order,
                host={"physical_cores": 4, "logical_cpus": 4},
            )

    def test_nearest_rank_percentiles_and_summary(self) -> None:
        values = [5.0, 1.0, 4.0, 2.0, 3.0]
        self.assertEqual(bench.nearest_rank(values, 0.50), 3.0)
        self.assertEqual(bench.nearest_rank(values, 0.95), 5.0)
        self.assertEqual(
            bench.timer_summary(values),
            {"p50_ms": 3.0, "p95_ms": 5.0, "p99_ms": 5.0, "max_ms": 5.0},
        )

    def test_v6_summary_has_median_mad_p95_and_best(self) -> None:
        summary = bench.duration_summary([1.0, 2.0, 10.0])
        self.assertEqual(summary["median_s"], 2.0)
        self.assertEqual(summary["mad_s"], 1.0)
        self.assertEqual(summary["p95_s"], 10.0)
        self.assertEqual(summary["best_s"], 1.0)

    def test_v6_startup_split_reports_whole_process_and_loop_estimates(self) -> None:
        split = bench.v6_startup_loop_summary(
            [0.005, 0.006, 0.004],
            {
                "int32": [0.037, 0.038, 0.036],
                "int64": [0.015, 0.016, 0.014],
            },
        )
        self.assertEqual(split["startup"]["median_s"], 0.005)
        self.assertAlmostEqual(split["loop_estimate"]["int32"]["median_s"], 0.032)
        self.assertAlmostEqual(split["loop_estimate"]["int64"]["median_s"], 0.010)
        self.assertEqual(
            split["method"],
            "paired whole-process duration minus the same repetition's startup duration",
        )

    def test_v6_startup_split_rejects_impossible_negative_noise_pairs(self) -> None:
        split = bench.v6_startup_loop_summary(
            [0.005, 0.020, 0.004],
            {
                "int32": [0.037, 0.038, 0.036],
                "int64": [0.015, 0.016, 0.014],
            },
        )
        int64 = split["loop_estimate"]["int64"]
        self.assertEqual(int64["invalid_negative_pair_repetitions"], [1])
        self.assertEqual(int64["valid_repetitions"], [0, 2])
        self.assertEqual(len(int64["samples_s"]), 2)
        self.assertGreaterEqual(int64["best_s"], 0.0)

    def test_timer_gate_uses_worst_valid_run_and_reports_invalid_runs(self) -> None:
        runs = [
            {
                "arm_span_ms": 2.0,
                "arm_span_valid": True,
                "summary": {"p99_ms": 6.0},
            },
            {
                "arm_span_ms": 3.0,
                "arm_span_valid": True,
                "summary": {"p99_ms": 0.0},
            },
            {
                "arm_span_ms": 11.0,
                "arm_span_valid": False,
                "summary": {"p99_ms": 100.0},
            },
        ]
        summary = bench.timer_gate_summary(runs)
        self.assertEqual(summary["worst_valid_run_p99_ms"], 6.0)
        self.assertEqual(summary["valid_run_indexes"], [0, 1])
        self.assertEqual(summary["invalid_overlap_runs"], [2])

    def test_starvation_gate_uses_worst_repetition(self) -> None:
        summary = bench.starvation_gate_summary(
            [{"elapsed_ms": 12}, {"elapsed_ms": 49}, {"elapsed_ms": 20}]
        )
        self.assertEqual(summary["observed_max_ms"], 49)
        self.assertEqual(summary["limit_ms"], 50)
        self.assertTrue(summary["passed"])
        self.assertFalse(
            bench.starvation_gate_summary([{"elapsed_ms": 51}])["passed"]
        )

    def test_massive_gate_is_joint_rss_timer_and_overlap_evidence(self) -> None:
        passing_runs = [
            {
                "peak_rss_bytes": 1024,
                "incremental_peak_rss_bytes": 1024,
                "arm_span_ms": 3.0,
                "arm_span_valid": True,
                "summary": {"p99_ms": 4.0},
            },
            {
                "peak_rss_bytes": 2048,
                "incremental_peak_rss_bytes": 2048,
                "arm_span_ms": 5.0,
                "arm_span_valid": True,
                "summary": {"p99_ms": 5.0},
            },
        ]
        gate = bench.massive_gate_summary(passing_runs)
        self.assertEqual(gate["observed_peak_rss_bytes"], 2048)
        self.assertEqual(gate["observed_incremental_peak_rss_bytes"], 2048)
        self.assertEqual(gate["observed_timer_p99_ms"], 5.0)
        self.assertTrue(gate["passed"])

        over_rss = [dict(passing_runs[0])]
        over_rss[0]["peak_rss_bytes"] = (
            bench.MASSIVE_RSS_LIMIT_BYTES + 1
        )
        self.assertFalse(bench.massive_gate_summary(over_rss)["passed"])

        invalid_overlap = [dict(passing_runs[0], arm_span_valid=False)]
        self.assertFalse(
            bench.massive_gate_summary(invalid_overlap)["passed"]
        )

    def test_sleepers_gate_uses_whole_process_peak_but_reports_incremental(self) -> None:
        runs = [
            {
                "peak_rss_bytes": bench.SLEEPER_RSS_LIMIT_BYTES + 1,
                "incremental_peak_rss_bytes": 1024,
            }
        ]
        gate = bench.rss_gate_summary(
            runs,
            limit_bytes=bench.SLEEPER_RSS_LIMIT_BYTES,
        )
        self.assertEqual(
            gate["observed_peak_rss_bytes"],
            bench.SLEEPER_RSS_LIMIT_BYTES + 1,
        )
        self.assertEqual(gate["observed_incremental_peak_rss_bytes"], 1024)
        self.assertFalse(gate["passed"])

    def test_massive_ready_and_cleanup_timeouts_are_both_300_seconds(self) -> None:
        self.assertEqual(bench.MASSIVE_READY_TIMEOUT_SECONDS, 300.0)
        self.assertEqual(bench.MASSIVE_COMPLETION_TIMEOUT_SECONDS, 300.0)


class ProcessUnitTests(unittest.TestCase):
    def test_rss_units_are_normalized_to_bytes(self) -> None:
        self.assertEqual(bench.parse_macos_ps_rss_bytes("  2048\n"), 2 * 1024 * 1024)
        status = "Name:\taura\nVmRSS:\t1536 kB\n"
        self.assertEqual(bench.parse_linux_status_rss_bytes(status), 1536 * 1024)
        with self.assertRaisesRegex(bench.BenchmarkError, "VmRSS"):
            bench.parse_linux_status_rss_bytes("Name:\taura\n")

    def test_linux_zombie_sample_is_natural_completion_not_invalid_rss(self) -> None:
        stat_fields = ["123", "(aura)", "Z"] + ["0"] * 12
        with self.assertRaises(ProcessLookupError):
            bench.parse_linux_process_stats_records(
                "Name:\taura\nState:\tZ (zombie)\n",
                " ".join(stat_fields),
                ticks=100,
                pid=123,
            )

    def test_linux_disappearing_proc_record_is_natural_completion(self) -> None:
        with mock.patch.object(Path, "read_text", side_effect=FileNotFoundError):
            with self.assertRaises(ProcessLookupError):
                bench.read_linux_process_stats(123)

    def test_cpu_time_parser_handles_portable_ps_shapes(self) -> None:
        self.assertEqual(bench.parse_ps_cpu_seconds("02:03"), 123.0)
        self.assertEqual(bench.parse_ps_cpu_seconds("01:02:03"), 3723.0)
        self.assertEqual(bench.parse_ps_cpu_seconds("2-01:02:03"), 176523.0)

    def test_macos_rusage_parser_reads_resident_bytes_and_nanosecond_cpu(self) -> None:
        record = bytearray(160)
        struct.pack_into("=Q", record, 16, 1_250_000_000)
        struct.pack_into("=Q", record, 24, 750_000_000)
        struct.pack_into("=Q", record, 64, 123_456_789)
        stats = bench.parse_macos_rusage_v2(bytes(record))
        self.assertEqual(stats.rss_bytes, 123_456_789)
        self.assertEqual(stats.cpu_seconds, 2.0)

    def test_macos_rusage_cpu_ticks_use_the_host_mach_timebase(self) -> None:
        record = bytearray(160)
        struct.pack_into("=Q", record, 16, 12_000_000)
        struct.pack_into("=Q", record, 24, 12_000_000)
        struct.pack_into("=Q", record, 64, 4096)
        stats = bench.parse_macos_rusage_v2(
            bytes(record),
            timebase_numer=125,
            timebase_denom=3,
        )
        self.assertEqual(stats.rss_bytes, 4096)
        self.assertEqual(stats.cpu_seconds, 1.0)

    def test_timer_monitor_never_uses_ps_as_a_sampling_fallback(self) -> None:
        with mock.patch.object(bench.platform, "system", return_value="Darwin"):
            with mock.patch.object(
                bench, "read_macos_proc_pid_rusage", side_effect=OSError("unavailable")
            ):
                with mock.patch.object(bench.subprocess, "run") as run:
                    monitor = bench.ProcessMonitor(
                        os.getpid(),
                        sample_interval_seconds=0.001,
                        allow_macos_ps_fallback=False,
                    )
                    monitor.start()
                    time.sleep(0.01)
                    monitor.stop()
        run.assert_not_called()
        self.assertEqual(monitor.samples, [])
        self.assertFalse(monitor.is_alive)

    def test_sampling_errors_invalidate_benchmark_evidence(self) -> None:
        monitor = mock.Mock()
        monitor.sampling_error = "proc_pid_rusage failed"
        monitor.samples = [{"rss_bytes": 1}]
        with self.assertRaisesRegex(bench.BenchmarkError, "sampling failed"):
            bench.require_monitor_evidence(monitor, "massive")

        monitor.sampling_error = None
        monitor.samples = []
        with self.assertRaisesRegex(bench.BenchmarkError, "no process samples"):
            bench.require_monitor_evidence(monitor, "massive")

    @unittest.skipUnless(hasattr(os, "killpg"), "requires POSIX process groups")
    def test_owned_process_group_reaps_a_term_resistant_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_pid_path = root / "child.pid"
            helper = root / "process-tree.py"
            helper.write_text(
                "#!/usr/bin/env python3\n"
                "import os\n"
                "import signal\n"
                "import time\n"
                "child = os.fork()\n"
                "if child == 0:\n"
                "    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                f"    open({str(child_pid_path)!r}, 'w').write(str(os.getpid()))\n"
                "    while True:\n"
                "        time.sleep(1)\n"
                "os._exit(0)\n",
                encoding="utf-8",
            )
            helper.chmod(helper.stat().st_mode | stat.S_IXUSR)

            process = bench.launch_owned_process(
                [str(helper)],
                stdin=bench.subprocess.DEVNULL,
                stdout=bench.subprocess.PIPE,
                stderr=bench.subprocess.PIPE,
            )
            process.wait(timeout=2.0)
            deadline = time.monotonic() + 2.0
            while not child_pid_path.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            child_pid = int(child_pid_path.read_text(encoding="ascii"))
            self.assertEqual(os.getpgid(child_pid), process.pid)

            bench.reap_owned_process_group(
                process,
                "test process tree",
                terminate_timeout_seconds=0.05,
                kill_timeout_seconds=2.0,
            )
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)

    @unittest.skipUnless(hasattr(os, "killpg"), "requires POSIX process groups")
    def test_owned_group_cleanup_detects_a_silently_ignored_kill(self) -> None:
        process = mock.Mock()
        process.pid = 424242
        process.stdin = None
        process.stdout = None
        process.stderr = None
        process.poll.return_value = None
        process.wait.side_effect = bench.subprocess.TimeoutExpired(
            ["fake-burner"], 0
        )

        with mock.patch.object(
            bench.benchmark_process, "process_group_exists", return_value=True
        ):
            with mock.patch.object(bench.benchmark_process.os, "killpg") as killpg:
                with mock.patch.object(process, "kill"):
                    with self.assertRaisesRegex(
                        bench.BenchmarkError, "still alive after SIGKILL"
                    ):
                        bench.reap_owned_process_group(
                            process,
                            "fake burner",
                            terminate_timeout_seconds=0.0,
                            kill_timeout_seconds=0.0,
                        )
        self.assertEqual(
            killpg.call_args_list,
            [
                mock.call(process.pid, signal.SIGTERM),
                mock.call(process.pid, signal.SIGKILL),
            ],
        )

    def test_owned_process_cleanup_runs_when_communicate_is_interrupted(self) -> None:
        process = mock.Mock()
        process.communicate.side_effect = KeyboardInterrupt()
        with mock.patch.object(
            bench, "launch_owned_process", return_value=process
        ):
            with mock.patch.object(bench, "reap_owned_process_group") as reap:
                with self.assertRaises(KeyboardInterrupt):
                    bench.run_owned_process(
                        ["/tmp/fake-benchmark"],
                        "interrupt probe",
                    )
        reap.assert_called_once_with(process, "interrupt probe")

    def test_integer_loop_helper_uses_the_shared_group_guard(self) -> None:
        completed = bench.subprocess.CompletedProcess(
            ["/tmp/int64-loop"], 0, b"10000000\n", b""
        )
        with mock.patch.object(
            direct_bench.benchmark_process,
            "run_process_group",
            return_value=completed,
        ) as run:
            elapsed = direct_bench.measure(Path("/tmp/int64-loop"), 2)
        self.assertGreaterEqual(elapsed, 0.0)
        self.assertEqual(run.call_count, 2)
        self.assertTrue(
            all(
                call.args[:2]
                == (
                    ["/tmp/int64-loop"],
                    "checked integer loop",
                )
                for call in run.call_args_list
            )
        )


class ValidationAndExecutionTests(unittest.TestCase):
    def make_executable(self, root: Path, name: str, body: str) -> Path:
        path = root / name
        path.write_text(
            "#!/bin/sh\n" + body,
            encoding="utf-8",
        )
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def test_cli_validation_rejects_debug_aura_and_target_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            debug = root / "target/debug/aura"
            debug.parent.mkdir(parents=True)
            self.make_executable(debug.parent, "aura", 'printf "aura 0.1.0\\n"\n')
            with self.assertRaisesRegex(bench.BenchmarkError, "debug"):
                bench.validate_options(
                    bench.Options(
                        label="baseline",
                        aura=debug,
                        repeats=1,
                        timer_repeats=1,
                        v6_repeats=1,
                        multicore_repeats=7,
                        idle_seconds=0.01,
                        json_path=root / "result.json",
                        allow_competing_processes=False,
                    ),
                    root=root,
                )

            release = root / "target/release/aura"
            release.parent.mkdir(parents=True)
            self.make_executable(release.parent, "aura", 'printf "aura 0.1.0\\n"\n')
            with self.assertRaisesRegex(bench.BenchmarkError, "outside target"):
                bench.validate_options(
                    bench.Options(
                        label="baseline",
                        aura=release,
                        repeats=1,
                        timer_repeats=1,
                        v6_repeats=1,
                        multicore_repeats=7,
                        idle_seconds=0.01,
                        json_path=root / "target/result.json",
                        allow_competing_processes=False,
                    ),
                    root=root,
                )

    def test_cli_defaults_to_seven_multicore_pairs_and_rejects_even_counts(
        self,
    ) -> None:
        options = bench.parse_options(
            [
                "--label",
                "phase57",
                "--aura",
                "/tmp/aura",
                "--json",
                "/tmp/report.json",
            ]
        )
        self.assertEqual(options.multicore_repeats, 7)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            aura = self.make_executable(root, "aura", "exit 0\n")
            with self.assertRaisesRegex(bench.BenchmarkError, "odd and at least 5"):
                bench.validate_options(
                    bench.Options(
                        label="phase57",
                        aura=aura,
                        repeats=1,
                        timer_repeats=1,
                        v6_repeats=1,
                        multicore_repeats=6,
                        idle_seconds=1.0,
                        json_path=root / "report.json",
                        allow_competing_processes=False,
                    ),
                    root=root,
                )

    def test_aura_qualification_requires_checkout_release_binary_and_fresh_build(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = root / "target/release/aura"
            expected.parent.mkdir(parents=True)
            self.make_executable(expected.parent, "aura", 'printf "aura\\n"\n')
            outside = self.make_executable(root, "aura", 'printf "aura\\n"\n')

            with self.assertRaisesRegex(bench.BenchmarkError, "target/release"):
                bench.qualify_aura_binary(outside, root=root)

            completed = mock.Mock(returncode=0, stdout=b"", stderr=b"")
            with mock.patch.object(
                bench.subprocess, "run", return_value=completed
            ) as run:
                record = bench.qualify_aura_binary(expected, root=root)
            self.assertEqual(record["path"], str(expected.resolve()))
            self.assertTrue(record["fresh_cargo_build"])
            command = run.call_args.args[0]
            self.assertEqual(
                command,
                [
                    "cargo",
                    "build",
                    "--release",
                    "--locked",
                    "-p",
                    "aura",
                    "--target-dir",
                    str((root / "target").resolve()),
                ],
            )

    def test_cli_validation_rejects_nonpositive_counts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            aura = self.make_executable(root, "aura", 'printf "aura 0.1.0\\n"\n')
            with self.assertRaisesRegex(bench.BenchmarkError, "positive"):
                bench.validate_options(
                    bench.Options(
                        label="baseline",
                        aura=aura,
                        repeats=0,
                        timer_repeats=1,
                        v6_repeats=1,
                        multicore_repeats=7,
                        idle_seconds=0.01,
                        json_path=root / "result.json",
                        allow_competing_processes=False,
                    ),
                    root=root,
                )

    def test_v6_fake_binary_requires_exact_stdout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valid = self.make_executable(root, "valid", 'printf "10000000\\n"\n')
            invalid = self.make_executable(
                root, "invalid", 'printf "10000000 extra\\n"\n'
            )
            result = bench.run_v6_once(valid)
            self.assertEqual(result["stdout"], "10000000\n")
            self.assertGreaterEqual(result["elapsed_s"], 0.0)
            with self.assertRaisesRegex(bench.BenchmarkError, "stdout"):
                bench.run_v6_once(invalid)

    def test_v6_startup_probe_requires_silent_success(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valid = self.make_executable(root, "valid-startup", "exit 0\n")
            noisy = self.make_executable(
                root, "noisy-startup", 'printf "unexpected\\n"\n'
            )
            result = bench.run_v6_startup_once(valid)
            self.assertEqual(result["stdout"], "")
            self.assertGreaterEqual(result["elapsed_s"], 0.0)
            with self.assertRaisesRegex(bench.BenchmarkError, "stdout"):
                bench.run_v6_startup_once(noisy)

    @unittest.skipUnless(hasattr(os, "killpg"), "requires POSIX process groups")
    def test_v6_probe_reaps_descendants_on_success_and_validation_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, stdout in enumerate(("10000000", "wrong")):
                child_pid_path = root / ("child-" + str(index) + ".pid")
                helper = root / ("v6-tree-" + str(index) + ".py")
                helper.write_text(
                    "#!/usr/bin/env python3\n"
                    "import os\n"
                    "import signal\n"
                    "import sys\n"
                    "import time\n"
                    "child = os.fork()\n"
                    "if child == 0:\n"
                    "    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                    "    sink = os.open(os.devnull, os.O_RDWR)\n"
                    "    os.dup2(sink, 0)\n"
                    "    os.dup2(sink, 1)\n"
                    "    os.dup2(sink, 2)\n"
                    f"    open({str(child_pid_path)!r}, 'w').write(str(os.getpid()))\n"
                    "    while True:\n"
                    "        time.sleep(1)\n"
                    f"print({stdout!r}, flush=True)\n",
                    encoding="utf-8",
                )
                helper.chmod(helper.stat().st_mode | stat.S_IXUSR)
                try:
                    if stdout == "10000000":
                        result = bench.run_v6_once(helper)
                        self.assertEqual(result["stdout"], "10000000\n")
                    else:
                        with self.assertRaisesRegex(
                            bench.BenchmarkError, "stdout"
                        ):
                            bench.run_v6_once(helper)
                    deadline = time.monotonic() + 2.0
                    while (
                        not child_pid_path.exists()
                        and time.monotonic() < deadline
                    ):
                        time.sleep(0.01)
                    child_pid = int(child_pid_path.read_text(encoding="ascii"))
                    with self.assertRaises(ProcessLookupError):
                        os.kill(child_pid, 0)
                finally:
                    if child_pid_path.exists():
                        child_pid = int(child_pid_path.read_text(encoding="ascii"))
                        try:
                            os.kill(child_pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_v6_benchmark_rotates_paired_startup_and_loop_order(self) -> None:
        startup = Path("/bench/startup")
        int32 = Path("/bench/int32")
        int64 = Path("/bench/int64")
        observed = []

        def startup_probe(binary: Path) -> dict:
            observed.append(binary.name)
            return {
                "command": [str(binary)],
                "elapsed_s": 0.001,
                "stdout": "",
                "returncode": 0,
            }

        def loop_probe(binary: Path) -> dict:
            observed.append(binary.name)
            return {
                "command": [str(binary)],
                "elapsed_s": 0.010 if binary.name == "int32" else 0.006,
                "stdout": "10000000\n",
                "returncode": 0,
            }

        with mock.patch.object(
            bench, "run_v6_startup_once", side_effect=startup_probe
        ):
            with mock.patch.object(bench, "run_v6_once", side_effect=loop_probe):
                result = bench.run_v6_benchmark(startup, int32, int64, 3)
        self.assertEqual(
            observed,
            [
                "startup",
                "int32",
                "int64",
                "startup",
                "int32",
                "int64",
                "int32",
                "int64",
                "startup",
                "int64",
                "startup",
                "int32",
            ],
        )
        self.assertEqual(
            [
                run["order"]
                for run in result["runs"]
                if run["workload"] == "startup"
            ],
            [
                ["startup", "int32", "int64"],
                ["int32", "int64", "startup"],
                ["int64", "startup", "int32"],
            ],
        )
        self.assertEqual(len(result["runs"]), 9)
        self.assertTrue(
            all(
                "workload" in run and "width" not in run
                for run in result["runs"]
            )
        )
        self.assertIn("startup_vs_loop", result)

    def test_multicore_run_uses_four_workers_and_exact_go_ack_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = self.make_executable(
                root,
                "multicore",
                'test "$AURA_WORKERS" = "4" || exit 20\n'
                'test "$1" = "1" || exit 21\n'
                'printf "READY multicore 1 0 48271 2147483647\\n"\n'
                "IFS= read -r go\n"
                'test "$go" = "GO multicore" || exit 22\n'
                "sleep 0.05\n"
                'printf "DONE multicore 1 1\\n"\n'
                "IFS= read -r ack\n"
                'test "$ack" = "ACK multicore" || exit 23\n',
            )
            with mock.patch.object(bench, "MULTICORE_ITERATIONS", 0):
                result = bench.run_multicore_once(binary, tasks=1)
        self.assertEqual(result["environment"], {"AURA_WORKERS": "4"})
        self.assertEqual(result["ready_observation"]["tasks"], 1)
        self.assertEqual(result["done_observation"]["checksum"], 1)
        self.assertGreaterEqual(result["elapsed_s"], 0.05)
        self.assertEqual(result["completion"]["returncode"], 0)
        self.assertEqual(result["completion"]["stdout"], "")
        self.assertEqual(result["completion"]["stderr"], "")
        self.assertGreater(len(result["process_samples"]), 0)

    def test_multicore_run_rejects_protocol_noise_and_reaps_child(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            noisy = self.make_executable(
                root,
                "noisy",
                'printf "READY multicore 1 0 48271 2147483647\\n"\n'
                "IFS= read -r go\n"
                'printf "DONE multicore 1 1\\nnoise\\n"\n'
                "IFS= read -r ack\n",
            )
            with mock.patch.object(bench, "MULTICORE_ITERATIONS", 0):
                with self.assertRaisesRegex(bench.BenchmarkError, "trailing"):
                    bench.run_multicore_once(noisy, tasks=1)

    def test_multicore_ready_timeout_reaps_the_child(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pid_path = root / "pid"
            stalled = self.make_executable(
                root,
                "stalled",
                'printf "%s" "$$" > "' + str(pid_path) + '"\n'
                "sleep 5\n",
            )
            with mock.patch.object(bench, "READY_TIMEOUT_SECONDS", 1.0):
                with self.assertRaisesRegex(bench.BenchmarkError, "timeout"):
                    bench.run_multicore_once(stalled, tasks=1)
            pid = int(pid_path.read_text(encoding="ascii"))
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_multicore_benchmark_warms_then_alternates_seven_pairs(self) -> None:
        observations: list[int] = []

        def fake_run(_binary: Path, tasks: int) -> dict[str, object]:
            observations.append(tasks)
            return {
                "elapsed_s": 0.5 if tasks == 1 else 0.6,
                "process_cpu_percent": 100.0 if tasks == 1 else 200.0,
            }

        with mock.patch.object(bench, "run_multicore_once", side_effect=fake_run):
            result = bench.run_multicore_benchmark(
                Path("/tmp/multicore"),
                repeats=7,
            )
        self.assertEqual(observations[:2], [1, 4])
        self.assertEqual(
            observations[2:],
            [1, 4, 4, 1, 1, 4, 4, 1, 1, 4, 4, 1, 1, 4],
        )
        self.assertEqual(len(result["pairs"]), 7)
        self.assertEqual(result["pairs"][0]["order"], [1, 4])
        self.assertEqual(result["pairs"][1]["order"], [4, 1])

    def test_starvation_run_records_elapsed_sleep_and_rejects_noise(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valid = self.make_executable(
                root,
                "valid-starvation",
                'test "$AURA_WORKERS" = "1" || exit 20\n'
                'printf "SAMPLE starvation 10 17\\nDONE starvation\\n"\n',
            )
            invalid = self.make_executable(
                root,
                "invalid-starvation",
                'printf "SAMPLE starvation 10 17\\nDONE starvation\\nnoise\\n"\n',
            )
            result = bench.run_starvation(valid)
            self.assertEqual(result["sleep_ms"], 10)
            self.assertEqual(result["elapsed_ms"], 17)
            self.assertEqual(result["returncode"], 0)
            self.assertEqual(result["environment"], {"AURA_WORKERS": "1"})
            with self.assertRaisesRegex(bench.BenchmarkError, "trailing"):
                bench.run_starvation(invalid)

    def test_controlled_runtime_environment_scrubs_ambient_worker_override(self) -> None:
        with mock.patch.dict(os.environ, {"AURA_WORKERS": "99"}):
            default_environment = bench.controlled_runtime_environment()
            single_worker_environment = bench.controlled_runtime_environment(
                worker_count=1
            )
        self.assertNotIn("AURA_WORKERS", default_environment)
        self.assertEqual(single_worker_environment["AURA_WORKERS"], "1")

    def test_massive_run_records_incremental_rss_and_timer_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = self.make_executable(
                Path(directory),
                "massive",
                'printf "BASELINE massive 2 2 10\\n"\n'
                "sleep 0.02\n"
                'printf "READY massive 2 2 10 100 101\\n"\n'
                "sleep 0.02\n"
                'printf "SAMPLE massive_timer 1 0.2\\n"\n'
                'printf "SAMPLE massive_timer 0 0.1\\n"\n'
                'printf "DONE massive 2 2\\n"\n',
            )
            with mock.patch.object(bench, "MASSIVE_SLEEPER_COUNT", 2):
                with mock.patch.object(bench, "MASSIVE_TIMER_COUNT", 2):
                    result = bench.run_massive(binary)
        self.assertEqual(result["ready_observation"]["sleepers"], 2)
        self.assertEqual(result["ready_observation"]["timer_count"], 2)
        self.assertEqual(result["summary"]["p99_ms"], 0.2)
        self.assertGreaterEqual(result["incremental_peak_rss_bytes"], 0)
        self.assertEqual(result["returncode"], 0)

    def test_sleepers_waits_for_exact_natural_completion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = self.make_executable(
                Path(directory),
                "sleepers",
                'printf "BASELINE sleepers 10000\\n"\n'
                'printf "READY sleepers 10000\\n"\n'
                "sleep 0.05\n"
                'printf "DONE sleepers 10000\\n"\n',
            )
            result = bench.run_sleepers(binary, stable_seconds=0.01)
        self.assertEqual(result["completion"]["returncode"], 0)
        self.assertEqual(result["completion"]["stdout"], "DONE sleepers 10000\n")
        self.assertEqual(result["completion"]["stderr"], "")
        self.assertGreaterEqual(result["ready_to_done_s"], 0.01)
        self.assertGreaterEqual(result["incremental_peak_rss_bytes"], 0)

    def test_sleepers_rejects_done_before_required_stable_window(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = self.make_executable(
                Path(directory),
                "sleepers",
                'printf "BASELINE sleepers 10000\\n"\n'
                'printf "READY sleepers 10000\\n"\n'
                "sleep 0.01\n"
                'printf "DONE sleepers 10000\\n"\n',
            )
            with self.assertRaisesRegex(bench.BenchmarkError, "completed too early"):
                bench.run_sleepers(binary, stable_seconds=0.1)

    def test_idle_waits_for_exact_natural_completion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = self.make_executable(
                Path(directory),
                "idle",
                'printf "READY idle 10 30000\\n"\n'
                # `run_idle` samples process statistics after READY and only
                # then begins its stability window. Keep the fixture alive
                # long enough for a contended hosted macOS `ps` call without
                # weakening the 10 ms behavior being asserted.
                "sleep 1\n"
                'printf "DONE idle 10\\n"\n',
            )
            result = bench.run_idle(binary, stable_seconds=0.01)
        self.assertEqual(result["completion"]["returncode"], 0)
        self.assertEqual(result["completion"]["stdout"], "DONE idle 10\n")
        self.assertEqual(result["completion"]["stderr"], "")

    def test_natural_completion_rejects_stderr_nonzero_and_wrong_done(self) -> None:
        cases = [
            ('printf "WRONG\\n"\n', "stdout"),
            ('printf "DONE sleepers 10000\\n"\nprintf "noise" >&2\n', "stderr"),
            ('printf "DONE sleepers 10000\\n"\nexit 7\n', "status 7"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, (tail, expected) in enumerate(cases):
                binary = self.make_executable(
                    root,
                    "sleepers-" + str(index),
                    'printf "BASELINE sleepers 10000\\n"\n'
                    'printf "READY sleepers 10000\\n"\n'
                    "sleep 0.01\n"
                    + tail,
                )
                with self.subTest(expected=expected):
                    with self.assertRaisesRegex(bench.BenchmarkError, expected):
                        bench.run_sleepers(binary, stable_seconds=0.001)

    def test_validation_allows_the_advertised_idle_window(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            aura = self.make_executable(root, "aura", 'printf "aura 0.1.0\\n"\n')
            bench.validate_options(
                bench.Options(
                    label="baseline",
                    aura=aura,
                    repeats=1,
                    timer_repeats=1,
                    v6_repeats=1,
                    multicore_repeats=7,
                    idle_seconds=30.0,
                    json_path=root / "result.json",
                    allow_competing_processes=False,
                ),
                root=root,
            )

    def test_process_inventory_filters_to_repo_competitors(self) -> None:
        root = Path("/repo")
        rows = [
            bench.ProcessRow(10, "cargo", "cargo test", Path("/repo")),
            bench.ProcessRow(11, "rustc", "rustc --crate-name x", Path("/other")),
            bench.ProcessRow(12, "aura", "/repo/target/release/aura run x.au", None),
            bench.ProcessRow(os.getpid(), "cargo", "cargo test", Path("/repo")),
        ]
        competitors = bench.find_competing_processes(
            root, rows=rows, ignored_pids={os.getpid()}
        )
        self.assertEqual([process.pid for process in competitors], [10, 12])

    def test_contractual_status_requires_both_quiet_checks_and_no_override(self) -> None:
        competitor = bench.ProcessRow(
            10, "cargo", "cargo test", Path("/repo")
        )
        self.assertTrue(bench.benchmark_is_contractual(False, ([], [])))
        self.assertFalse(
            bench.benchmark_is_contractual(False, ([], [competitor]))
        )
        self.assertFalse(bench.benchmark_is_contractual(True, ([], [])))
        self.assertEqual(
            bench.benchmark_noncontractual_reasons(True, ([], [competitor])),
            [
                "the competing-process override was enabled",
                "competing Aura-repository processes were observed",
            ],
        )

    def test_contractual_status_requires_clean_mac14_9_evidence(self) -> None:
        clean_repository = {"dirty_files": []}
        dirty_repository = {"dirty_files": [" M crates/runtime.rs"]}
        baseline_host = {"hardware_model": "Mac14,9"}
        other_host = {"hardware_model": "Mac15,6"}

        self.assertTrue(
            bench.benchmark_is_contractual(
                False,
                ([], []),
                host=baseline_host,
                repository=clean_repository,
            )
        )
        self.assertEqual(
            bench.benchmark_noncontractual_reasons(
                False,
                ([], []),
                host=other_host,
                repository=dirty_repository,
            ),
            [
                "host hardware model is not the contractual Mac14,9 baseline",
                "repository worktree was dirty",
            ],
        )

    def test_execute_rechecks_process_inventory_after_workload_builds(self) -> None:
        competitor = bench.ProcessRow(
            10, "cargo", "cargo test", Path("/repo")
        )
        options = bench.Options(
            label="baseline",
            aura=Path("/tmp/release/aura"),
            repeats=1,
            timer_repeats=1,
            v6_repeats=1,
            multicore_repeats=7,
            idle_seconds=1.0,
            json_path=Path("/tmp/result.json"),
            allow_competing_processes=False,
        )
        with mock.patch.object(bench, "validate_options"):
            with mock.patch.object(
                bench, "qualify_aura_binary", return_value={}
            ):
                with mock.patch.object(
                    bench,
                    "find_competing_processes",
                    side_effect=[[], [competitor]],
                ):
                    with mock.patch.object(
                        bench, "hardware_record", return_value={}
                    ):
                        with mock.patch.object(
                            bench, "repository_record", return_value={}
                        ):
                            with mock.patch.object(
                                bench, "build_workloads", return_value=({}, [])
                            ):
                                with mock.patch.object(
                                    bench,
                                    "compiler_runtime_inputs",
                                    return_value={},
                                ):
                                    with self.assertRaisesRegex(
                                        bench.BenchmarkError,
                                        "immediately before timing",
                                    ):
                                        bench.execute(options)


if __name__ == "__main__":
    unittest.main()
