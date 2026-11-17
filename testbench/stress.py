#!/usr/bin/env python3
"""Nested-only padlink regression/stress runner; standard library only."""
import argparse
import datetime as dt
import fcntl
import json
import math
import os
import re
from pathlib import Path
import signal
import socket
import statistics
import struct
import subprocess as sp
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
MANUAL = [
    'Image sharpness, correct colours and no cropped/garbage strip (R1.15).',
    'Touch, Pencil, pressure/tilt and two-finger scroll on the physical iPad (R1.16).',
    'Physical USB unplug/replug and iPad lock/unlock (R2.3).',
    'Physical rotation and workspace restoration (R2.4/R2.7).',
    'Laptop suspend/resume, compositor re-attach and visible notifications (R2.6/R2.8/R2.9).',
    'True plug-to-first-displayed-frame timing; connect timing below ends at streaming status.',
]


def metric(values):
    values = [x for x in values if isinstance(x, (int, float)) and math.isfinite(x)]
    return ({'median': statistics.median(values), 'min': min(values), 'max': max(values),
             'samples': len(values)} if values else None)


def summarize(samples):
    """Summarize rolling windows, never pretend their percentiles are raw frames."""
    out = {}
    for field in ('fps', 'bitrate_mbps', 'cpu_percent', 'rss_mib'):
        out[field] = metric([s.get(field) for s in samples])
    for field in ('capture_to_encoded', 'encoded_to_sent', 'end_to_end'):
        selected = samples
        if field == 'end_to_end':
            # Only one observation per new receiver report; missing/stale is not zero.
            seen = set()
            selected = []
            for s in samples:
                seq = s.get('receiver_stats_sequence', 0)
                age = s.get('receiver_stats_age_ms')
                if seq and seq not in seen and age is not None and age <= 6000:
                    selected.append(s)
                    seen.add(seq)
        out[field] = {p: metric([(s.get(field) or {}).get(p) for s in selected])
                      for p in ('p50_ms', 'p95_ms')}
    drops = [s['frames_replaced'] for s in samples if 'frames_replaced' in s]
    out['frames_replaced_delta'] = max(0, drops[-1] - drops[0]) if drops else None
    out['streaming_samples'] = sum(s.get('state') == 'streaming' for s in samples)
    return out


class Suite:
    def __init__(self, args):
        self.args = args
        self.out = args.output.resolve()
        self.out.mkdir(parents=True, exist_ok=False)
        self.env = os.environ.copy()
        self.runtime = Path(self.env.get('XDG_RUNTIME_DIR', '/nonexistent'))
        self.env_file = self.runtime / 'padlink-testbench/env'
        self.daemon = self.load = None
        self.log_handles = []
        self.signature = None
        self.results = []
        self.profiles = []
        self.report = {'started': dt.datetime.now(dt.timezone.utc).isoformat(),
                       'arguments': vars(args) | {'output': str(self.out)},
                       'checks': self.results, 'profiles': self.profiles, 'manual': MANUAL,
                       'notes': ['Latency values are median/min/max of reported rolling p50/p95 windows, not whole-run frame percentiles.',
                                 'Receiver timing is receiver telemetry, not a camera measurement.',
                                 'CPU is daemon-only percent of ONE logical core (may exceed 100%). Workload/compositor cost is excluded.',
                                 'Nested rendering shares the GPU with the desktop. Idle FPS near zero is expected.',
                                 'Performance thresholds are applied to the observed stream size; default SPEC reference is 2360x1640 at 60 Hz.']}

    def record(self, name, status, detail):
        self.results.append({'name': name, 'status': status, 'detail': detail})
        print(f'{status}: {name}: {detail}', flush=True)
        self.save()

    def save(self):
        self.report['finished'] = dt.datetime.now(dt.timezone.utc).isoformat()
        self.report['overall'] = ('FAIL' if any(r['status'] == 'FAIL' for r in self.results)
                                  else 'INCOMPLETE' if any(r['status'] == 'SKIP' for r in self.results)
                                  else 'PASS (automated checks only)')
        (self.out / 'report.json').write_text(json.dumps(self.report, indent=2) + '\n')
        lines = ['# padlink automated test report', '', f"Result: **{self.report['overall']}**", '',
                 '| Check | Result | Detail |', '| --- | --- | --- |']
        for r in self.results:
            detail = str(r['detail']).replace('|', '/').replace('\n', ' ')
            lines.append(f"| {r['name']} | {r['status']} | {detail} |")
        for p in self.profiles:
            lines += ['', f"## {p['name']}", '',
                      f"Stream: {p['stream']}", '',
                      '| Metric | Median | Min | Max | Samples |',
                      '| --- | ---: | ---: | ---: | ---: |']
            for field, value in p['summary'].items():
                entries = value.items() if field in ('capture_to_encoded', 'encoded_to_sent', 'end_to_end') else [(None, value)]
                for percentile, result in entries:
                    label = f'{field} {percentile}' if percentile else field
                    if isinstance(result, dict):
                        lines.append(f"| {label} | {result['median']:.2f} | {result['min']:.2f} | {result['max']:.2f} | {result['samples']} |")
                    else:
                        lines.append(f"| {label} | {result if result is not None else 'unavailable'} | | | |")
        lines += ['', '## Interpretation', ''] + ['- ' + n for n in self.report['notes']]
        lines += ['', '## Manual checks still required', ''] + ['- ' + n for n in MANUAL]
        (self.out / 'report.md').write_text('\n'.join(lines) + '\n')

    def command(self, argv, timeout=30, env=None):
        return sp.run(argv, cwd=ROOT, env=env or self.env, text=True,
                      stdout=sp.PIPE, stderr=sp.STDOUT, timeout=timeout)

    def check_command(self, name, argv, timeout=600):
        result = self.command(argv, timeout)
        (self.out / f'{name}.log').write_text(result.stdout)
        self.record(name, 'PASS' if result.returncode == 0 else 'FAIL',
                    f'exit {result.returncode}; {name}.log')
        if result.returncode:
            raise RuntimeError(f'{name} failed; see log')

    def spawn(self, argv, log):
        handle = (self.out / log).open('w')
        self.log_handles.append(handle)
        return sp.Popen(argv, cwd=ROOT, env=self.env, stdout=handle,
                        stderr=sp.STDOUT, start_new_session=True)

    def guard(self):
        """Prove the exact compositor is the bench, not just a supplied signature."""
        values = {}
        for line in self.env_file.read_text().splitlines():
            if not line.startswith('export ') or '=' not in line:
                raise RuntimeError('invalid bench environment')
            k, v = line[7:].split('=', 1)
            values[k] = v
        sig = values.get('HYPRLAND_INSTANCE_SIGNATURE', '')
        allowed = {'HYPRLAND_INSTANCE_SIGNATURE', 'WAYLAND_DISPLAY', 'PADLINK_TESTBENCH'}
        if (set(values) != allowed or values.get('PADLINK_TESTBENCH') != '1'
                or not all(re.fullmatch(r'[A-Za-z0-9_.-]+', v) for v in values.values())):
            raise RuntimeError('invalid nested signature/marker')
        if self.signature and sig != self.signature:
            raise RuntimeError('bench instance changed during test; refusing to continue')
        lock = (self.runtime / 'hypr' / sig / 'hyprland.lock').read_text().splitlines()
        cmd = Path(f'/proc/{int(lock[0])}/cmdline').read_bytes().split(b'\0')
        expected = os.fsencode(ROOT / 'testbench/nested-hyprland.lua')
        if expected not in cmd or values.get('WAYLAND_DISPLAY') != lock[1]:
            raise RuntimeError('compositor is not running the repository nested config/socket')
        self.signature = sig
        self.env.update(values)

    def hypr(self, *args):
        self.guard()
        result = self.command(['hyprctl', '-i', self.signature, *args])
        if result.returncode:
            raise RuntimeError(result.stdout)
        return result.stdout

    def monitors(self):
        return json.loads(self.hypr('-j', 'monitors'))

    def outputs_gone(self):
        return not any(m['name'].startswith('padlink-') for m in self.monitors())

    def control(self, cmd='status'):
        if not self.daemon or self.daemon.poll() is not None:
            raise RuntimeError('test daemon is not running')
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(20)
            sock.connect(str(self.runtime / 'padlink/control'))
            pid, _, _ = struct.unpack('3i', sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if pid != self.daemon.pid:
                raise RuntimeError('control socket belongs to another daemon; refusing command')
            sock.sendall(json.dumps({'cmd': cmd}).encode() + b'\n')
            with sock.makefile('rb') as reader:
                reply = json.loads(reader.readline(1_000_000))
            if not reply['ok']:
                raise RuntimeError(reply.get('error', 'control refused'))
            return reply['status']

    def wait_stream(self):
        until = time.monotonic() + self.args.connect_timeout
        while time.monotonic() < until:
            status = self.control()
            if status['state'] == 'streaming' and status.get('fps', 0) > 0:
                return status
            time.sleep(.2)
        raise RuntimeError('No video within timeout: plug in/unlock iPad and foreground OpenDisplay')

    def connect(self):
        start = time.monotonic()
        self.control('connect')
        status = self.wait_stream()
        return status, time.monotonic() - start

    def disconnect(self):
        start = time.monotonic()
        status = self.control('disconnect')
        if status['state'] != 'paused' or not self.outputs_gone():
            raise RuntimeError('disconnect did not pause and remove nested padlink output')
        return time.monotonic() - start

    def start_daemon(self, config, log):
        self.guard()
        if self.command(['pgrep', '-x', 'padlinkd']).returncode == 0:
            raise RuntimeError('A padlinkd already runs. Stop it yourself before the bench test.')
        self.daemon = self.spawn(['testbench/padlinkd.sh', '--config', str(config)], log)
        until = time.monotonic() + 30
        while time.monotonic() < until:
            if self.daemon.poll() is not None:
                raise RuntimeError(f'daemon exited; see {log}')
            try:
                self.control()
                return
            except (OSError, RuntimeError):
                time.sleep(.2)
        raise RuntimeError('daemon control socket did not become ready')

    def stop_load(self):
        if self.load and self.load.poll() is None:
            os.killpg(self.load.pid, signal.SIGTERM)
            self.load.wait(timeout=10)
        self.load = None

    def stop_daemon(self):
        if self.daemon and self.daemon.poll() is None:
            self.daemon.terminate()
            # Never remove outputs or stop the compositor before capture has exited.
            self.daemon.wait(timeout=30)
        self.daemon = None

    def proc_stats(self):
        text = Path(f'/proc/{self.daemon.pid}/stat').read_text().rsplit(')', 1)[1].split()
        return ((int(text[11]) + int(text[12])) / os.sysconf('SC_CLK_TCK'),
                int(text[21]) * os.sysconf('SC_PAGE_SIZE') / 1024**2)

    def sample(self, name, seconds, moving):
        samples = []
        cpu, _ = self.proc_stats()
        before = time.monotonic()
        end = before + seconds
        next_sample = before + 1
        i = 0
        while time.monotonic() < end:
            if moving:
                monitors = self.monitors() if i == 0 else monitors
                m = next(m for m in monitors if m['name'].startswith('padlink-'))
                w, h = m['width'] / m['scale'], m['height'] / m['scale']
                x, y = m['x'] + int(w * (.1 + (i * 13 % 80) / 100)), m['y'] + int(h * (.1 + (i * 7 % 80) / 100))
                self.hypr('dispatch', f'hl.dsp.cursor.move({{ x = {x}, y = {y} }})')
                i += 1
            if time.monotonic() >= next_sample:
                now = time.monotonic()
                status = self.control()
                used, rss = self.proc_stats()
                status.update(elapsed_s=seconds - max(0, end - now),
                              cpu_percent=100 * (used - cpu) / (now - before), rss_mib=rss)
                samples.append(status)
                with (self.out / f'{name}.jsonl').open('a') as f:
                    f.write(json.dumps(status) + '\n')
                cpu, before = used, now
                next_sample = now + 1
            if self.load and self.load.poll() is not None:
                raise RuntimeError('heavy workload exited early')
            time.sleep(.015 if moving else .05)
        return samples

    def assess(self, name, samples, idle):
        summary = summarize(samples)
        self.profiles.append({'name': name, 'summary': summary,
                              'stream': {k: samples[-1].get(k) for k in ('encoder', 'capture', 'resolution', 'transport')} if samples else {}})
        self.record(name + ' continuity', 'PASS' if samples and all(s['state'] == 'streaming' for s in samples) else 'FAIL', f'{len(samples)} samples')
        cpu = summary['cpu_percent']
        self.record(name + ' CPU', 'PASS' if cpu and cpu['median'] < (2 if idle else 15) else 'FAIL',
                    f'median {cpu}; threshold {2 if idle else 15}% of one core; idle 2% is a bench heuristic')
        expected = 'zero-copy GPU buffers' if name.startswith('zero-copy-') else 'shared memory'
        self.record(name + ' capture path', 'PASS' if samples and all(expected in s.get('capture', '') for s in samples) else 'FAIL', expected)
        if idle:
            return
        for field, percentile, target in [('capture_to_encoded', 'p50_ms', 8), ('end_to_end', 'p50_ms', 20), ('end_to_end', 'p95_ms', 35)]:
            value = summary[field][percentile]
            self.record(f'{name} {field} {percentile}', 'SKIP' if value is None or value['samples'] < 2 else 'PASS' if value['median'] <= target else 'FAIL',
                        f'{value}; target <= {target} ms')
        fps = summary['fps']
        threshold = 55 if name.endswith('-heavy') else 1
        self.record(name + ' FPS', 'PASS' if fps and fps['median'] >= threshold else 'FAIL',
                    f'{fps}; threshold >={threshold} FPS; light damage rate is limited by IPC, heavy requests 60 Hz')

    def run(self):
        self.check_command('fmt', ['cargo', 'fmt', '--check'])
        self.check_command('clippy', ['cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings'])
        self.check_command('unit', ['cargo', 'test', '--workspace', '--', '--nocapture'])
        skipped = [line for line in (self.out / 'unit.log').read_text().splitlines() if 'SKIPPED:' in line]
        if skipped:
            self.record('hardware test coverage', 'SKIP', '; '.join(skipped))
        self.check_command('runner-tests', [sys.executable, '-m', 'unittest', 'discover', '-s', 'testbench', '-p', 'test_stress.py'])
        if self.args.checks_only:
            self.record('streaming and nested tests', 'SKIP', '--checks-only selected')
            return
        if self.command(['pgrep', '-x', 'padlinkd']).returncode == 0:
            raise RuntimeError('padlinkd already runs; stop it yourself before testing (disconnect alone leaves the daemon running)')
        if not self.env_file.exists():
            raise RuntimeError('Start testbench/nested.sh in another terminal, then run this script again')
        self.guard()
        if not self.outputs_gone():
            raise RuntimeError('bench has leftover padlink outputs; inspect/clean the nested bench first')
        self.check_command('nested', ['testbench/run-nested-tests.sh'])
        self.check_command('release', ['cargo', 'build', '--release', '-p', 'padlinkd', '-p', 'padlink'])
        self.report['environment'] = {'kernel': self.command(['uname', '-sr']).stdout.strip(),
                                      'hyprland': self.hypr('version'),
                                      'commit': self.command(['git', 'rev-parse', 'HEAD']).stdout.strip()}
        for profile in self.args.profiles.split(','):
            config = self.out / f'{profile}.toml'
            config.write_text(f'encoder = "{"x264" if profile == "x264" else "vaapi"}"\nzero-copy = {str(profile == "zero-copy").lower()}\nautoconnect = false\nnotifications = false\n')
            self.start_daemon(config, f'{profile}-daemon.log')
            status, seconds = self.connect()
            self.record(profile + ' connect', 'PASS', f'{seconds:.3f}s to streaming status with video; not first displayed frame')
            wanted = 'GPU' if profile == 'zero-copy' else 'shared'
            self.record(profile + ' capture', 'PASS' if wanted.lower() in status.get('capture', '').lower() else 'FAIL', status.get('capture', 'missing'))
            second = self.command(['testbench/padlinkd.sh', '--config', str(config)], timeout=30)
            self.record(profile + ' lock', 'PASS' if second.returncode and 'already running' in second.stdout else 'FAIL', second.stdout[-500:])
            for load in ('idle', 'light', 'heavy'):
                self.disconnect()
                status, _ = self.connect()
                if load == 'heavy':
                    output = json.dumps(status['output'])
                    self.hypr('dispatch', f'hl.dsp.focus({{ monitor = {output} }})')
                    self.load = self.spawn(['foot', '--fullscreen', '--app-id=padlink-stress', sys.executable,
                                            str(ROOT / 'testbench/stress-load.py')], f'{profile}-load.log')
                    time.sleep(2)
                    clients = json.loads(self.hypr('-j', 'clients'))
                    monitor = next(m for m in self.monitors() if m['name'] == status['output'])
                    if not any(c.get('class') == 'padlink-stress' and c.get('monitor') == monitor['id'] and c.get('fullscreen') for c in clients):
                        raise RuntimeError('heavy workload did not map fullscreen on the nested iPad output')
                self.sample(f'{profile}-{load}-warmup', self.args.warmup, load == 'light')
                samples = self.sample(f'{profile}-{load}', self.args.duration, load == 'light')
                self.assess(f'{profile}-{load}', samples, load == 'idle')
                self.stop_load()
            self.hypr('reload')
            recovered = self.sample(f'{profile}-reload', 4, True)
            good = recovered and all(s['state'] == 'streaming' and s.get('fps', 0) > 0 for s in recovered)
            self.record(profile + ' reload', 'PASS' if good else 'FAIL', 'nested config reload; verified video samples for 4 seconds; not a subsecond recovery measurement')
            for i in range(self.args.cycles):
                down = self.disconnect()
                _, up = self.connect()
                self.record(f'{profile} reconnect {i + 1}', 'PASS', f'disconnect {down:.3f}s; streaming {up:.3f}s')
            self.daemon.kill()
            self.daemon.wait(timeout=10)
            self.daemon = None
            self.start_daemon(config, f'{profile}-restart.log')
            self.record(profile + ' crash cleanup', 'PASS' if self.outputs_gone() else 'FAIL', 'startup after SIGKILL with autoconnect disabled')
            self.connect()
            self.disconnect()
            self.stop_daemon()
            self.record(profile + ' final cleanup', 'PASS' if self.outputs_gone() else 'FAIL', 'no nested padlink output remains')

    def cleanup(self):
        self.stop_load()
        self.stop_daemon()
        if self.signature:
            self.record('cleanup', 'PASS' if self.outputs_gone() else 'FAIL', 'nested outputs checked after daemon exit')
        for handle in self.log_handles:
            handle.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--checks-only', action='store_true', help='no compositor or iPad needed')
    parser.add_argument('--duration', type=int, default=30, help='measured seconds per load (>=10)')
    parser.add_argument('--warmup', type=int, default=10, help='discarded warmup seconds per load (>=6)')
    parser.add_argument('--cycles', type=int, default=5, help='reconnect cycles per profile')
    parser.add_argument('--connect-timeout', type=int, default=30)
    parser.add_argument('--profiles', default='zero-copy,shared-memory', help='comma-separated zero-copy,shared-memory,x264')
    parser.add_argument('--output', type=Path, default=ROOT / 'test-results' / dt.datetime.now().strftime('%Y%m%d-%H%M%S'))
    args = parser.parse_args()
    if args.duration < 10 or args.warmup < 6 or args.cycles < 1 or args.connect_timeout < 1:
        parser.error('duration >=10, warmup >=6, cycles >=1 and connect-timeout >=1 required')
    if any(p not in ('zero-copy', 'shared-memory', 'x264') for p in args.profiles.split(',')):
        parser.error('unknown profile')
    suite = Suite(args)
    lock = None
    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f'signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    try:
        lock = (ROOT / 'testbench/.stress.lock').open('w')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        suite.run()
    except (Exception, KeyboardInterrupt) as exc:
        suite.record('runner', 'FAIL', f'{type(exc).__name__}: {exc}')
    finally:
        try:
            suite.cleanup()
        except Exception as exc:
            suite.record('cleanup', 'FAIL', f'{exc}; processes/outputs may remain; inspect the nested bench')
        suite.save()
        if lock:
            lock.close()
    print(f'Report: {suite.out / "report.md"}', flush=True)
    return 1 if any(r['status'] == 'FAIL' for r in suite.results) else 2 if any(r['status'] == 'SKIP' for r in suite.results) else 0


if __name__ == '__main__':
    sys.exit(main())
