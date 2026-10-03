#!/usr/bin/env python3
"""Compare immediate and consecutive guest images from one saved origin across paces."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from PIL import Image
from input_pacing import Origin, Session
from observation_speed import ROOT, Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    setup = profile.get('pre_origin_actions', [])
    assert all(action['tool'] in ('tap', 'step', 'change_media') for action in setup)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out/'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    session = None
    rows = []

    def capture(run, frame):
        path = out/f'{run}-{frame}.png'
        before = w.call('status')
        reply = w.call('screenshot', {'save_path': str(path)})
        after = w.call('status')
        assert before['frame'] == after['frame'] and after['state'] == 'frozen'
        observation = {}
        if profile.get('image_observation_ranges'):
            state = w.call('get_state')
            memory = w.call('read_memory_batch', {'ranges': profile['image_observation_ranges']})
            observation = {'state': state, 'memory_sha256': hashlib.sha256(
                b''.join(bytes.fromhex(item['hex']) for item in memory['reads'])).hexdigest()}
        with Image.open(path) as image:
            rgb = image.convert('RGB')
            return {'size': list(rgb.size), 'pixels_sha256': hashlib.sha256(rgb.tobytes()).hexdigest(),
                    'center_rgb': list(rgb.getpixel((rgb.width//2, rgb.height//2))),
                    'reply': reply, **observation}

    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        (out/'identity.json').write_text(json.dumps({'launch': session.launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 60))
        # Explicit game/media preparation remains in the same owned generation and request log.
        for action in setup:
            w.call(action['tool'], action.get('arguments', {}))
        if profile.get('origin_instructions'):
            result = w.call('step', {'unit': 'instructions', 'count': profile['origin_instructions']})
            assert result['status'] == 'completed', result
        raster = profile.get('origin_raster')
        if raster:
            for _ in range(200):
                state = w.call('get_state')['state']
                if raster['first'] <= state[raster['scanline']] <= raster['last']:
                    break
                result = w.call('step', {'unit': 'instructions', 'count': 100})
                assert result['status'] == 'completed', result
            else:
                raise AssertionError('requested visible-raster origin was not reached')
        origin = Origin(w, out, status, 60, session)
        assert origin.path, origin.save_error
        (out/'origin-state.json').write_text(json.dumps(w.call('get_state'), indent=2))
        reference = None
        for run, percent in enumerate([100, 100, 50, 400, 10000, None, 1]):
            if run:
                # Disturb the live raster before restoring; an even-length alternating
                # fixture can otherwise accidentally leave matching unsaved buffers.
                disturbed = w.call('step', {'unit': 'frames', 'count': 1})
                assert disturbed['status'] == 'completed', disturbed
                origin.restore()
            policy = {'mode': 'limited', 'percent': percent} if percent else {'mode': 'unlimited'}
            w.speed(policy)
            images = [capture(run, 0)]
            for frame in range(1, 7):
                result = w.call('step', {'unit': 'frames', 'count': 1})
                assert result['status'] == 'completed', result
                images.append(capture(run, frame))
            signature = [(i['size'], i['pixels_sha256']) for i in images]
            if reference is None:
                reference = signature
                assert len({i[1] for i in signature[1:]}) > 1, 'fixture must visibly change'
            rows.append({'policy': policy, 'images': images, 'matches': signature == reference})
            (out/'result.json').write_text(json.dumps({'passed': all(r['matches'] for r in rows), 'runs': rows}, indent=2))
            assert signature == reference, f'ordered images differ at pace {policy}'
    finally:
        try:
            if session:
                (out/'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
