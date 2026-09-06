#!/usr/bin/env python3
"""Paired direct Aura/Rust checked integer loops; whole-process protocol, schema 2."""
from __future__ import annotations
import argparse
import json
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time
try:
    from scripts import benchmark_process, rust_baselines
except ImportError:
    import benchmark_process
    import rust_baselines
ROOT = pathlib.Path(__file__).resolve().parent.parent
WIDTHS = ('int32', 'int64')
REPORT_SCHEMA_VERSION = 2


def resolve_aura():
    for candidate in (ROOT / 'target/release/aura', ROOT / 'target/debug/aura'):
        if candidate.is_file():
            return candidate
    sys.exit('no aura binary found; run cargo build -p aura first')


def measure(binary, repeats):
    samples = []
    for _ in range(repeats):
        started = time.perf_counter()
        result = benchmark_process.run_process_group([str(binary)], 'checked integer loop',
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
        elapsed = time.perf_counter() - started
        result.check_returncode()
        if result.stdout != b'10000000\n' or result.stderr:
            raise ValueError('integer loop checksum or stderr mismatch')
        samples.append(elapsed)
    return min(samples)


def summarize(samples):
    result = {}
    for width, lanes in samples.items():
        result[width] = {lane: {'samples_s': values, 'median_s': statistics.median(values), 'best_s': min(values)}
                         for lane, values in lanes.items()}
        ratios = [a/r for a, r in zip(lanes['aura'], lanes['rust'])]
        result[width]['paired_ratios'] = ratios
        result[width]['paired_median_ratio'] = statistics.median(ratios)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repeats', type=int, default=11)
    parser.add_argument('--aura', type=pathlib.Path)
    parser.add_argument('--raw-json', type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error('--repeats must be positive')
    aura = (args.aura or resolve_aura()).resolve()
    commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    if subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT):
        parser.error('measurement requires a clean checkout')
    samples = {width: {'aura': [], 'rust': []} for width in WIDTHS}
    with tempfile.TemporaryDirectory(prefix='aura-integer-bench-') as directory:
        work = pathlib.Path(directory)
        rust, rust_record = rust_baselines.build(work / 'rust-target')
        binaries, builds = {}, {}
        for width in WIDTHS:
            source = ROOT / f'benchmarks/direct_integer_loops/{width}_loop.au'
            binary = work / width
            command = [str(aura), 'build', '--backend', 'direct', '-o', str(binary), str(source)]
            benchmark_process.run_process_group(command, 'integer-loop build', stdout=subprocess.PIPE,
                                                stderr=subprocess.PIPE, timeout=1800).check_returncode()
            binaries[width] = {'aura': binary, 'rust': rust[width + '_loop']}
            builds[width] = {'command': command, 'source_sha256': rust_baselines.digest(source),
                             'binary_sha256': rust_baselines.digest(binary)}
            for path in binaries[width].values():
                measure(path, 1)  # excluded warmup
        for repetition in range(args.repeats):
            for width in WIDTHS if repetition % 2 == 0 else reversed(WIDTHS):
                for lane in ('aura', 'rust') if repetition % 2 == 0 else ('rust', 'aura'):
                    samples[width][lane].append(measure(binaries[width][lane], 1))
        if rust_baselines.source_identity() != rust_record['sources']:
            raise ValueError('Rust sources changed during measurement')
        if subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT) or commit != subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip():
            raise ValueError('repository changed during measurement')
    report = {'schema_version': REPORT_SCHEMA_VERSION, 'commit': commit,
              'aura_sha256': rust_baselines.digest(aura), 'rust_build': rust_record,
              'builds': builds, 'summaries': summarize(samples),
              'measurement': 'whole process, checked arithmetic, paired alternating lanes',
              'host': subprocess.check_output(['uname', '-a'], text=True).strip()}
    args.raw_json.parent.mkdir(parents=True, exist_ok=True)
    args.raw_json.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report['summaries'], indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
