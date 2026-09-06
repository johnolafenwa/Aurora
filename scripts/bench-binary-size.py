#!/usr/bin/env python3
"""Measure executable bytes from clean detached refs, removing build trees afterward."""
from __future__ import annotations
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

try:
    from scripts import benchmark_process
except ImportError:
    import benchmark_process

ROOT = Path(__file__).resolve().parent.parent
HELLO_SOURCE = 'print("Hello, world!")\n'
SUBJECTS = {'hello_world': 'examples/basics/hello_world.au',
            'reference_agent_standin': 'examples/agents/retrying_network_worker.au'}


def run(command, **kwargs):
    result = benchmark_process.run_process_group(
        command, 'binary-size command', stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, **kwargs,
    )
    result.check_returncode()
    return result


def build_environment(default_profile=False):
    env = {k: v for k, v in os.environ.items() if not k.startswith(('CARGO_PROFILE_RELEASE_', 'AURA_'))
           and k not in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_DIR',
                         'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')}
    env['CARGO_TERM_COLOR'] = 'never'
    env['CARGO_PROFILE_RELEASE_PANIC'] = 'unwind'
    if default_profile:
        env.update({'CARGO_PROFILE_RELEASE_OPT_LEVEL': '3', 'CARGO_PROFILE_RELEASE_LTO': 'false',
                    'CARGO_PROFILE_RELEASE_CODEGEN_UNITS': '16', 'CARGO_PROFILE_RELEASE_STRIP': 'none',
                    'CARGO_PROFILE_RELEASE_DEBUG': '0'})
    return env


def artifact(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return {'bytes': path.stat().st_size, 'sha256': digest.hexdigest()}


@contextmanager
def detached_checkout(ref):
    with tempfile.TemporaryDirectory(prefix='aura-size-') as directory:
        path = Path(directory) / 'checkout'
        run(['git', 'worktree', 'add', '--detach', str(path), ref], cwd=ROOT)
        try:
            yield path
        finally:
            run(['git', 'worktree', 'remove', '--force', str(path)], cwd=ROOT)


def measure(ref, default_profile=False):
    with detached_checkout(ref) as checkout:
        commit = run(['git', 'rev-parse', 'HEAD'], cwd=checkout).stdout.strip()
        if run(['git', 'status', '--porcelain'], cwd=checkout).stdout:
            raise RuntimeError('size measurement requires a clean checkout')
        if shutil.disk_usage(checkout).free < 25 * 1024**3:
            raise RuntimeError('less than 25 GiB free before size build')
        env = build_environment(default_profile)
        target = checkout / ('target-default' if default_profile else 'target-measured')
        env['CARGO_TARGET_DIR'] = str(target)
        build = ['cargo', '+1.95.0', 'build', '--release', '--locked', '-p', 'aura']
        print(f'Building {commit} ({"default" if default_profile else "release"} profile)', flush=True)
        run(build, cwd=checkout, env=env)
        link_command = ['cargo', '+1.95.0', 'rustc', '--release', '--locked', '-p', 'aura-compiler',
                        '--lib', '--', '--print', 'native-static-libs']
        link_output = run(link_command, cwd=checkout, env=env)
        lines = (link_output.stdout + link_output.stderr).splitlines()
        args = next(line.split('native-static-libs:', 1)[1].split() for line in reversed(lines) if 'native-static-libs:' in line)
        package = target / 'installed'
        (package / 'bin').mkdir(parents=True)
        runtime = package / 'lib/aura'
        runtime.mkdir(parents=True)
        compiler = package / 'bin/aura'
        shutil.copy2(target / 'release/aura', compiler)
        shutil.copy2(target / 'release/libaura_compiler.a', runtime / 'libaura_compiler.a')
        (runtime / 'native-link-args.json').write_text(json.dumps(args))
        records = {'aura': artifact(compiler)}
        smoke_env = dict(env, CARGO=str(target / 'missing-cargo'))
        run([str(compiler), '--version'], cwd=target, env=smoke_env)
        verification = {}
        sources = {}
        commands = []
        for name, relative in SUBJECTS.items():
            source = checkout / relative
            # The before ref predates the maintained hello fixture. Use identical
            # source bytes outside its source tree and disclose that input below.
            if name == 'hello_world' and not source.exists():
                source = target / 'hello_world.au'
                source.write_text(HELLO_SOURCE)
            if name == 'hello_world' and source.read_text() != HELLO_SOURCE:
                raise RuntimeError('hello source changed; before/after must be equivalent')
            binary = target / name
            command = [str(compiler), 'build', '--backend', 'direct', '-o', str(binary), str(source)]
            run(command, cwd=target, env=smoke_env)
            execution = run([str(binary)], cwd=target, env=smoke_env, timeout=120)
            if name == 'hello_world' and execution.stdout != 'Hello, world!\n':
                raise RuntimeError('hello executable smoke failed')
            if name == 'reference_agent_standin' and not execution.stdout.endswith('requests 7\n'):
                raise RuntimeError('reference-agent stand-in smoke failed')
            if execution.stderr:
                raise RuntimeError('unexpected standalone stderr')
            verification[name] = {'returncode': execution.returncode, 'stdout': execution.stdout,
                                  'cargo_unavailable': True}
            commands.append(command)
            records[name] = artifact(binary)
            sources[name] = {'path': relative, **artifact(source)}
        return {'ref': ref, 'commit': commit, 'profile': 'cargo-default' if default_profile else 'release',
                'profile_overrides': {k: v for k, v in env.items() if k.startswith('CARGO_PROFILE_RELEASE_')},
                'build_command': build, 'link_query_command': link_command, 'program_builds': commands,
                'subjects': records, 'sources': sources, 'standalone_verification': verification,
                'toolchain': run(['rustc', '+1.95.0', '-Vv']).stdout,
                'linker': run(['cc', '--version']).stdout,
                'hello_before_input_policy': 'identical single-print bytes staged outside the before source tree',
                'reference_agent': 'retrying_network_worker is the stand-in until pre-Batch-1 item 6'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before-ref', default='v0.3.3-preview')
    parser.add_argument('--after-ref', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    report = {'schema_version': 1, 'generated_at': datetime.now(timezone.utc).isoformat(),
              'host': platform.platform(), 'machine': platform.machine(),
              'runner_sha256': artifact(Path(__file__))['sha256'], 'measurements': []}
    report['process_helper_sha256'] = artifact(ROOT / 'scripts/benchmark_process.py')['sha256']
    for ref, default in [(args.before_ref, False), (args.after_ref, True), (args.after_ref, False)]:
        report['measurements'].append(measure(ref, default))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
