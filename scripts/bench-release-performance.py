#!/usr/bin/env python3
"""Measure Aura's Batch-6 release workloads against CPython reproducibly.

The runner builds every Aura workload as a standalone direct-native binary
before timing. Protocol workloads use exact READY/GO/DONE records; the V6
startup and loop controls retain their established whole-process contract.
Every measured child owns a fresh process group, and the raw report preserves
all paired observations and the identities needed to reproduce them.
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
from typing import Dict, List, NamedTuple, Optional, Sequence, Tuple

try:
    from scripts import benchmark_process, rust_baselines
except ImportError:
    import benchmark_process
    import rust_baselines


ROOT = pathlib.Path(__file__).resolve().parent.parent
PERFORMANCE_ROOT = ROOT / "benchmarks/release_performance"
REPORT_SCHEMA_VERSION = 2
PAIR_COUNT = 11
READY_TIMEOUT_SECONDS = 120.0
COMPLETION_TIMEOUT_SECONDS = 120.0
WHOLE_PROCESS_TIMEOUT_SECONDS = 120.0
BUILD_TIMEOUT_SECONDS = 1800.0
HELPER_TIMEOUT_SECONDS = 10.0
MAX_PROTOCOL_LINE_BYTES = 512
COMPETING_CPU_PERCENT = 50.0
QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS = 0.25
REPOSITORY_PROCESS_EXECUTABLES = {"cargo", "rustc", "aura"}
PROCESS_INVENTORY_EXECUTABLES = {"ps", "lsof"}
CONTROLLED_ENVIRONMENT = {"PYTHONHASHSEED": "0"}
CLEARED_ENVIRONMENT_VARIABLES = (
    "AURA_WORKERS",
    "AURA_BLOCKING_WORKERS",
    "AURA_BLOCKING_QUEUE_CAPACITY",
    "AURA_CACHE_DIR",
    "AURA_NATIVE_CACHE_DIR",
    "PYTHONASYNCIODEBUG",
)

ARRAY_EVIDENCE = {
    "measurement": "numeric Array add and sum against NumPy",
    "raw_path": "/private/tmp/aura-phase73-arrays-post-reboot-raw.json",
    "raw_sha256": "f51b979977519b5cbca9be4119a77bb3aff1d1a2874e1cdd4269f315bc1f9e7d",
    "summary_path": "/private/tmp/aura-phase73-arrays-post-reboot-summary.json",
    "summary_sha256": "f6fc84c1f0fadfb4b93a5f07befb5a33cbaa6926d54ef88a795e103106b410ab",
    "measured_commit": "0511adf61931953df096dc1b6721a543d856be25",
    "merge_policy": (
        "excluded from this runner; merge the separately qualified rows with "
        "their original commit and binary provenance"
    ),
}

METHODOLOGY_NOTES = {
    "fib30": (
        "Both lanes run the same naive recursive fib(30) algorithm with the "
        "measured interval bounded by GO and DONE."
    ),
    "tasks_10000": (
        "Both lanes create, join, verify, and clean up 10,000 short-lived tasks; "
        "the checksum is the sum of task indexes."
    ),
    "tcp_fanout": (
        "Both lanes pre-bind 20 ephemeral loopback listeners before READY, then "
        "fan out 20 clients to 20 owning server tasks with a 100 ms handler "
        "delay. Aura cannot transfer an accepted TcpStream to a handler task "
        "because host resources are non-Transfer (AU3008), so a one-listener "
        "shape would serialize the handlers and is deliberately not measured."
    ),
    "retrying_worker": (
        "Both lanes run 16 deterministic reset cycles totaling 112 loopback HTTP "
        "requests and 288 ms of scheduled retry delay."
    ),
}


class BenchmarkError(RuntimeError):
    """A release measurement cannot produce trustworthy evidence."""


class ProtocolWorkload(NamedTuple):
    name: str
    aura_source: pathlib.Path
    cpython_source: pathlib.Path
    ready: bytes
    go: bytes
    done: bytes


class WholeProcessLane(NamedTuple):
    name: str
    implementation: str
    source: pathlib.Path
    expected_stdout: bytes


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


PROTOCOL_WORKLOADS: Dict[str, ProtocolWorkload] = {
    "fib30": ProtocolWorkload(
        name="fib30",
        aura_source=PERFORMANCE_ROOT / "fib30.au",
        cpython_source=PERFORMANCE_ROOT / "fib30.py",
        ready=b"READY release-performance fib30 30\n",
        go=b"GO release-performance fib30\n",
        done=b"DONE release-performance fib30 832040\n",
    ),
    "tasks_10000": ProtocolWorkload(
        name="tasks_10000",
        aura_source=PERFORMANCE_ROOT / "tasks_10000.au",
        cpython_source=PERFORMANCE_ROOT / "tasks_10000.py",
        ready=b"READY release-performance tasks 10000\n",
        go=b"GO release-performance tasks\n",
        done=b"DONE release-performance tasks 10000 49995000\n",
    ),
    "tcp_fanout": ProtocolWorkload(
        name="tcp_fanout",
        aura_source=PERFORMANCE_ROOT / "tcp_fanout.au",
        cpython_source=PERFORMANCE_ROOT / "tcp_fanout.py",
        ready=b"READY release-performance tcp-fanout 20 100 4\n",
        go=b"GO release-performance tcp-fanout\n",
        done=b"DONE release-performance tcp-fanout 20 80\n",
    ),
    "retrying_worker": ProtocolWorkload(
        name="retrying_worker",
        aura_source=PERFORMANCE_ROOT / "retrying_worker.au",
        cpython_source=PERFORMANCE_ROOT / "retrying_worker.py",
        ready=b"READY release-performance retrying-worker 16 112 288\n",
        go=b"GO release-performance retrying-worker\n",
        done=b"DONE release-performance retrying-worker 112 18112\n",
    ),
}

V6_LANES: Dict[str, WholeProcessLane] = {
    "aura_startup": WholeProcessLane(
        name="aura_startup",
        implementation="aura",
        source=ROOT / "benchmarks/direct_integer_loops/startup.au",
        expected_stdout=b"",
    ),
    "aura_int32": WholeProcessLane(
        name="aura_int32",
        implementation="aura",
        source=ROOT / "benchmarks/direct_integer_loops/int32_loop.au",
        expected_stdout=b"10000000\n",
    ),
    "aura_int64": WholeProcessLane(
        name="aura_int64",
        implementation="aura",
        source=ROOT / "benchmarks/direct_integer_loops/int64_loop.au",
        expected_stdout=b"10000000\n",
    ),
    "cpython_startup": WholeProcessLane(
        name="cpython_startup",
        implementation="cpython",
        source=PERFORMANCE_ROOT / "python_startup.py",
        expected_stdout=b"",
    ),
    "cpython_int": WholeProcessLane(
        name="cpython_int",
        implementation="cpython",
        source=PERFORMANCE_ROOT / "python_int_loop.py",
        expected_stdout=b"10000000\n",
    ),
}

AURA_BUILD_SOURCES: Dict[str, pathlib.Path] = {
    **{
        name: contract.aura_source
        for name, contract in PROTOCOL_WORKLOADS.items()
    },
    **{
        name: lane.source
        for name, lane in V6_LANES.items()
        if lane.implementation == "aura"
    },
}


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
    if not 0.0 < percentile <= 1.0:
        raise BenchmarkError("percentile must be in (0, 1]")
    ordered = sorted(float(value) for value in values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def duration_summary(values: Sequence[float]) -> Dict[str, object]:
    samples = [float(value) for value in values]
    if not samples or any(not math.isfinite(value) or value <= 0 for value in samples):
        raise BenchmarkError("duration samples must be positive and finite")
    median = statistics.median(samples)
    return {
        "samples_s": samples,
        "median_s": median,
        "mad_s": statistics.median(abs(value - median) for value in samples),
        "p95_s": nearest_rank(samples, 0.95),
        "best_s": min(samples),
    }


def paired_duration_summary(
    aura: Sequence[float], cpython: Sequence[float], reference: str = "cpython"
) -> Dict[str, object]:
    if len(aura) != len(cpython) or not aura:
        raise BenchmarkError("paired duration samples must be nonempty and aligned")
    aura_values = [float(value) for value in aura]
    cpython_values = [float(value) for value in cpython]
    aura_summary = duration_summary(aura_values)
    cpython_summary = duration_summary(cpython_values)
    ratios = [
        aura_value / cpython_value
        for aura_value, cpython_value in zip(aura_values, cpython_values)
    ]
    return {
        "aura": aura_summary,
        reference: cpython_summary,
        "paired_ratios": ratios,
        "paired_median_ratio": statistics.median(ratios),
        "ratio_of_medians": (
            float(aura_summary["median_s"])
            / float(cpython_summary["median_s"])
        ),
    }


def rotate(values: Sequence[str], offset: int) -> List[str]:
    if not values:
        return []
    shift = offset % len(values)
    return list(values[shift:]) + list(values[:shift])


def v6_lane_order(repeat: int) -> List[str]:
    lanes = rotate(list(V6_LANES), repeat)
    if (repeat // len(V6_LANES)) % 2 == 1:
        lanes.reverse()
    return lanes


def measurement_plan(repeat: int) -> List[Tuple[str, List[str]]]:
    groups = [*PROTOCOL_WORKLOADS, "v6"]
    order = rotate(groups, repeat)
    plan: List[Tuple[str, List[str]]] = []
    for position, workload in enumerate(order):
        if workload == "v6":
            lanes = v6_lane_order(repeat)
        else:
            lanes = ["aura", "cpython", "rust"]
            if (repeat + position) % 2 == 1:
                lanes.reverse()
        plan.append((workload, lanes))
    return plan


def read_line_with_timeout(
    process: subprocess.Popen, timeout_seconds: float, label: str
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
                raise BenchmarkError(label + " ended before a complete line")
            line.extend(chunk)
            if len(line) > MAX_PROTOCOL_LINE_BYTES:
                raise BenchmarkError(label + " exceeded the protocol line bound")
            if chunk == b"\n":
                return bytes(line)
    finally:
        selector.close()


def controlled_environment() -> Dict[str, str]:
    environment = os.environ.copy()
    environment.update(CONTROLLED_ENVIRONMENT)
    for name in CLEARED_ENVIRONMENT_VARIABLES:
        environment.pop(name, None)
    return environment


def run_protocol_lane(
    contract: ProtocolWorkload,
    lane: str,
    command: List[str],
) -> Dict[str, object]:
    whole_started_ns = time.perf_counter_ns()
    with benchmark_process.owned_process_group(
        command,
        contract.name + " " + lane,
        cwd=ROOT,
        env=controlled_environment(),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ) as process:
        ready_line = read_line_with_timeout(
            process,
            READY_TIMEOUT_SECONDS,
            contract.name + " " + lane + " READY",
        )
        if ready_line != contract.ready:
            raise BenchmarkError(
                contract.name
                + " "
                + lane
                + " emitted unexpected READY line: "
                + repr(ready_line)
            )
        assert process.stdin is not None
        protocol_started_ns = time.perf_counter_ns()
        process.stdin.write(contract.go)
        process.stdin.flush()
        done_line = read_line_with_timeout(
            process,
            COMPLETION_TIMEOUT_SECONDS,
            contract.name + " " + lane + " DONE",
        )
        protocol_elapsed_s = (
            time.perf_counter_ns() - protocol_started_ns
        ) / 1_000_000_000.0
        if done_line != contract.done:
            raise BenchmarkError(
                contract.name
                + " "
                + lane
                + " emitted unexpected DONE line: "
                + repr(done_line)
            )
        process.stdin.close()
        process.stdin = None
        remaining_stdout, stderr = process.communicate(
            timeout=COMPLETION_TIMEOUT_SECONDS
        )
        whole_process_elapsed_s = (
            time.perf_counter_ns() - whole_started_ns
        ) / 1_000_000_000.0
        if process.returncode != 0:
            raise BenchmarkError(
                contract.name + " " + lane + " exited with status " + str(process.returncode)
            )
        if remaining_stdout:
            raise BenchmarkError(contract.name + " " + lane + " emitted trailing stdout")
        if stderr:
            raise BenchmarkError(
                contract.name
                + " "
                + lane
                + " emitted stderr: "
                + stderr.decode("utf-8", errors="replace")
            )
    return {
        "command": command,
        "environment": dict(CONTROLLED_ENVIRONMENT),
        "ready": ready_line.decode("ascii"),
        "go": contract.go.decode("ascii"),
        "done": done_line.decode("ascii"),
        "protocol_elapsed_s": protocol_elapsed_s,
        "whole_process_elapsed_s": whole_process_elapsed_s,
        "returncode": 0,
    }


def run_whole_process_lane(
    lane: str,
    command: List[str],
    expected_stdout: bytes,
) -> Dict[str, object]:
    started_ns = time.perf_counter_ns()
    result = benchmark_process.run_process_group(
        command,
        "release performance " + lane,
        timeout=WHOLE_PROCESS_TIMEOUT_SECONDS,
        cwd=ROOT,
        env=controlled_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    elapsed_s = (time.perf_counter_ns() - started_ns) / 1_000_000_000.0
    if result.returncode != 0:
        raise BenchmarkError(lane + " exited with status " + str(result.returncode))
    if result.stdout != expected_stdout:
        raise BenchmarkError(
            lane
            + " emitted unexpected stdout: expected "
            + repr(expected_stdout)
            + ", found "
            + repr(result.stdout)
        )
    if result.stderr:
        raise BenchmarkError(
            lane + " emitted stderr: " + result.stderr.decode("utf-8", errors="replace")
        )
    return {
        "command": command,
        "environment": dict(CONTROLLED_ENVIRONMENT),
        "stdout": result.stdout.decode("ascii"),
        "stderr": "",
        "whole_process_elapsed_s": elapsed_s,
        "returncode": 0,
    }


def summarize_protocol_workload(
    pairs: Sequence[Dict[str, object]], workload: str
) -> Dict[str, object]:
    result: Dict[str, object] = {}
    for label, metric in (
        ("protocol", "protocol_elapsed_s"),
        ("whole_process", "whole_process_elapsed_s"),
    ):
        aura = [
            float(pair["workloads"][workload]["runs"]["aura"][metric])  # type: ignore[index]
            for pair in pairs
        ]
        cpython = [
            float(pair["workloads"][workload]["runs"]["cpython"][metric])  # type: ignore[index]
            for pair in pairs
        ]
        result[label] = paired_duration_summary(aura, cpython)
        rust = [float(pair["workloads"][workload]["runs"]["rust"][metric]) for pair in pairs]
        result[label + "_vs_rust"] = paired_duration_summary(aura, rust, "rust")
    result["primary_measurement"] = "protocol"
    result["performance_gate"] = None
    return result


def v6_samples(
    pairs: Sequence[Dict[str, object]], lane: str
) -> List[float]:
    return [
        float(pair["workloads"]["v6"]["runs"][lane]["whole_process_elapsed_s"])  # type: ignore[index]
        for pair in pairs
    ]


def summarize_v6(pairs: Sequence[Dict[str, object]]) -> Dict[str, object]:
    samples = {lane: v6_samples(pairs, lane) for lane in V6_LANES}
    lane_summaries = {
        lane: duration_summary(values) for lane, values in samples.items()
    }
    adjusted = {
        "aura_int32": [
            loop - startup
            for loop, startup in zip(samples["aura_int32"], samples["aura_startup"])
        ],
        "aura_int64": [
            loop - startup
            for loop, startup in zip(samples["aura_int64"], samples["aura_startup"])
        ],
        "cpython_int": [
            loop - startup
            for loop, startup in zip(samples["cpython_int"], samples["cpython_startup"])
        ],
    }
    adjustment_validity: Dict[str, Dict[str, List[int]]] = {}
    adjusted_summaries: Dict[str, Dict[str, object]] = {}
    for lane, values in adjusted.items():
        valid = [index for index, value in enumerate(values) if value > 0.0]
        invalid = [index for index, value in enumerate(values) if value <= 0.0]
        if not valid:
            raise BenchmarkError("all paired V6 " + lane + " adjustments were nonpositive")
        adjusted_summaries[lane] = {
            **duration_summary([values[index] for index in valid]),
            "raw_samples_s": values,
            "valid_pair_repetitions": valid,
            "invalid_nonpositive_pair_repetitions": invalid,
        }
        adjustment_validity[lane] = {"valid": valid, "invalid": invalid}
    whole_int32 = paired_duration_summary(
        samples["aura_int32"], samples["cpython_int"]
    )
    whole_int64 = paired_duration_summary(
        samples["aura_int64"], samples["cpython_int"]
    )
    def adjusted_comparison(aura_lane: str) -> Dict[str, object]:
        valid = sorted(
            set(adjustment_validity[aura_lane]["valid"])
            & set(adjustment_validity["cpython_int"]["valid"])
        )
        if not valid:
            raise BenchmarkError(
                "no valid paired startup-adjusted V6 samples for " + aura_lane
            )
        return {
            **paired_duration_summary(
                [adjusted[aura_lane][index] for index in valid],
                [adjusted["cpython_int"][index] for index in valid],
            ),
            "valid_pair_repetitions": valid,
            "excluded_pair_repetitions": [
                index for index in range(len(pairs)) if index not in valid
            ],
        }

    adjusted_int32 = adjusted_comparison("aura_int32")
    adjusted_int64 = adjusted_comparison("aura_int64")
    return {
        "whole_process": lane_summaries,
        "startup_adjusted": adjusted_summaries,
        "comparisons": {
            "aura_int32_vs_cpython": whole_int32,
            "aura_int64_vs_cpython": whole_int64,
            "aura_int32_vs_cpython_startup_adjusted": adjusted_int32,
            "aura_int64_vs_cpython_startup_adjusted": adjusted_int64,
        },
        "primary_measurement": "whole_process",
        "performance_gate": None,
    }


def command_output(command: Sequence[str]) -> Optional[str]:
    try:
        result = benchmark_process.run_process_group(
            list(command),
            "release performance helper " + pathlib.Path(command[0]).name,
            timeout=HELPER_TIMEOUT_SECONDS,
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def python_identity(python: pathlib.Path) -> Dict[str, str]:
    code = (
        "import json,platform; "
        "print(json.dumps({'implementation': platform.python_implementation(), "
        "'version': platform.python_version()}, sort_keys=True))"
    )
    result = benchmark_process.run_process_group(
        [str(python), "-c", code],
        "release performance Python identity",
        timeout=HELPER_TIMEOUT_SECONDS,
        cwd=ROOT,
        env=controlled_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0 or result.stderr:
        raise BenchmarkError("Python identity probe failed: " + result.stderr.strip())
    try:
        identity = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkError("Python identity probe did not emit JSON") from error
    if identity.get("implementation") != "CPython":
        raise BenchmarkError("--python must name a CPython interpreter")
    version = identity.get("version")
    if not isinstance(version, str) or not version:
        raise BenchmarkError("CPython identity did not include a version")
    return {"implementation": "CPython", "version": version}


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


def require_measurement_host(host: Dict[str, object]) -> None:
    if host.get("hardware_model") != "Mac14,9":
        raise BenchmarkError("release evidence requires the Mac14,9 baseline host")
    if not host.get("boot_time"):
        raise BenchmarkError("release evidence requires recorded boot provenance")


def repository_record() -> Dict[str, object]:
    commit = command_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    branch = command_output(
        ["git", "-C", str(ROOT), "symbolic-ref", "--quiet", "--short", "HEAD"]
    )
    status_result = benchmark_process.run_process_group(
        ["git", "-C", str(ROOT), "status", "--porcelain=v1", "-z"],
        "release performance git status",
        timeout=HELPER_TIMEOUT_SECONDS,
        cwd=ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if status_result.returncode != 0:
        raise BenchmarkError(
            "git status failed: "
            + status_result.stderr.decode("utf-8", errors="replace")
        )
    status = status_result.stdout
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


def require_measurement_repository(repository: Dict[str, object]) -> None:
    if repository.get("dirty_files"):
        raise BenchmarkError("release measurement checkout is dirty")
    if repository.get("detached") is not True:
        raise BenchmarkError("release measurement checkout must be detached")
    if not repository.get("commit"):
        raise BenchmarkError("release measurement checkout has no commit identity")


def process_cwd(pid: int) -> Optional[pathlib.Path]:
    if platform.system() == "Linux":
        try:
            return pathlib.Path(os.readlink("/proc/" + str(pid) + "/cwd"))
        except (FileNotFoundError, PermissionError, OSError):
            return None
    if platform.system() == "Darwin" and shutil.which("lsof"):
        result = benchmark_process.run_process_group(
            ["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"],
            "release performance process cwd",
            timeout=HELPER_TIMEOUT_SECONDS,
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        if result.returncode != 0:
            return None
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
    result = benchmark_process.run_process_group(
        ["ps", "-axo", "pid=,ppid=,%cpu=,comm=,args="],
        "release performance process inventory",
        timeout=HELPER_TIMEOUT_SECONDS,
        cwd=ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        raise BenchmarkError("process inventory failed: " + result.stderr.strip())
    samples: Dict[int, ProcessSample] = {}
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 4)
        if len(fields) < 4:
            raise BenchmarkError("malformed process inventory row: " + repr(line))
        arguments = fields[4] if len(fields) == 5 else ""
        try:
            sample = ProcessSample(
                pid=int(fields[0]),
                parent_pid=int(fields[1]),
                cpu_percent=float(fields[2]),
                command=fields[3],
                arguments=arguments,
            )
        except ValueError as error:
            raise BenchmarkError(
                "malformed process inventory row: " + repr(line)
            ) from error
        samples[sample.pid] = sample
    return samples


def process_executables(sample: ProcessSample) -> set[str]:
    argument_executable = (
        pathlib.Path(sample.arguments.split(None, 1)[0]).name
        if sample.arguments
        else ""
    )
    return {pathlib.Path(sample.command).name, argument_executable}


def runner_descendants(
    snapshots: Sequence[Dict[int, ProcessSample]], runner_pid: int
) -> set[int]:
    descendants = {runner_pid}
    changed = True
    while changed:
        changed = False
        for snapshot in snapshots:
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
        first, second, runner_pid=runner_pid, parent_pid=parent_pid
    ):
        sample = second[pid]
        previous = first.get(pid)
        cwd = cwd_by_pid.get(pid)
        reasons: List[str] = []
        if process_executables(sample) & REPOSITORY_PROCESS_EXECUTABLES:
            if cwd is None:
                reasons.append("cargo/rustc/aura process with unknown cwd")
            elif path_is_within(cwd, ROOT) or str(ROOT.resolve()) in sample.arguments:
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
        first, second, runner_pid=runner_pid, parent_pid=parent_pid
    )
    return classify_competing_processes(
        first,
        second,
        cwd_by_pid={pid: process_cwd(pid) for pid in candidates},
        runner_pid=runner_pid,
        parent_pid=parent_pid,
    )


def require_quiet_process_check(
    inventory: Sequence[Dict[str, object]],
    *,
    phase: str,
    allow_competing_processes: bool,
) -> None:
    if inventory and not allow_competing_processes:
        raise BenchmarkError("competing host processes detected " + phase)


def manifest_record() -> Dict[str, object]:
    return {
        "protocol_workloads": {
            name: {
                "aura_source": str(contract.aura_source.resolve()),
                "cpython_source": str(contract.cpython_source.resolve()),
                "ready": contract.ready.decode("ascii"),
                "go": contract.go.decode("ascii"),
                "done": contract.done.decode("ascii"),
                "primary_measurement": "GO-to-DONE",
                "methodology": METHODOLOGY_NOTES[name],
            }
            for name, contract in PROTOCOL_WORKLOADS.items()
        },
        "v6": {
            "lanes": {
                name: {
                    "implementation": lane.implementation,
                    "source": str(lane.source.resolve()),
                    "expected_stdout": lane.expected_stdout.decode("ascii"),
                }
                for name, lane in V6_LANES.items()
            },
            "primary_measurement": "whole process",
            "startup_adjustment": "paired loop duration minus paired startup duration",
        },
    }


def source_paths() -> List[pathlib.Path]:
    paths = set(AURA_BUILD_SOURCES.values())
    paths.update(contract.cpython_source for contract in PROTOCOL_WORKLOADS.values())
    paths.update(
        lane.source for lane in V6_LANES.values() if lane.implementation == "cpython"
    )
    return sorted(paths)


def qualify_inputs(options: Options) -> Dict[str, object]:
    runner = pathlib.Path(__file__).resolve()
    process_helper = ROOT / "scripts/benchmark_process.py"
    return {
        "aura": {
            "path": str(options.aura.resolve()),
            "sha256": sha256_file(options.aura.resolve()),
            "version": command_output([str(options.aura.resolve()), "--version"]),
        },
        "python": {
            "path": str(options.python.resolve()),
            "sha256": sha256_file(options.python.resolve()),
            **python_identity(options.python.resolve()),
        },
        "rust_sources": rust_baselines.source_identity(),
        "rust_helper_sha256": sha256_file(ROOT / "scripts/rust_baselines.py"),
        "runner": {"path": str(runner), "sha256": sha256_file(runner)},
        "benchmark_process": {
            "path": str(process_helper.resolve()),
            "sha256": sha256_file(process_helper),
        },
        "sources": {
            path.name: {
                "path": str(path.resolve()),
                "sha256": sha256_file(path),
            }
            for path in source_paths()
        },
    }


def qualify_aura_binary(aura: pathlib.Path) -> Dict[str, object]:
    command = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "-p",
        "aura",
        "--target-dir",
        str((ROOT / "target").resolve()),
    ]
    result = benchmark_process.run_process_group(
        command,
        "release performance Aura qualification",
        timeout=BUILD_TIMEOUT_SECONDS,
        cwd=ROOT,
        env=controlled_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise BenchmarkError(
            "fresh locked release Aura build failed: "
            + result.stderr.decode("utf-8", errors="replace")
        )
    resolved = aura.resolve()
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise BenchmarkError("fresh locked release build did not create " + str(resolved))
    return {
        "command": command,
        "path": str(resolved),
        "sha256": sha256_file(resolved),
        "stdout": result.stdout.decode("utf-8", errors="strict"),
        "stderr": result.stderr.decode("utf-8", errors="strict"),
        "returncode": result.returncode,
        "fresh_locked_release_build": True,
    }


def build_aura_workloads(
    aura: pathlib.Path, output_directory: pathlib.Path
) -> Tuple[Dict[str, pathlib.Path], List[Dict[str, object]]]:
    binaries: Dict[str, pathlib.Path] = {}
    records: List[Dict[str, object]] = []
    for name, source in AURA_BUILD_SOURCES.items():
        if not source.is_file():
            raise BenchmarkError("missing Aura benchmark source " + str(source))
        output = output_directory / name
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
            "release performance " + name + " build",
            timeout=BUILD_TIMEOUT_SECONDS,
            cwd=ROOT,
            env=controlled_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.returncode != 0:
            raise BenchmarkError(
                "failed to build "
                + str(source)
                + ": "
                + result.stderr.decode("utf-8", errors="replace")
            )
        if not output.is_file() or not os.access(output, os.X_OK):
            raise BenchmarkError("aura build did not create " + str(output))
        binaries[name] = output
        records.append(
            {
                "name": name,
                "source": str(source.resolve()),
                "source_sha256": sha256_file(source),
                "binary": str(output.resolve()),
                "binary_sha256": sha256_file(output),
                "command": command,
                "stdout": result.stdout.decode("utf-8", errors="strict"),
                "stderr": result.stderr.decode("utf-8", errors="strict"),
                "returncode": result.returncode,
            }
        )
    return binaries, records


def protocol_commands(
    binaries: Dict[str, pathlib.Path], python: pathlib.Path
) -> Dict[str, Dict[str, List[str]]]:
    return {
        name: {
            "aura": [str(binaries[name])],
            "cpython": [str(python), str(contract.cpython_source)],
            "rust": [str(binaries["rust_" + name])],
        }
        for name, contract in PROTOCOL_WORKLOADS.items()
    }


def v6_commands(
    binaries: Dict[str, pathlib.Path], python: pathlib.Path
) -> Dict[str, List[str]]:
    commands: Dict[str, List[str]] = {}
    for name, lane in V6_LANES.items():
        commands[name] = (
            [str(binaries[name])]
            if lane.implementation == "aura"
            else [str(python), str(lane.source)]
        )
    return commands


def run_warmups(
    binaries: Dict[str, pathlib.Path], python: pathlib.Path
) -> Dict[str, object]:
    protocol = protocol_commands(binaries, python)
    v6 = v6_commands(binaries, python)
    result: Dict[str, object] = {}
    for name, contract in PROTOCOL_WORKLOADS.items():
        result[name] = {
            lane: run_protocol_lane(contract, lane, protocol[name][lane])
            for lane in ("aura", "cpython", "rust")
        }
    result["v6"] = {
        lane: run_whole_process_lane(
            lane, v6[lane], V6_LANES[lane].expected_stdout
        )
        for lane in V6_LANES
    }
    return result


def run_pair(
    repeat: int, binaries: Dict[str, pathlib.Path], python: pathlib.Path
) -> Dict[str, object]:
    protocol = protocol_commands(binaries, python)
    v6 = v6_commands(binaries, python)
    plan = measurement_plan(repeat)
    workloads: Dict[str, object] = {}
    for workload, order in plan:
        if workload == "v6":
            runs = {
                lane: run_whole_process_lane(
                    lane,
                    v6[lane],
                    V6_LANES[lane].expected_stdout,
                )
                for lane in order
            }
        else:
            contract = PROTOCOL_WORKLOADS[workload]
            runs = {
                lane: run_protocol_lane(
                    contract,
                    lane,
                    protocol[workload][lane],
                )
                for lane in order
            }
        workloads[workload] = {"order": order, "runs": runs}
    return {
        "repeat": repeat,
        "workload_order": [workload for workload, _ in plan],
        "workloads": workloads,
    }


def validate_options(options: Options) -> None:
    if not options.label.strip():
        raise BenchmarkError("--label must not be empty")
    if options.pairs != PAIR_COUNT:
        raise BenchmarkError("release evidence requires exactly 11 pairs")
    resolved_python = options.python.resolve()
    if not resolved_python.is_file() or not os.access(resolved_python, os.X_OK):
        raise BenchmarkError("--python must name an executable file")
    expected_aura = (ROOT / "target/release/aura").resolve()
    if options.aura.resolve() != expected_aura:
        raise BenchmarkError("--aura must name this checkout's target/release/aura")
    for path in source_paths():
        if not path.is_file():
            raise BenchmarkError("missing benchmark source " + str(path))
    for path in (options.raw_json.resolve(), options.summary_json.resolve()):
        try:
            path.relative_to((ROOT / "target").resolve())
        except ValueError:
            pass
        else:
            raise BenchmarkError("benchmark JSON must be stored outside target/")
    if options.raw_json.resolve() == options.summary_json.resolve():
        raise BenchmarkError("raw and summary JSON paths must differ")


def execute(options: Options) -> Dict[str, object]:
    validate_options(options)
    repository = repository_record()
    require_measurement_repository(repository)
    host = hardware_record()
    require_measurement_host(host)
    before_build = quiet_process_inventory()
    require_quiet_process_check(
        before_build,
        phase="before build",
        allow_competing_processes=options.allow_competing_processes,
    )
    aura_qualification = qualify_aura_binary(options.aura.resolve())
    inputs = qualify_inputs(options)

    with tempfile.TemporaryDirectory(
        prefix="aura-release-performance-"
    ) as directory:
        binaries, builds = build_aura_workloads(
            options.aura.resolve(), pathlib.Path(directory)
        )
        rust_binaries, rust_build = rust_baselines.build(pathlib.Path(directory) / "rust-target")
        binaries.update({"rust_" + name: binary for name, binary in rust_binaries.items()})
        before_timing = quiet_process_inventory()
        require_quiet_process_check(
            before_timing,
            phase="before timing",
            allow_competing_processes=options.allow_competing_processes,
        )
        warmups = run_warmups(binaries, options.python.resolve())
        pairs = [
            run_pair(repeat, binaries, options.python.resolve())
            for repeat in range(options.pairs)
        ]
        after_timing = quiet_process_inventory()
        require_quiet_process_check(
            after_timing,
            phase="after timing",
            allow_competing_processes=options.allow_competing_processes,
        )

    repository_after_timing = repository_record()
    require_measurement_repository(repository_after_timing)
    if repository_after_timing.get("commit") != repository.get("commit"):
        raise BenchmarkError("repository commit changed during measurement")
    inputs_after_timing = qualify_inputs(options)
    if inputs_after_timing != inputs:
        raise BenchmarkError("benchmark input identity changed during measurement")

    noncontractual_reasons: List[str] = []
    if options.allow_competing_processes:
        noncontractual_reasons.append("the competing-process override was enabled")
    if before_build or before_timing or after_timing:
        noncontractual_reasons.append("competing host CPU consumers were observed")
    summaries = {
        name: summarize_protocol_workload(pairs, name)
        for name in PROTOCOL_WORKLOADS
    }
    summaries["v6"] = summarize_v6(pairs)
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
        "post_timing_verification": {
            "repository": repository_after_timing,
            "input_identities_match": True,
            "source_hashes": inputs_after_timing["sources"],
        },
        "inputs": inputs,
        "aura_qualification": aura_qualification,
        "scope": {
            "measured_here": [*PROTOCOL_WORKLOADS, "v6"],
            "external_array_evidence": dict(ARRAY_EVIDENCE),
        },
        "manifest": manifest_record(),
        "parameters": {
            "pairs": options.pairs,
            "ready_timeout_seconds": READY_TIMEOUT_SECONDS,
            "completion_timeout_seconds": COMPLETION_TIMEOUT_SECONDS,
            "whole_process_timeout_seconds": WHOLE_PROCESS_TIMEOUT_SECONDS,
            "build_timeout_seconds": BUILD_TIMEOUT_SECONDS,
            "helper_timeout_seconds": HELPER_TIMEOUT_SECONDS,
            "max_protocol_line_bytes": MAX_PROTOCOL_LINE_BYTES,
            "competing_cpu_percent": COMPETING_CPU_PERCENT,
            "quiet_process_sample_interval_seconds": (
                QUIET_PROCESS_SAMPLE_INTERVAL_SECONDS
            ),
            "controlled_environment": dict(CONTROLLED_ENVIRONMENT),
            "cleared_environment_variables": list(CLEARED_ENVIRONMENT_VARIABLES),
            "allow_competing_processes": options.allow_competing_processes,
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
        "summaries": summaries,
        "performance_gate": None,
        "evidence_policy": (
            "measured release evidence only; no Aura-versus-CPython threshold"
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
        "--python", type=pathlib.Path, default=pathlib.Path(sys.executable)
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


def summary_report(report: Dict[str, object]) -> Dict[str, object]:
    qualification = report["aura_qualification"]
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "label": report["label"],
        "generated_at": report["generated_at"],
        "contractual": report["contractual"],
        "noncontractual_reasons": report["noncontractual_reasons"],
        "host": report["host"],
        "repository": report["repository"],
        "post_timing_verification": report["post_timing_verification"],
        "inputs": report["inputs"],
        "aura_qualification": {
            key: qualification[key]  # type: ignore[index]
            for key in (
                "command",
                "path",
                "sha256",
                "returncode",
                "fresh_locked_release_build",
            )
        },
        "scope": report["scope"],
        "manifest": report["manifest"],
        "parameters": report["parameters"],
        "benchmarks": report["summaries"],
        "performance_gate": None,
        "evidence_policy": report["evidence_policy"],
    }


def main(argv: Optional[Sequence[str]] = None) -> int:
    try:
        options = parse_options(argv)
        report = execute(options)
        write_artifacts(
            options.raw_json.resolve(),
            options.summary_json.resolve(),
            report,
            summary_report(report),
        )
    except (
        BenchmarkError,
        OSError,
        subprocess.SubprocessError,
        benchmark_process.ProcessGroupCleanupError,
    ) as error:
        print("benchmark error: " + str(error), file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("benchmark interrupted", file=sys.stderr)
        return 130
    print("wrote " + str(options.raw_json.resolve()))
    print("wrote " + str(options.summary_json.resolve()))
    return 0 if report["contractual"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
