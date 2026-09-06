#!/usr/bin/env python3
"""Measure Aura's one-million-element float64 add and sum against NumPy.

This runner produces release evidence, not a performance gate. It builds the
Aura workloads with the direct backend before timing, starts every measured
process in an owned process group, times only the exact GO/DONE protocol
window, and retains every raw paired observation with host and input identity.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import platform
import selectors
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from typing import Dict, List, NamedTuple, Optional, Sequence

try:
    from scripts import benchmark_process, rust_baselines
except ImportError:
    import benchmark_process
    import rust_baselines


ROOT = pathlib.Path(__file__).resolve().parent.parent
REPORT_SCHEMA_VERSION = 2
ELEMENT_COUNT = 1_000_000
PAIR_COUNT = 11
ADD_ITERATIONS = 512
SUM_ITERATIONS = 1_024
READY_TIMEOUT_SECONDS = 30.0
COMPLETION_TIMEOUT_SECONDS = 120.0
BUILD_TIMEOUT_SECONDS = 1800.0
MAX_PROTOCOL_LINE_BYTES = 512
COMPETING_CPU_PERCENT = 50.0
QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS = 0.25
REPOSITORY_PROCESS_EXECUTABLES = {"cargo", "rustc", "aura"}
PROCESS_INVENTORY_EXECUTABLES = {"ps", "lsof"}

SINGLE_THREAD_ENVIRONMENT = {
    "AURA_WORKERS": "1",
    "OMP_NUM_THREADS": "1",
    "VECLIB_MAXIMUM_THREADS": "1",
    "OPENBLAS_NUM_THREADS": "1",
    "MKL_NUM_THREADS": "1",
    "NUMEXPR_NUM_THREADS": "1",
}

LANES: Dict[str, Dict[str, object]] = {
    "aura_add": {
        "implementation": "aura",
        "workload": "add",
        "iterations": ADD_ITERATIONS,
        "expected_checksum": 2_048.0,
    },
    "numpy_add": {
        "implementation": "numpy",
        "workload": "add",
        "iterations": ADD_ITERATIONS,
        "expected_checksum": 2_048.0,
    },
    "aura_sum": {
        "implementation": "aura",
        "workload": "sum",
        "iterations": SUM_ITERATIONS,
        "expected_checksum": 4_096_000_000.0,
    },
    "numpy_sum": {
        "implementation": "numpy",
        "workload": "sum",
        "iterations": SUM_ITERATIONS,
        "expected_checksum": 4_096_000_000.0,
    },
}

for _workload in ("add", "sum"):
    LANES["rust_" + _workload] = {**LANES["aura_" + _workload], "implementation": "rust"}


class BenchmarkError(RuntimeError):
    """A benchmark cannot produce trustworthy evidence."""


class Options(NamedTuple):
    label: str
    aura: pathlib.Path
    python: pathlib.Path
    pairs: int
    raw_json: pathlib.Path
    summary_json: pathlib.Path
    allow_competing_processes: bool


class ProcessSample(NamedTuple):
    pid: int
    parent_pid: int
    cpu_percent: float
    command: str
    arguments: str


def sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def nearest_rank(values: Sequence[float], percentile: float) -> float:
    if not values:
        raise BenchmarkError("cannot summarize an empty sample")
    ordered = sorted(float(value) for value in values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def duration_summary(values: Sequence[float]) -> Dict[str, object]:
    if not values or any(not math.isfinite(value) or value <= 0 for value in values):
        raise BenchmarkError("duration samples must be positive and finite")
    samples = [float(value) for value in values]
    median = statistics.median(samples)
    return {
        "samples_s": samples,
        "median_s": median,
        "mad_s": statistics.median(abs(value - median) for value in samples),
        "p95_s": nearest_rank(samples, 0.95),
        "best_s": min(samples),
    }


def parse_ready_line(
    line: bytes, *, workload: str, iterations: int
) -> Dict[str, object]:
    expected = f"READY numeric-arrays {workload} {ELEMENT_COUNT} {iterations}\n".encode(
        "ascii"
    )
    if line != expected:
        raise BenchmarkError("unexpected READY line: " + repr(line))
    return {
        "workload": workload,
        "elements": ELEMENT_COUNT,
        "iterations": iterations,
    }


def parse_done_line(
    line: bytes,
    *,
    workload: str,
    iterations: int,
    expected_checksum: float,
) -> Dict[str, object]:
    try:
        text = line.decode("ascii")
    except UnicodeDecodeError as error:
        raise BenchmarkError("DONE line is not ASCII") from error
    fields = text.rstrip("\n").split(" ")
    if (
        len(fields) != 5
        or fields[:3] != ["DONE", "numeric-arrays", workload]
        or fields[3] != str(iterations)
        or not text.endswith("\n")
    ):
        raise BenchmarkError("unexpected DONE line: " + repr(line))
    try:
        checksum = float(fields[4])
    except ValueError as error:
        raise BenchmarkError("invalid checksum") from error
    if not math.isfinite(checksum):
        raise BenchmarkError("checksum must be finite")
    if checksum != expected_checksum:
        raise BenchmarkError(
            f"checksum mismatch: expected {expected_checksum}, found {checksum}"
        )
    return {
        "workload": workload,
        "iterations": iterations,
        "checksum": checksum,
    }


def pair_order(repeat: int) -> List[str]:
    forward = list(LANES)
    return forward if repeat % 2 == 0 else list(reversed(forward))


def summarize_pairs(pairs: Sequence[Dict[str, object]]) -> Dict[str, object]:
    result: Dict[str, object] = {}
    for workload in ("add", "sum"):
        iterations = int(
            LANES[f"aura_{workload}"]["iterations"]  # type: ignore[arg-type]
        )
        aura = [
            float(pair["runs"][f"aura_{workload}"]["elapsed_s"])  # type: ignore[index]
            for pair in pairs
        ]
        numpy = [
            float(pair["runs"][f"numpy_{workload}"]["elapsed_s"])  # type: ignore[index]
            for pair in pairs
        ]
        paired_ratios = [
            aura_value / numpy_value
            for aura_value, numpy_value in zip(aura, numpy)
        ]
        aura_summary = duration_summary(aura)
        numpy_summary = duration_summary(numpy)
        aura_summary["median_per_operation_s"] = (
            float(aura_summary["median_s"]) / iterations
        )
        numpy_summary["median_per_operation_s"] = (
            float(numpy_summary["median_s"]) / iterations
        )
        result[workload] = {
            "elements": ELEMENT_COUNT,
            "dtype": "float64",
            "iterations_per_observation": iterations,
            "aura": aura_summary,
            "numpy": numpy_summary,
            "paired_ratios": paired_ratios,
            "paired_median_ratio": statistics.median(paired_ratios),
            "ratio_of_medians": (
                float(aura_summary["median_s"]) / float(numpy_summary["median_s"])
            ),
        }
        rust = [float(pair["runs"][f"rust_{workload}"]["elapsed_s"]) for pair in pairs]
        rust_summary = duration_summary(rust)
        rust_summary["median_per_operation_s"] = float(rust_summary["median_s"]) / iterations
        result[workload]["rust"] = rust_summary
        ratios = [a / r for a, r in zip(aura, rust)]
        result[workload]["aura_vs_rust"] = {
            "paired_ratios": ratios, "paired_median_ratio": statistics.median(ratios),
            "ratio_of_medians": float(aura_summary["median_s"]) / float(rust_summary["median_s"]),
        }
    return result


def read_line_with_timeout(
    process: subprocess.Popen[bytes], timeout_seconds: float, label: str
) -> bytes:
    assert process.stdout is not None
    descriptor = process.stdout.fileno()
    deadline = time.monotonic() + timeout_seconds
    line = bytearray()
    selector = selectors.DefaultSelector()
    selector.register(descriptor, selectors.EVENT_READ)
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise BenchmarkError(label + " timed out")
            chunk = os.read(descriptor, 1)
            if not chunk:
                raise BenchmarkError(label + " exited before emitting a complete line")
            line.extend(chunk)
            if len(line) > MAX_PROTOCOL_LINE_BYTES:
                raise BenchmarkError(label + " exceeded the protocol line bound")
            if chunk == b"\n":
                break
    finally:
        selector.close()
    return bytes(line)


def run_lane(lane: str, commands: Dict[str, List[str]]) -> Dict[str, object]:
    contract = LANES[lane]
    workload = str(contract["workload"])
    iterations = int(contract["iterations"])
    expected_checksum = float(contract["expected_checksum"])
    environment = os.environ.copy()
    environment.update(SINGLE_THREAD_ENVIRONMENT)
    command = commands[lane]

    with benchmark_process.owned_process_group(
        command,
        lane,
        cwd=ROOT,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ) as process:
        ready_line = read_line_with_timeout(
            process, READY_TIMEOUT_SECONDS, lane + " READY"
        )
        ready = parse_ready_line(ready_line, workload=workload, iterations=iterations)
        assert process.stdin is not None
        started_ns = time.perf_counter_ns()
        process.stdin.write(f"GO numeric-arrays {workload}\n".encode("ascii"))
        process.stdin.flush()
        done_line = read_line_with_timeout(
            process, COMPLETION_TIMEOUT_SECONDS, lane + " DONE"
        )
        elapsed_s = (time.perf_counter_ns() - started_ns) / 1_000_000_000.0
        done = parse_done_line(
            done_line,
            workload=workload,
            iterations=iterations,
            expected_checksum=expected_checksum,
        )
        process.stdin.close()
        process.stdin = None
        remaining_stdout, stderr = process.communicate(
            timeout=COMPLETION_TIMEOUT_SECONDS
        )
        if process.returncode != 0:
            raise BenchmarkError(f"{lane} exited with status {process.returncode}")
        if remaining_stdout:
            raise BenchmarkError(lane + " emitted trailing stdout")
        if stderr:
            raise BenchmarkError(
                lane + " emitted stderr: " + stderr.decode("utf-8", errors="replace")
            )
    return {
        "command": command,
        "environment": dict(SINGLE_THREAD_ENVIRONMENT),
        "ready": ready,
        "done": done,
        "elapsed_s": elapsed_s,
        "checksum": done["checksum"],
        "returncode": 0,
    }


def command_output(command: Sequence[str]) -> Optional[str]:
    try:
        result = subprocess.run(
            list(command),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
        )
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def hardware_record() -> Dict[str, object]:
    uname = platform.uname()
    hardware_model = command_output(["sysctl", "-n", "hw.model"])
    cpu_model = command_output(["sysctl", "-n", "machdep.cpu.brand_string"])
    memory = command_output(["sysctl", "-n", "hw.memsize"])
    physical = command_output(["sysctl", "-n", "hw.physicalcpu"])
    boot_time = command_output(["sysctl", "-n", "kern.boottime"])
    return {
        "system": platform.system(),
        "release": uname.release,
        "version": uname.version,
        "machine": uname.machine,
        "hardware_model": hardware_model,
        "cpu_model": cpu_model,
        "logical_cpus": os.cpu_count(),
        "physical_cores": int(physical) if physical else None,
        "memory_bytes": int(memory) if memory else None,
        "boot_time": boot_time,
        "python": platform.python_version(),
        "load_average": list(os.getloadavg()),
    }


def repository_record() -> Dict[str, object]:
    commit = command_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    branch = command_output(
        ["git", "-C", str(ROOT), "symbolic-ref", "--quiet", "--short", "HEAD"]
    )
    status = subprocess.run(
        ["git", "-C", str(ROOT), "status", "--porcelain=v1", "-z"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    ).stdout
    return {
        "root": str(ROOT),
        "commit": commit,
        "branch": branch,
        "detached": branch is None,
        "dirty_files": [
            record.decode("utf-8", errors="surrogateescape")
            for record in status.split(b"\0")
            if record
        ],
    }


def process_cwd(pid: int) -> Optional[pathlib.Path]:
    if platform.system() == "Linux":
        try:
            return pathlib.Path(os.readlink("/proc/" + str(pid) + "/cwd"))
        except (FileNotFoundError, PermissionError, OSError):
            return None
    if platform.system() == "Darwin" and shutil.which("lsof"):
        result = subprocess.run(
            ["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
        )
        for line in result.stdout.splitlines():
            if line.startswith("n"):
                return pathlib.Path(line[1:])
    return None


def path_is_within(path: pathlib.Path, root: pathlib.Path) -> bool:
    try:
        path.resolve().relative_to(root.resolve())
    except ValueError:
        return False
    return True


def process_snapshot() -> Dict[int, ProcessSample]:
    result = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,%cpu=,comm=,args="],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    samples: Dict[int, ProcessSample] = {}
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 4)
        if len(fields) != 5:
            continue
        pid_text, parent_pid_text, cpu_percent_text, command, arguments = fields
        try:
            sample = ProcessSample(
                pid=int(pid_text),
                parent_pid=int(parent_pid_text),
                cpu_percent=float(cpu_percent_text),
                command=command,
                arguments=arguments,
            )
        except ValueError:
            continue
        samples[sample.pid] = sample
    return samples


def process_executables(sample: ProcessSample) -> set[str]:
    argument_executable = (
        pathlib.Path(sample.arguments.split(None, 1)[0]).name
        if sample.arguments
        else ""
    )
    return {
        pathlib.Path(sample.command).name,
        argument_executable,
    }


def runner_descendants(
    samples: Sequence[Dict[int, ProcessSample]], runner_pid: int
) -> set[int]:
    descendants = {runner_pid}
    changed = True
    while changed:
        changed = False
        for snapshot in samples:
            for sample in snapshot.values():
                if sample.parent_pid in descendants and sample.pid not in descendants:
                    descendants.add(sample.pid)
                    changed = True
    return descendants


def competing_candidate_pids(
    first: Dict[int, ProcessSample],
    second: Dict[int, ProcessSample],
    *,
    runner_pid: int,
    parent_pid: int,
) -> List[int]:
    ignored = runner_descendants((first, second), runner_pid)
    ignored.add(parent_pid)
    candidates: List[int] = []
    for pid, sample in second.items():
        executables = process_executables(sample)
        if pid in ignored or executables & PROCESS_INVENTORY_EXECUTABLES:
            continue
        previous = first.get(pid)
        sustained_high_cpu = (
            previous is not None
            and previous.cpu_percent >= COMPETING_CPU_PERCENT
            and sample.cpu_percent >= COMPETING_CPU_PERCENT
        )
        if executables & REPOSITORY_PROCESS_EXECUTABLES or sustained_high_cpu:
            candidates.append(pid)
    return sorted(candidates)


def classify_competing_processes(
    first: Dict[int, ProcessSample],
    second: Dict[int, ProcessSample],
    *,
    cwd_by_pid: Dict[int, Optional[pathlib.Path]],
    runner_pid: int,
    parent_pid: int,
) -> List[Dict[str, object]]:
    competitors: List[Dict[str, object]] = []
    for pid in competing_candidate_pids(
        first,
        second,
        runner_pid=runner_pid,
        parent_pid=parent_pid,
    ):
        sample = second[pid]
        previous = first.get(pid)
        cwd = cwd_by_pid.get(pid)
        reasons: List[str] = []
        if process_executables(sample) & REPOSITORY_PROCESS_EXECUTABLES and (
            (cwd is not None and path_is_within(cwd, ROOT))
            or str(ROOT.resolve()) in sample.arguments
        ):
            reasons.append("Aura repository cargo/rustc/aura process")
        if (
            previous is not None
            and previous.cpu_percent >= COMPETING_CPU_PERCENT
            and sample.cpu_percent >= COMPETING_CPU_PERCENT
        ):
            reasons.append(f"sustained high CPU (>= {COMPETING_CPU_PERCENT:.1f}%)")
        if reasons:
            competitors.append(
                {
                    "pid": pid,
                    "parent_pid": sample.parent_pid,
                    "command": sample.command,
                    "arguments": sample.arguments,
                    "cwd": str(cwd) if cwd else None,
                    "cpu_percent_samples": [
                        previous.cpu_percent if previous else None,
                        sample.cpu_percent,
                    ],
                    "reasons": reasons,
                }
            )
    return competitors


def quiet_process_inventory() -> List[Dict[str, object]]:
    runner_pid = os.getpid()
    parent_pid = os.getppid()
    first = process_snapshot()
    time.sleep(QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS)
    second = process_snapshot()
    candidates = competing_candidate_pids(
        first,
        second,
        runner_pid=runner_pid,
        parent_pid=parent_pid,
    )
    return classify_competing_processes(
        first,
        second,
        cwd_by_pid={pid: process_cwd(pid) for pid in candidates},
        runner_pid=runner_pid,
        parent_pid=parent_pid,
    )


def qualify_inputs(options: Options) -> Dict[str, object]:
    identity_script = ROOT / "benchmarks/numeric_arrays/numpy_reference.py"
    process_helper = ROOT / "scripts/benchmark_process.py"
    identity_result = subprocess.run(
        [str(options.python), str(identity_script), "--identity"],
        cwd=ROOT,
        env={**os.environ, **SINGLE_THREAD_ENVIRONMENT},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if identity_result.returncode != 0:
        raise BenchmarkError(
            "NumPy qualification failed: " + identity_result.stderr.strip()
        )
    try:
        numpy_identity = json.loads(identity_result.stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkError("NumPy qualification did not emit JSON") from error
    if numpy_identity.get("float64_itemsize") != 8:
        raise BenchmarkError("NumPy float64 is not eight bytes")
    return {
        "aura": {
            "path": str(options.aura.resolve()),
            "sha256": sha256_file(options.aura.resolve()),
            "version": command_output([str(options.aura.resolve()), "--version"]),
        },
        "python": {
            "path": str(options.python.resolve()),
            "version": command_output([str(options.python.resolve()), "--version"]),
            "sha256": sha256_file(options.python.resolve()),
        },
        "numpy": numpy_identity,
        "runner": {
            "path": str(pathlib.Path(__file__).resolve()),
            "sha256": sha256_file(pathlib.Path(__file__).resolve()),
        },
        "numpy_reference": {
            "path": str(identity_script.resolve()),
            "sha256": sha256_file(identity_script),
        },
        "benchmark_process": {
            "path": str(process_helper.resolve()),
            "sha256": sha256_file(process_helper),
        },
    }


def build_aura_workloads(
    aura: pathlib.Path, output_directory: pathlib.Path
) -> tuple[Dict[str, pathlib.Path], List[Dict[str, object]]]:
    binaries: Dict[str, pathlib.Path] = {}
    records: List[Dict[str, object]] = []
    for workload in ("add", "sum"):
        source = ROOT / f"benchmarks/numeric_arrays/float64_{workload}.au"
        output = output_directory / f"aura_{workload}"
        command = [
            str(aura),
            "build",
            "--backend",
            "direct",
            "-o",
            str(output),
            str(source),
        ]
        result = benchmark_process.run_process_group(
            command,
            f"numeric-array {workload} build",
            timeout=BUILD_TIMEOUT_SECONDS,
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.returncode != 0:
            raise BenchmarkError(
                f"failed to build {source}: "
                + result.stderr.decode("utf-8", errors="replace")
            )
        if not output.is_file() or not os.access(output, os.X_OK):
            raise BenchmarkError("aura build did not create " + str(output))
        binaries[workload] = output
        records.append(
            {
                "workload": workload,
                "source": str(source),
                "source_sha256": sha256_file(source),
                "binary": str(output),
                "binary_sha256": sha256_file(output),
                "command": command,
                "stdout": result.stdout.decode("utf-8", errors="strict"),
                "stderr": result.stderr.decode("utf-8", errors="strict"),
                "returncode": result.returncode,
            }
        )
    return binaries, records


def lane_commands(
    binaries: Dict[str, pathlib.Path], python: pathlib.Path
) -> Dict[str, List[str]]:
    reference = ROOT / "benchmarks/numeric_arrays/numpy_reference.py"
    return {
        "aura_add": [str(binaries["add"])],
        "numpy_add": [str(python), str(reference), "--workload", "add"],
        "aura_sum": [str(binaries["sum"])],
        "numpy_sum": [str(python), str(reference), "--workload", "sum"],
        "rust_add": [str(binaries["rust_add"])],
        "rust_sum": [str(binaries["rust_sum"])],
    }


def validate_options(options: Options) -> None:
    if not options.label.strip():
        raise BenchmarkError("--label must not be empty")
    if options.pairs < 5 or options.pairs % 2 == 0:
        raise BenchmarkError("--pairs must be odd and at least 5")
    for label, executable in (("--aura", options.aura), ("--python", options.python)):
        resolved = executable.resolve()
        if not resolved.is_file() or not os.access(resolved, os.X_OK):
            raise BenchmarkError(label + " must name an executable file")
    if "debug" in options.aura.resolve().parts:
        raise BenchmarkError("refusing a debug Aura binary")
    for path in (options.raw_json.resolve(), options.summary_json.resolve()):
        try:
            path.relative_to((ROOT / "target").resolve())
        except ValueError:
            pass
        else:
            raise BenchmarkError("benchmark JSON must be stored outside target/")
    if options.raw_json.resolve() == options.summary_json.resolve():
        raise BenchmarkError("raw and summary JSON paths must differ")


def evidence_noncontractual_reasons(
    *,
    allow_competing_processes: bool,
    process_checks: Sequence[Sequence[Dict[str, object]]],
    host: Dict[str, object],
    repository: Dict[str, object],
) -> List[str]:
    reasons: List[str] = []
    if allow_competing_processes:
        reasons.append("the competing-process override was enabled")
    if any(process_checks):
        reasons.append("competing host CPU consumers were observed")
    if host.get("hardware_model") != "Mac14,9":
        reasons.append("host hardware model is not the contractual Mac14,9 baseline")
    if repository.get("dirty_files"):
        reasons.append("repository worktree was dirty")
    if repository.get("detached") is not True:
        reasons.append("repository HEAD was not detached")
    return reasons


def execute(options: Options) -> Dict[str, object]:
    validate_options(options)
    before_build = quiet_process_inventory()
    if before_build and not options.allow_competing_processes:
        raise BenchmarkError("competing host processes detected before build")
    inputs = qualify_inputs(options)
    repository = repository_record()
    host = hardware_record()

    with tempfile.TemporaryDirectory(prefix="aura-numeric-array-bench-") as directory:
        binaries, builds = build_aura_workloads(
            options.aura.resolve(), pathlib.Path(directory)
        )
        rust_binaries, rust_build = rust_baselines.build(pathlib.Path(directory) / "rust-target")
        binaries.update({"rust_" + name: rust_binaries["float64_" + name] for name in ("add", "sum")})
        commands = lane_commands(binaries, options.python.resolve())
        before_timing = quiet_process_inventory()
        if before_timing and not options.allow_competing_processes:
            raise BenchmarkError("competing host processes detected before timing")
        warmups = {lane: run_lane(lane, commands) for lane in LANES}
        pairs: List[Dict[str, object]] = []
        for repeat in range(options.pairs):
            order = pair_order(repeat)
            runs = {lane: run_lane(lane, commands) for lane in order}
            pairs.append({"repeat": repeat, "order": order, "runs": runs})
        after_timing = quiet_process_inventory()
        if after_timing and not options.allow_competing_processes:
            raise BenchmarkError("competing host processes detected after timing")

    if rust_baselines.source_identity() != rust_build["sources"]:
        raise BenchmarkError("Rust input identity changed during measurement")
    noncontractual_reasons = evidence_noncontractual_reasons(
        allow_competing_processes=options.allow_competing_processes,
        process_checks=(before_build, before_timing, after_timing),
        host=host,
        repository=repository,
    )
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "label": options.label,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "runner_command": [
            sys.executable,
            str(pathlib.Path(__file__).resolve()),
            *sys.argv[1:],
        ],
        "contractual": not noncontractual_reasons,
        "noncontractual_reasons": noncontractual_reasons,
        "host": host,
        "repository": repository,
        "inputs": inputs,
        "parameters": {
            "elements": ELEMENT_COUNT,
            "dtype": "float64",
            "pairs": options.pairs,
            "add_iterations": ADD_ITERATIONS,
            "sum_iterations": SUM_ITERATIONS,
            "single_thread_environment": dict(SINGLE_THREAD_ENVIRONMENT),
            "ready_timeout_seconds": READY_TIMEOUT_SECONDS,
            "completion_timeout_seconds": COMPLETION_TIMEOUT_SECONDS,
            "build_timeout_seconds": BUILD_TIMEOUT_SECONDS,
            "competing_cpu_percent": COMPETING_CPU_PERCENT,
            "quiet_process_sample_interval_seconds": (
                QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS
            ),
        },
        "quiet_process_checks": {
            "before_build": before_build,
            "before_timing": before_timing,
            "after_timing": after_timing,
        },
        "rust_build": rust_build,
        "builds": builds,
        "warmups": warmups,
        "pairs": pairs,
        "summaries": summarize_pairs(pairs),
        "performance_gate": None,
        "evidence_policy": (
            "measured release evidence only; no Aura-versus-NumPy threshold"
        ),
    }


def write_json_atomic(path: pathlib.Path, value: Dict[str, object]) -> None:
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=path.name + ".",
        suffix=".tmp",
        delete=False,
    ) as stream:
        temporary = pathlib.Path(stream.name)
        json.dump(value, stream, indent=2, sort_keys=True, allow_nan=False)
        stream.write("\n")
    os.replace(temporary, path)


def write_artifacts(
    raw_path: pathlib.Path,
    summary_path: pathlib.Path,
    raw_report: Dict[str, object],
    summary_report: Dict[str, object],
) -> None:
    write_json_atomic(raw_path, raw_report)
    raw_bytes = raw_path.resolve().read_bytes()
    linked_summary = dict(summary_report)
    linked_summary["raw_report"] = {
        "path": str(raw_path.resolve()),
        "sha256": sha256_bytes(raw_bytes),
    }
    write_json_atomic(summary_path, linked_summary)


def parse_options(argv: Optional[Sequence[str]] = None) -> Options:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", required=True)
    parser.add_argument("--aura", type=pathlib.Path, required=True)
    parser.add_argument(
        "--python",
        type=pathlib.Path,
        default=pathlib.Path(sys.executable),
    )
    parser.add_argument("--pairs", type=int, default=PAIR_COUNT)
    parser.add_argument("--raw-json", type=pathlib.Path, required=True)
    parser.add_argument("--summary-json", type=pathlib.Path, required=True)
    parser.add_argument("--allow-competing-processes", action="store_true")
    arguments = parser.parse_args(argv)
    return Options(
        label=arguments.label,
        aura=arguments.aura,
        python=arguments.python,
        pairs=arguments.pairs,
        raw_json=arguments.raw_json,
        summary_json=arguments.summary_json,
        allow_competing_processes=arguments.allow_competing_processes,
    )


def main(argv: Optional[Sequence[str]] = None) -> int:
    try:
        options = parse_options(argv)
        report = execute(options)
        summary = {
            "schema_version": REPORT_SCHEMA_VERSION,
            "label": report["label"],
            "generated_at": report["generated_at"],
            "contractual": report["contractual"],
            "noncontractual_reasons": report["noncontractual_reasons"],
            "host": report["host"],
            "repository": report["repository"],
            "inputs": report["inputs"],
            "parameters": report["parameters"],
            "benchmarks": report["summaries"],
            "performance_gate": None,
            "evidence_policy": report["evidence_policy"],
        }
        write_artifacts(
            options.raw_json.resolve(),
            options.summary_json.resolve(),
            report,
            summary,
        )
    except (BenchmarkError, OSError, subprocess.SubprocessError) as error:
        print("benchmark error: " + str(error), file=sys.stderr)
        return 2
    print("wrote " + str(options.raw_json.resolve()))
    print("wrote " + str(options.summary_json.resolve()))
    return 0 if report["contractual"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
