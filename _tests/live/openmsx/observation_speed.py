#!/usr/bin/env python3
"""Opt-in native batching and host pacing witness; all generated evidence stays private."""
import argparse
import json
import os
from pathlib import Path
import socket
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import McpProcess
from disk_state import fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--firmware', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    disk = out / 'boot.dsk'
    stops = fixture(disk)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(port),
               EMUCAP_EMU_HOME=str(out / 'home'), EMUCAP_OPENMSX_FIRMWARE=str(args.firmware.resolve()))
    process = McpProcess(ROOT / 'target/release/emucap-mcp', env)
    launch = None
    rows = []

    def call(name, arguments=None, error=False):
        started = time.perf_counter()
        response = process.request('tools/call', {'name': name, 'arguments': arguments or {}}, timeout=300)
        elapsed = time.perf_counter() - started
        rows.append({'tool': name, 'arguments': arguments, 'seconds': elapsed, 'response': response})
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        failed = bool(response.get('error') or response.get('result', {}).get('isError'))
        assert failed == error, response
        return response.get('result', {}).get('structuredContent', response)

    try:
        process.initialize()
        call('bootstrap')
        plan = call('launch_plan', {'content_path': str(disk), 'system': 'msx2'})
        assert plan['ready_to_launch'], plan
        launch = call('launch', {**plan['preferred_launcher']['args'], 'display': False, 'sound': False})
        status = call('status')
        assert status['contracts']['state'] == 'validated', status
        assert 'read_memory_batch' in status['methods'], status
        assert status['memory_batch_capability']['max_ranges'] == 64, status
        assert status['execution_speed_capability']['percent'] == {'min': 0.01, 'max': 10000.0, 'quantum': 0.01}
        assert status['execution_speed']['mode'] == 'unlimited' and status['execution_speed']['percent'] is None
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        bp_args = {'kind': 'exec', 'memory_type': 'memory', 'start': hex(stops[2]), 'end': hex(stops[2]), 'pause_on_hit': True}
        bp = call('set_breakpoint', bp_args)
        assert call('step', {'unit': 'frames', 'count': 2500})['status'] == 'interrupted'
        call('poll_events')
        call('clear_breakpoint', {'id': bp['id']})
        before = call('get_state', {'groups': ['cpu']})
        ranges = [{'memory_type': region, 'address': address, 'length': length}
                  for region, address, length in [('memory', 0xc01e, 16), ('memory', 0xd000, 512), ('ram', 0, 8), ('vram', 0, 8)]]
        single = [call('read_memory', r)['hex'] for r in ranges]
        assert single[0] != '00' * 16 and single[1] == 'a5' * 512
        single_seconds = sum(row['seconds'] for row in rows[-len(ranges):])
        batch = call('read_memory_batch', {'ranges': ranges})
        batch_seconds = rows[-1]['seconds']
        assert [r['hex'] for r in batch['reads']] == single
        assert batch['boundary']['runtime_generation'] == launch['launch_id']
        assert call('read_memory_batch', {'ranges': ranges})['boundary'] == batch['boundary']
        assert call('get_state', {'groups': ['cpu']}) == before
        # Windows are admitted before any adapter request.
        call('read_memory_batch', {'ranges': [{'memory_type': 'memory', 'address': 0xffff, 'length': 2}]}, error=True)
        call('read_memory_batch', {'ranges': [ranges[0], {'memory_type': 'ram', 'address': -1, 'length': 1}]}, error=True)
        call('read_memory_batch', {'ranges': []}, error=True)
        assert call('get_state', {'groups': ['cpu']}) == before
        drawer = call('debug', {'operation': 'describe'})
        revision = drawer['capability_revision']

        def speed(arguments, error=False):
            return call('debug', {'operation': 'execution_speed', 'known_capability_revision': revision,
                                  'arguments': arguments}, error=error)

        timings = []
        for policy in [{'mode': 'limited', 'percent': p} for p in (50, 100, 400)] + [{'mode': 'unlimited'}]:
            prior = call('get_state', {'groups': ['cpu']})
            changed = speed(policy)
            assert changed['execution_speed']['mode'] == policy['mode']
            if 'percent' in policy:
                assert changed['execution_speed']['percent'] == policy['percent']
            assert call('get_state', {'groups': ['cpu']}) == prior
            start_frame = call('status')['frame']
            step = call('step', {'unit': 'frames', 'count': 60})
            seconds = rows[-1]['seconds']
            assert step['status'] == 'completed', step
            after = call('status')
            assert after['frame'] - start_frame == 60 and after['state'] == 'frozen'
            timings.append({'policy': policy, 'frames': 60, 'seconds': seconds})
            bp = call('set_breakpoint', bp_args)
            assert call('step', {'unit': 'frames', 'count': 60})['status'] == 'interrupted'
            call('poll_events')
            call('clear_breakpoint', {'id': bp['id']})
        speed({'mode': 'limited', 'percent': 0}, error=True)
        speed({'mode': 'limited', 'percent': 33.333}, error=True)
        epoch = call('read_memory_batch', {'ranges': ranges[:1]})['boundary']['stop_epoch']
        slow = speed({'mode': 'limited', 'percent': 1})
        assert slow['state'] == 'frozen' and slow['execution_speed']['percent'] == 1, slow
        assert call('read_memory_batch', {'ranges': ranges[:1]})['boundary']['stop_epoch'] == epoch
        interrupted = call('step', {'unit': 'frames', 'count': 60})
        assert interrupted['status'] == 'interrupted' and interrupted['reason'] == 'host_deadline', interrupted
        assert 0 < interrupted['count'] < 60 and interrupted['state'] == 'frozen', interrupted
        deadline_seconds = rows[-1]['seconds']
        assert call('read_memory_batch', {'ranges': ranges[:1]})['boundary']['stop_epoch'] != epoch
        speed({'mode': 'limited', 'percent': 50})
        call('reset')
        assert speed({})['percent'] == 50
        state_path = str(out / 'pacing.oms')
        call('save_state', {'path': state_path})
        speed({'mode': 'limited', 'percent': 400})
        call('load_state', {'path': state_path})
        assert speed({})['percent'] == 400
        call('resume')
        for percent in (100, 50, 200):
            assert speed({'mode': 'limited', 'percent': percent})['state'] == 'running'
            assert call('status')['execution_speed']['percent'] == percent
        call('read_memory_batch', {'ranges': ranges}, error=True)
        call('pause')
        final = speed({})
        assert final['percent'] == 200 and final['policy_revision'], final
        assert call('status')['execution_speed'] == final
        (out / 'result.json').write_text(json.dumps({'passed': True, 'read_seconds': {'individual': single_seconds,
            'batch': batch_seconds}, 'step_timings': timings,
            'one_percent_deadline': {'frames': interrupted['count'], 'seconds': deadline_seconds}}, indent=2))
        print('Native batch boundary, pacing, deadline, lifecycle and breakpoint checks passed', flush=True)
    finally:
        try:
            if launch:
                call('stop', {'launch_id': launch['launch_id']})
        finally:
            process.close()


if __name__ == '__main__':
    main()
