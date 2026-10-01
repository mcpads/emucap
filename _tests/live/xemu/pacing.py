#!/usr/bin/env python3
"""Opt-in Xbox pacing lifecycle witness using caller-owned machine inputs and XISO."""
import argparse
import json
import os
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup, check_batch, check_pacing


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--warmup-frames', type=int, default=1800)
    parser.add_argument('--restore-count', type=int, default=10)
    parser.add_argument('--display', action=argparse.BooleanOptionalAction, default=False)
    parser.add_argument('--sound', action=argparse.BooleanOptionalAction, default=False)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home')))
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system': 'xbox', 'content_path': str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], 'start_frozen': True,
                                  'display': args.display, 'sound': args.sound})
        status = w.call('status')
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        assert launch['display'] == args.display and launch['sound'] == args.sound, launch
        assert status['execution_speed']['percent'] == 100
        w.speed({'mode': 'unlimited'})
        warmup(w, status, args.warmup_frames)
        ranges = [{'memory_type': 'main', 'address': 0x10000, 'length': 32},
                  {'memory_type': 'main', 'address': 0x10008, 'length': 16}]
        rejected = [{'memory_type': 'main', 'address': 0x3ffffff, 'length': 2}]
        result = {'batch': check_batch(w, ranges, rejected, ['cpu'])}
        result['pacing'] = check_pacing(w, ranges, 30, .2)
        w.speed({'mode': 'limited', 'percent': 1})
        w.call('resume')
        time.sleep(.2)
        assert w.speed({})['percent'] == 1
        started = time.monotonic()
        w.call('pause')
        result['slow_pause_seconds'] = time.monotonic() - started
        assert result['slow_pause_seconds'] < 2, 'slow policy blocked control'
        w.speed({'mode': 'unlimited'})
        w.call('screenshot', {'save_path': str(out / 'before-input.png')})
        w.call('tap', {'buttons': ['start'], 'press_frames': 6, 'after_frames': 120})
        w.call('screenshot', {'save_path': str(out / 'after-input.png')})
        checkpoint = str(out / 'checkpoint.json')
        w.call('save_state', {'path': checkpoint})
        result['restores'] = []
        for iteration in range(args.restore_count):
            w.speed({'mode': 'limited', 'percent': 200})
            w.call('load_state', {'path': checkpoint})
            assert w.speed({})['percent'] == 200
            step = w.call('step', {'unit': 'frames', 'count': 30})
            assert step['status'] == 'completed', step
            w.call('screenshot', {'save_path': str(out / f'restored-{iteration:02}.png')})
            result['restores'].append(step)
        w.call('reset')
        assert w.speed({})['percent'] == 200
        result['passed'] = True
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
