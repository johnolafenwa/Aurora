import importlib.util
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest import mock
from scripts import benchmark_process

spec = importlib.util.spec_from_file_location('size_bench', Path(__file__).with_name('bench-binary-size.py'))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class BinarySizeTests(unittest.TestCase):
    def test_commands_finish_owned_process_cleanup_before_checkout_removal(self):
        result = subprocess.CompletedProcess(['probe'], 0, 'output', '')
        with mock.patch.object(benchmark_process, 'run_process_group', return_value=result) as owned, mock.patch.object(bench.subprocess, 'run', return_value=result):
            self.assertEqual(bench.run(['probe']).stdout, 'output')
        owned.assert_called_once()
        self.assertEqual(owned.call_args.args[0], ['probe'])

    def test_default_profile_overrides_every_tuned_setting_but_keeps_unwind(self):
        env = bench.build_environment(True)
        self.assertEqual(env['CARGO_PROFILE_RELEASE_LTO'], 'false')
        self.assertEqual(env['CARGO_PROFILE_RELEASE_CODEGEN_UNITS'], '16')
        self.assertEqual(env['CARGO_PROFILE_RELEASE_STRIP'], 'none')
        self.assertEqual(env['CARGO_PROFILE_RELEASE_DEBUG'], '0')
        self.assertEqual(env['CARGO_PROFILE_RELEASE_OPT_LEVEL'], '3')
        self.assertEqual(env['CARGO_PROFILE_RELEASE_PANIC'], 'unwind')

    def test_workspace_release_profile_retains_unwinding(self):
        text = (bench.ROOT / 'Cargo.toml').read_text().split('[profile.release]', 1)[1]
        for setting in ('opt-level = 3', 'lto = "fat"', 'codegen-units = 1', 'strip = "symbols"', 'debug = false'):
            self.assertIn(setting, text)
        self.assertNotIn('panic = "abort"', text)

    def test_size_record_hashes_actual_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'binary'
            path.write_bytes(b'abc')
            result = bench.artifact(path)
            self.assertEqual(result['bytes'], 3)
            self.assertEqual(result['sha256'], 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')

    def test_detached_checkout_is_removed_on_failure(self):
        commands = []
        def run(command, **kwargs):
            commands.append(command)
        with mock.patch.object(bench, 'run', side_effect=run):
            with self.assertRaisesRegex(RuntimeError, 'probe'):
                with bench.detached_checkout('abc'):
                    raise RuntimeError('probe')
        self.assertEqual(commands[0][:4], ['git', 'worktree', 'add', '--detach'])
        self.assertEqual(commands[-1][:4], ['git', 'worktree', 'remove', '--force'])


if __name__ == '__main__':
    unittest.main()
