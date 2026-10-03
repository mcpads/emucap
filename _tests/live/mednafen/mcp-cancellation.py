#!/usr/bin/env python3
"""Public MCP cancellation or controller loss during a managed native input hold."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observation_speed import ROOT, Witness, free_port
from input_pacing import Session, choose_buttons
from support import terminate_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--controller-loss', action='store_true')
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home'))
    w = Witness(out, env); session = None; timer = None; pid = None; fingerprint = None

    def owned_exit():
        if pid is None:
            return
        actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
        if actual.returncode == 0:
            assert actual.stdout == fingerprint
            terminate_owned(pid)
        absent = subprocess.run(['ps', '-p', str(pid)], stdout=subprocess.DEVNULL).returncode != 0
        (out / 'process-exit.json').write_text(json.dumps(dict(pid=pid, absent=absent)))
        assert absent

    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        session.start(); status = w.call('status')
        pid = status['runtime_instance']['emulator_pid']
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        (out / 'identity.json').write_text(json.dumps(dict(launch=session.launch, status=status), indent=2))
        capability = status['temporal_cancellation_capability']
        assert set(capability['methods']) == {'step', 'step_instructions'}
        assert capability['control_service_ms'] <= 50 and capability['stop_host_ms'] == 5000
        slowest = status['execution_speed_capability']['percent']['min']
        w.speed(dict(mode='limited', percent=slowest))
        proc = w.process; request_id = proc.next_id; proc.next_id += 1
        def send(value):
            proc.process.stdin.write(json.dumps(value) + '\n'); proc.process.stdin.flush()
        send(dict(jsonrpc='2.0', id=request_id, method='tools/call', params=dict(name='tap',
                  arguments=dict(buttons=choose_buttons(profile, status), press_frames=120, after_frames=120))))
        records = list((out / 'home').glob('sessions/*/generations/*/link.json'))
        assert len(records) == 1
        deadline = time.monotonic() + 5
        while True:
            active = json.loads(records[0].read_text())
            if active.get('temporal_operation') and active.get('last_method') == 'set_input' and not active.get('last_failure'):
                break
            assert time.monotonic() < deadline, active
            time.sleep(.01)
        time.sleep(.25)
        active = json.loads(records[0].read_text())
        assert active.get('temporal_operation') and not active.get('last_failure'), active
        (out / 'active.json').write_text(json.dumps(active, indent=2))
        timer = threading.Timer(15, owned_exit); timer.daemon = True; timer.start()
        started = time.monotonic()
        if args.controller_loss:
            (out / 'before-loss-requests.json').write_text(json.dumps(w.rows, indent=2))
            proc.process.kill(); proc.process.wait(timeout=5); proc.reader.join(timeout=2)
            for stream in (proc.process.stdin, proc.process.stdout, proc.process.stderr):
                stream.close()
            w = Witness(out, env); session.w = w; w.process.initialize()
        else:
            send(dict(jsonrpc='2.0', method='notifications/cancelled', params=dict(requestId=request_id)))
        observations = []
        while True:
            value, failed = w.call_result('status')
            elapsed = time.monotonic() - started
            observations.append(dict(seconds=elapsed, failed=failed, value=value))
            (out / 'observations.json').write_text(json.dumps(observations, indent=2))
            if not failed and value.get('connected') and value.get('state') == 'frozen':
                break
            assert elapsed < 6, value
            time.sleep(.02)
        assert elapsed < capability['stop_host_ms'] / 1000 + 1, elapsed
        assert not value['input_override']['engaged']
        assert value['native_input_mask'] == 0
        assert value['native_control']['parked'] and value['native_control']['valid']
        assert value['execution_speed']['percent'] == slowest
        record = json.loads(records[0].read_text())
        if args.controller_loss:
            assert record['temporal_operation'] == active['temporal_operation']
            rejection = w.call('step', dict(frames=1), error=True)
            assert rejection['error']['code'] == 'temporal_quarantined', rejection
        else:
            assert not record.get('temporal_operation'), record
        frame = value['frame']
        for _ in range(3):
            time.sleep(.05); assert w.call('status')['frame'] == frame
        if not args.controller_loss:
            w.speed(dict(mode='unlimited'))
            following = w.call('step', dict(frames=2))
            assert following['status'] == 'completed' and following.get('count', following.get('advanced')) == 2
            assert w.call('status')['frame'] == frame + 2
        timer.cancel()
        (out / 'result.json').write_text(json.dumps(dict(passed=True, controller_loss=args.controller_loss,
            seconds=elapsed, record=record, observations=observations), indent=2))
    finally:
        try:
            if session:
                (out / 'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            if timer: timer.cancel()
            owned_exit()
            w.process.close()


if __name__ == '__main__':
    main()
