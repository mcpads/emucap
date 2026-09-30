#!/usr/bin/env python3
"""Opt-in adapter-independent witness for read_memory_batch and debug.execution_speed.

The adapter profile supplies only launch arguments and memory ranges; every check uses the public
Control MCP surface and the live capabilities. Generated evidence stays in the private output.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import McpProcess  # noqa: E402


class Witness:
    def __init__(self, out, env):
        self.out = out
        self.rows = []
        self.process = McpProcess(ROOT / 'target/release/emucap-mcp', env)
        self.revision = None

    def call(self, name, arguments=None, error=False):
        started = time.perf_counter()
        response = self.process.request('tools/call', {'name': name, 'arguments': arguments or {}}, timeout=300)
        elapsed = time.perf_counter() - started
        self.rows.append({'tool': name, 'arguments': arguments, 'seconds': elapsed, 'response': response})
        (self.out / 'requests.json').write_text(json.dumps(self.rows, indent=2))
        failed = bool(response.get('error') or response.get('result', {}).get('isError'))
        assert failed == error, response
        return response.get('result', {}).get('structuredContent', response)

    def seconds(self):
        return self.rows[-1]['seconds']

    def speed(self, arguments, error=False):
        if self.revision is None:
            self.revision = self.call('debug', {'operation': 'describe'})['capability_revision']
        return self.call('debug', {'operation': 'execution_speed', 'known_capability_revision': self.revision,
                                   'arguments': arguments}, error=error)


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def warmup(w, status, frames):
    # Split at the live synchronous bound instead of assuming one adapter's cap.
    chunk = status.get('execution_limits', {}).get('frame', {}).get('max_count') or frames
    while frames > 0:
        step = w.call('step', {'unit': 'frames', 'count': min(chunk, frames)})
        assert step['status'] == 'completed', step
        frames -= min(chunk, frames)


def derive_ranges(capability):
    # Up to four windows of distinct memory types, plus one range crossing a window end.
    chosen, seen = [], set()
    for window in capability['windows']:
        if window['memory_type'] in seen or window['length'] < 32:
            continue
        seen.add(window['memory_type'])
        offset = min(0x100, window['length'] - 16)
        chosen.append({'memory_type': window['memory_type'], 'address': window['address'] + offset, 'length': 16})
        if len(chosen) == 4:
            break
    first = capability['windows'][0]
    rejected = [{'memory_type': first['memory_type'], 'address': first['address'] + first['length'] - 1, 'length': 2}]
    return chosen, rejected


def check_batch(w, ranges, rejected, groups):
    before = w.call('get_state', {'groups': groups})
    single = [w.call('read_memory', r)['hex'] for r in ranges]
    single_seconds = sum(row['seconds'] for row in w.rows[-len(ranges):])
    batch = w.call('read_memory_batch', {'ranges': ranges})
    batch_seconds = w.seconds()
    assert [r['hex'] for r in batch['reads']] == single, (batch, single)
    assert any(h.strip('0') for h in single), 'fixture ranges must contain nonzero bytes'
    assert w.call('read_memory_batch', {'ranges': ranges})['boundary'] == batch['boundary']
    assert w.call('get_state', {'groups': groups}) == before
    # Register observations must not mint a new guest stop epoch.
    assert w.call('read_memory_batch', {'ranges': ranges})['boundary'] == batch['boundary']
    for bad in rejected:
        w.call('read_memory_batch', {'ranges': [ranges[0], bad]}, error=True)
    w.call('read_memory_batch', {'ranges': []}, error=True)
    assert w.call('get_state', {'groups': groups}) == before
    return {'individual': single_seconds, 'batch': batch_seconds, 'boundary': batch['boundary']}


def check_pacing(w, ranges, frames, running_seconds):
    timings = []
    for policy in [{'mode': 'limited', 'percent': p} for p in (50, 100, 200, 400)] + [{'mode': 'unlimited'}]:
        epoch = w.call('read_memory_batch', {'ranges': ranges[:1]})['boundary']['stop_epoch']
        changed = w.speed(policy)
        assert changed['state'] == 'frozen', changed
        assert changed['execution_speed']['mode'] == policy['mode'], changed
        assert changed['execution_speed']['percent'] == policy.get('percent'), changed
        assert w.call('read_memory_batch', {'ranges': ranges[:1]})['boundary']['stop_epoch'] == epoch
        step = w.call('step', {'unit': 'frames', 'count': frames})
        assert step['status'] == 'completed', step
        timings.append({'policy': policy, 'frames': frames, 'seconds': w.seconds()})
    running = []
    for percent in (50, 200):
        w.speed({'mode': 'limited', 'percent': percent})
        start = w.call('status')['frame']
        w.call('resume')
        time.sleep(running_seconds)
        w.call('pause')
        running.append({'percent': percent, 'seconds': running_seconds,
                        'frames': w.call('status')['frame'] - start})
    w.call('resume')
    for percent in (100, 400):
        changed = w.speed({'mode': 'limited', 'percent': percent})
        assert changed['state'] == 'running', changed
        assert w.call('status')['execution_speed']['percent'] == percent
    w.call('read_memory_batch', {'ranges': ranges}, error=True)
    w.call('pause')
    final = w.speed({})
    assert final['percent'] == 400 and final['policy_revision'], final
    assert w.call('status')['execution_speed'] == final
    for bad in ({'mode': 'limited', 'percent': 0}, {'mode': 'limited', 'percent': 33.333},
                {'mode': 'unlimited', 'percent': 100}, {'percent': 50}):
        w.speed(bad, error=True)
    assert w.speed({}) == final
    return {'steps': timings, 'running': running}


def check_lifecycle(w, out, status):
    w.speed({'mode': 'limited', 'percent': 50})
    w.call('reset')
    assert w.speed({})['percent'] == 50
    if w.call('status')['state'] != 'frozen':
        w.call('pause')
    state = str(out / 'pacing.state')
    save = {'path': state}
    if 'instruction_snapshot_capture' in json.dumps(status):
        save['snapshot_key'] = 'pacing-lifecycle'
    # Some hosts save only at a proven main-CPU instruction halt.
    units = status.get('contracts', {}).get('constraints', {}).get('execution.step.units', ['instructions'])
    if 'instructions' in units:
        w.call('step', {'unit': 'instructions', 'count': 1})
    saved = w.process.request('tools/call', {'name': 'save_state', 'arguments': save}, timeout=300)
    content = saved.get('result', {}).get('structuredContent', {})
    if saved.get('result', {}).get('isError'):
        # Record a host that cannot save at this frozen boundary instead of claiming the check.
        return {'reset_preserved': True, 'load_preserved': None, 'save_error': content}
    w.speed({'mode': 'limited', 'percent': 400})
    w.call('load_state', {'path': state})
    assert w.speed({})['percent'] == 400
    return {'reset_preserved': True, 'load_preserved': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True,
                        help='JSON: launch_plan, launch overrides, env, warmup_frames, ranges, rejected')
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    profile = json.loads(args.profile.read_text())
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    # A profile may move the runtime home, e.g. where a Unix socket path must stay short.
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            # The profile owner reviewed these indirect members; keep them with the evidence.
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = w.call('status')
        assert status['contracts']['state'] == 'validated', status
        for field in ('memory_batch_capability', 'execution_speed_capability', 'execution_speed'):
            assert field in status, field
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        if status['state'] != 'frozen':
            w.call('pause')
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 1))
        result = {'launch_default': status['execution_speed']}
        if 'ranges' not in profile:
            profile['ranges'], profile['rejected'] = derive_ranges(status['memory_batch_capability'])
        result['batch'] = check_batch(w, profile['ranges'], profile.get('rejected', []),
                                      status.get('state_groups', ['cpu'])[:1])
        result['ranges'] = profile['ranges']
        assert result['batch']['boundary']['runtime_generation'] == launch['launch_id']
        result['pacing'] = check_pacing(w, profile['ranges'], profile.get('step_frames', 60),
                                        profile.get('running_seconds', 2.0))
        if profile.get('lifecycle', True):
            result['lifecycle'] = check_lifecycle(w, out, status)
        result['passed'] = True
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    finally:
        try:
            if launch:
                stopped = w.call('stop', {'launch_id': launch['launch_id']})
                (out / 'stop.json').write_text(json.dumps(stopped, indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
