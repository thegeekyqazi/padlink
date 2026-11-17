"""Regression tests for report honesty and the nested-session guard."""
import importlib.util
import os
from pathlib import Path
import tempfile
import struct
import argparse
import json
import unittest
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location('stress', Path(__file__).with_name('stress.py'))
stress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stress)


class Reporting(unittest.TestCase):
    def test_missing_latency_is_missing_not_a_fast_pass(self):
        result = stress.summarize([{'state': 'streaming', 'fps': 0}])
        self.assertIsNone(result['end_to_end']['p50_ms'])
        self.assertIsNone(result['capture_to_encoded']['p50_ms'])
        self.assertEqual(result['fps']['median'], 0)

    def test_receiver_reports_are_deduplicated_and_stale_values_excluded(self):
        def sample(seq, age, ms):
            return dict(receiver_stats_sequence=seq, receiver_stats_age_ms=age,
                        end_to_end={'p50_ms': ms, 'p95_ms': ms + 1})
        result = stress.summarize([sample(1, 0, 10), sample(1, 1000, 10),
                                   sample(2, 0, 30), sample(3, 10000, 900)])
        self.assertEqual(result['end_to_end']['p50_ms']['median'], 20)
        self.assertEqual(result['end_to_end']['p50_ms']['samples'], 2)

    def test_nonfinite_and_missing_values_are_excluded(self):
        self.assertEqual(stress.metric([None, float('nan'), 5, float('inf')])['samples'], 1)

    def test_drop_count_is_a_delta(self):
        self.assertEqual(stress.summarize([{'frames_replaced': 90},
                                          {'frames_replaced': 110}])['frames_replaced_delta'], 20)

    def test_report_persists_failures_and_manual_limits(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = stress.Suite(argparse.Namespace(output=Path(directory) / 'report'))
            suite.profiles.append({'name': 'sample', 'summary': stress.summarize([]), 'stream': {}})
            suite.record('missing measurement', 'SKIP', 'no telemetry')
            suite.record('latency', 'FAIL', 'above target')
            result = json.loads((suite.out / 'report.json').read_text())
            self.assertEqual(result['overall'], 'FAIL')
            markdown = (suite.out / 'report.md').read_text()
            self.assertIn('Manual checks still required', markdown)
            self.assertIn('unavailable', markdown)


class Safety(unittest.TestCase):
    def make_suite(self, directory):
        suite = stress.Suite.__new__(stress.Suite)
        suite.runtime = Path(directory)
        suite.env_file = suite.runtime / 'env'
        suite.env_file.write_text('export HYPRLAND_INSTANCE_SIGNATURE=test_1\n'
                                  'export WAYLAND_DISPLAY=wayland-2\nexport PADLINK_TESTBENCH=1\n')
        lock = suite.runtime / 'hypr/test_1/hyprland.lock'
        lock.parent.mkdir(parents=True)
        lock.write_text(f'{os.getpid()}\nwayland-2\n')
        suite.signature = None
        suite.env = {}
        return suite

    def test_control_refuses_another_daemons_socket_before_sending(self):
        suite = stress.Suite.__new__(stress.Suite)
        suite.runtime = Path('/tmp')
        suite.daemon = MagicMock(pid=123)
        suite.daemon.poll.return_value = None
        sock = MagicMock()
        sock.__enter__.return_value = sock
        sock.getsockopt.return_value = struct.pack('3i', 456, 1000, 1000)
        with patch.object(stress.socket, 'socket', return_value=sock):
            with self.assertRaisesRegex(RuntimeError, 'another daemon'):
                suite.control('disconnect')
        sock.sendall.assert_not_called()

    def test_marker_alone_cannot_authorize_live_session(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = self.make_suite(directory)
            with self.assertRaisesRegex(RuntimeError, 'not running the repository nested config'):
                suite.guard()

    def test_shell_expressions_in_bench_environment_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = self.make_suite(directory)
            suite.env_file.write_text(suite.env_file.read_text().replace('wayland-2', '$(bad-command)'))
            with self.assertRaisesRegex(RuntimeError, 'invalid nested'):
                suite.guard()

    def test_changed_instance_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = self.make_suite(directory)
            suite.signature = 'previous'
            with self.assertRaisesRegex(RuntimeError, 'changed'):
                suite.guard()

    def test_matching_bench_process_and_socket_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = self.make_suite(directory)
            cmd = b'Hyprland\0-c\0' + os.fsencode(stress.ROOT / 'testbench/nested-hyprland.lua') + b'\0'
            with patch.object(Path, 'read_bytes', return_value=cmd):
                suite.guard()
                self.assertEqual(suite.env['PADLINK_TESTBENCH'], '1')
                suite.env_file.write_text(suite.env_file.read_text().replace('wayland-2', 'wayland-1'))
                with self.assertRaisesRegex(RuntimeError, 'config/socket'):
                    suite.guard()


if __name__ == '__main__':
    unittest.main()
