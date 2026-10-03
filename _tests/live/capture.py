#!/usr/bin/env python3
"""Verify capture bytes and the declared running/stable/advancing boundary.

This witness does not claim ordered-image equivalence across execution speeds.
Profiles explicitly choose capture_mode and a visible guest scene via warmup.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path

from PIL import Image
from input_pacing import Session
from observation_speed import ROOT, Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    mode = profile['capture_mode']
    assert mode in ('running', 'frozen_stable', 'frozen_advancing'), mode
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    session = None
    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        (out / 'identity.json').write_text(json.dumps({'launch': session.launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 300))
        for action in profile.get('pre_origin_actions', []):
            assert action['tool'] in ('tap', 'step', 'change_media')
            w.call(action['tool'], action.get('arguments', {}))
        if mode == 'running':
            w.call('resume')
        images = []
        for index in range(3):
            before = w.call('status')
            assert before['state'] == ('running' if mode == 'running' else 'frozen'), before
            path = out / f'capture-{index}.png'
            reply = w.call('screenshot', {'save_path': str(path)})
            receipt = reply['provenance']
            after = w.call('status')
            assert after['state'] == before['state'], after
            if mode == 'frozen_stable':
                assert after['frame'] == before['frame'], (before, after)
            elif mode == 'frozen_advancing':
                assert receipt['frame_stable'] is False, reply
                assert receipt['frame_before'] == before['frame'], reply
                assert receipt['frame_after'] == after['frame'] > before['frame'], reply
            else:
                assert after['frame'] >= before['frame'], (before, after)
            data = path.read_bytes()
            digest = hashlib.sha256(data).hexdigest()
            assert receipt['sha256'] == digest, reply
            assert receipt['byte_len'] == len(data), reply
            with Image.open(path) as image:
                assert image.format == 'PNG' and min(image.size) > 0
                rgb = image.convert('RGB')
                extrema = rgb.getextrema()
                size = list(rgb.size)
            images.append({'sha256': digest, 'size': size, 'extrema': extrema,
                           'frame_before': before['frame'], 'frame_after': after['frame'], 'reply': reply})
        # The profile must reach an actual visible scene, not an empty framebuffer.
        assert any(any(low != high for low, high in row['extrema']) for row in images), 'fixture scene is uniform'
        if mode == 'running':
            w.call('pause')
        assert w.call('status')['state'] == 'frozen'
        result = {'passed': True, 'capture_mode': mode, 'images': images}
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    finally:
        try:
            if session and session.launch:
                (out / 'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
