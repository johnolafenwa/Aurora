"""Build and verify pinned Rust baselines without publishing timings."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import pathlib
import subprocess

try:
    from scripts import benchmark_process
except ImportError:
    import benchmark_process

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROJECT = ROOT / 'benchmarks/rust_baselines'
TOOLCHAIN = '1.95.0'
CONTRACTS = {
    'fib30': (b'READY release-performance fib30 30\n', b'GO release-performance fib30\n', b'DONE release-performance fib30 832040\n'),
    'tasks_10000': (b'READY release-performance tasks 10000\n', b'GO release-performance tasks\n', b'DONE release-performance tasks 10000 49995000\n'),
    'tcp_fanout': (b'READY release-performance tcp-fanout 20 100 4\n', b'GO release-performance tcp-fanout\n', b'DONE release-performance tcp-fanout 20 80\n'),
    'retrying_worker': (b'READY release-performance retrying-worker 16 112 288\n', b'GO release-performance retrying-worker\n', b'DONE release-performance retrying-worker 112 18112\n'),
    'int32_loop': (b'', b'', b'10000000\n'),
    'int64_loop': (b'', b'', b'10000000\n'),
    'startup': (b'', b'', b''),
    'float64_add': (b'READY numeric-arrays add 1000000 512\n', b'GO numeric-arrays add\n', b'DONE numeric-arrays add 512 2048.0\n'),
    'float64_sum': (b'READY numeric-arrays sum 1000000 1024\n', b'GO numeric-arrays sum\n', b'DONE numeric-arrays sum 1024 4096000000.0\n'),
}


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def source_identity():
    return {str(p.relative_to(PROJECT)): digest(p) for p in sorted([
        PROJECT / 'Cargo.toml', PROJECT / 'Cargo.lock', PROJECT / 'rust-toolchain.toml',
        *PROJECT.glob('src/**/*.rs'),
    ])}


def build(target):
    target = pathlib.Path(target).resolve()
    command = ['cargo', '+' + TOOLCHAIN, 'build', '--release', '--locked',
               '--manifest-path', str(PROJECT / 'Cargo.toml'), '--target-dir', str(target), '--bins']
    # Inherited profile overrides must not silently change the reference lane.
    env = {k: v for k, v in os.environ.items() if not k.startswith('CARGO_PROFILE_RELEASE_')
           and k not in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')}
    benchmark_process.run_process_group(
        command, 'Rust baseline build', env=env, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, timeout=1800,
    ).check_returncode()
    version = subprocess.run(['rustc', '+' + TOOLCHAIN, '-Vv'], check=True,
                             capture_output=True, text=True).stdout
    binaries = {name: target / 'release' / name for name in CONTRACTS}
    record = {'command': command, 'toolchain': version, 'sources': source_identity(),
              'binaries': {name: {'path': str(p), 'sha256': digest(p), 'bytes': p.stat().st_size}
                           for name, p in binaries.items()}}
    return binaries, record


def smoke(binaries):
    for name, (ready, go, done) in CONTRACTS.items():
        result = subprocess.run([str(binaries[name])], input=go, capture_output=True, timeout=120)
        if result.returncode or result.stderr or result.stdout != ready + done:
            raise RuntimeError(f'{name} protocol failed: {result!r}')
        print(f'PASS {name}: exact protocol and checksum')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=pathlib.Path, default=PROJECT / 'target')
    args = parser.parse_args()
    binaries, _ = build(args.target_dir)
    smoke(binaries)


if __name__ == '__main__':
    main()
