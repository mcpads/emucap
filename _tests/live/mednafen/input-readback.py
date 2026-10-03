#!/usr/bin/env python3
"""Opt-in frozen input replacement/release witness against native buffer readback."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observation_speed import Witness, ROOT, free_port
from input_pacing import Session
from support import terminate_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home'))
    w = Witness(out, env); session = None; owned = []
    try:
        w.process.initialize(); w.call('bootstrap')
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile['launch']})
        session.start(); status = w.call('status')
        (out / 'identity.json').write_text(json.dumps(status, indent=2))
        for role in ('bridge_pid', 'emulator_pid'):
            pid = status['runtime_instance'].get(role)
            if pid:
                fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
                owned.append((role, pid, fingerprint))
        assert status['state'] == 'frozen'
        frame = status['frame']
        native_origin = status['native_control']
        assert native_origin['valid'] and native_origin['parked'], status
        assert native_origin['context'] == 'frame_boundary', status
        buttons = ['cross', 'circle'] if profile['launch_plan']['system'] == 'psx' else ['a', 'b']
        checks = []
        for policy in ({'mode': 'limited', 'percent': 1}, {'mode': 'limited', 'percent': 10000}, {'mode': 'unlimited'}):
            w.speed(policy)
            masks = []
            for held in ([buttons[0]], [buttons[1]], []):
                w.call('debug', {'operation': 'set_input', 'known_capability_revision': w.revision,
                                'arguments': {'port': 0, 'buttons': held}})
                actual = w.call('status')
                assert actual['state'] == 'frozen' and actual['frame'] == frame, actual
                assert actual['native_control'] == native_origin, actual
                assert actual['native_input_mask'] == actual['input_override']['pressed_mask'], actual
                assert actual['input_override']['engaged'] == bool(held), actual
                masks.append(actual['native_input_mask'])
            assert masks[0] and masks[1] and masks[0] != masks[1] and masks[2] == 0, masks
            checks.append(dict(policy=policy, native_masks=masks))
        w.speed({'mode': 'limited', 'percent': 100})
        result = w.call('tap', {'buttons': [buttons[0]], 'press_frames': 2, 'after_frames': 2})
        final = w.call('status')
        assert not final['input_override']['engaged'] and final['native_input_mask'] == 0, final
        native_final = final['native_control']
        assert native_final['valid'] and native_final['parked'], final
        assert native_final['context'] == 'frame_boundary', final
        assert native_final['generation'] == native_origin['generation'], final
        # Public tap includes one release-edge frame before its trailing frames.
        assert native_final['completed_frames'] - native_origin['completed_frames'] == 2 + 1 + 2, final
        binary = ROOT / 'adapters/mednafen/work/mednafen/src/mednafen'
        (out / 'artifacts.json').write_text(json.dumps({str(binary): hashlib.sha256(binary.read_bytes()).hexdigest(),
            'sidecar': json.loads(Path(str(binary) + '.emucap-build.json').read_text())}, indent=2))
        (out / 'result.json').write_text(json.dumps(dict(passed=True, checks=checks, tap=result), indent=2))
    finally:
        try:
            if session: (out / 'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            rows = []
            for role, pid, fingerprint in owned:
                actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
                if actual.returncode == 0:
                    assert actual.stdout == fingerprint
                    terminate_owned(pid)
                absent = subprocess.run(['ps', '-p', str(pid)], stdout=subprocess.DEVNULL).returncode != 0
                rows.append(dict(role=role, pid=pid, exit_verified=absent)); assert absent
            (out / 'process-exits.json').write_text(json.dumps(rows, indent=2))
            w.process.close()


if __name__ == '__main__':
    main()
