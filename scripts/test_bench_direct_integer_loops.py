import importlib.util
from pathlib import Path
import unittest
from unittest import mock
import subprocess

spec = importlib.util.spec_from_file_location('integer_bench', Path(__file__).with_name('bench-direct-integer-loops.py'))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class IntegerBenchmarkTests(unittest.TestCase):
    def test_measure_checks_the_observable_result(self):
        with mock.patch.object(bench.benchmark_process, 'run_process_group',
                               return_value=subprocess.CompletedProcess([], 0, b'wrong\n', b'')):
            with self.assertRaisesRegex(ValueError, 'checksum'):
                bench.measure(Path('/binary'), 1)

    def test_summary_preserves_paired_rust_lanes(self):
        result = bench.summarize({'int32': {'aura': [2, 4], 'rust': [1, 2]},
                                  'int64': {'aura': [3, 6], 'rust': [1, 2]}})
        self.assertEqual(result['int32']['paired_median_ratio'], 2)
        self.assertEqual(result['int64']['rust']['samples_s'], [1, 2])


if __name__ == '__main__':
    unittest.main()
