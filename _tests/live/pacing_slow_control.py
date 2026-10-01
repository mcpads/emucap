#!/usr/bin/env python3
"""Opt-in witness that control stays responsive at a very slow pacing target.

While running at the adapter's minimum percent, status and pause must answer within the
advertised control-service bound plus request overhead, and a frame step longer than the host
budget must stop at a reached frame with reason host_deadline instead of timing out.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--deadline-frames', type=int, default=0,
                        help='frame count whose paced duration exceeds the host budget; 0 skips')
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
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            # The profile owner reviewed these indirect members; keep them with the evidence.
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        launch = w.call('launch', {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = w.call('status')
        if status['state'] != 'frozen':
            w.call('pause')
        capability = status['execution_speed_capability']
        slowest = capability['percent'].get('min') or capability['percent']['values'][0]
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 1))
        w.speed({'mode': 'limited', 'percent': slowest})
        w.call('resume')
        time.sleep(0.5)
        latency = {}
        before = w.call('status')
        latency['status'] = w.seconds()
        # Polling must not advance a slow guest: ten polls span far less than one target frame.
        # A frame clock that counts several frames per vertical blank (rendered frames) allows one
        # blank's worth through `polled_frame_allowance`.
        polled = []
        poll_seconds = []
        for _ in range(10):
            time.sleep(0.1)
            polled.append(w.call('status')['frame'])
            poll_seconds.append(w.seconds())
        allowance = profile.get('polled_frame_allowance', 1)
        assert polled[-1] - before['frame'] <= allowance, (before['frame'], polled, allowance)
        w.call('pause')
        latency['pause'] = w.seconds()
        latency['slowest_status'] = max([latency['status'], *poll_seconds])
        after = w.call('status')
        assert after['state'] == 'frozen', after
        changed = w.speed({'mode': 'limited', 'percent': 100})
        latency['speed_change'] = w.seconds()
        assert changed['state'] == 'frozen', changed
        bound = capability['control_service_ms'] / 1000 + 0.5
        for name, seconds in latency.items():
            assert seconds < bound, (name, seconds, bound)
        result = {'slowest_percent': slowest, 'latency': latency, 'polled_frames': polled,
                  'frames_while_running': after['frame'] - before['frame'],
                  'status_poll_seconds': poll_seconds}
        if args.deadline_frames:
            w.speed({'mode': 'limited', 'percent': profile.get('deadline_percent', 1)})
            step = w.call('step', {'unit': 'frames', 'count': args.deadline_frames})
            result['deadline_step'] = {'response': step, 'seconds': w.seconds()}
            assert step['status'] == 'interrupted' and step['reason'] == 'host_deadline', step
            assert w.call('status')['state'] == 'frozen'
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
