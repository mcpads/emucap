#!/usr/bin/env python3
"""Compare native states, images and public recording bundles from one saved origin."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observation_speed import ROOT, Witness, free_port, warmup
from input_pacing import Session
from recording_events import check_frame_sequence, normalized_events
from state_sections import StateSections


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--frames', type=int, default=6)
    args = parser.parse_args()
    assert 1 <= args.frames <= 300
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    (out / 'bundles').mkdir()
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env); session = None; rows = []
    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        assert status['contracts']['state'] == 'validated'
        (out / 'identity.json').write_text(json.dumps(dict(launch=session.launch, status=status), indent=2))
        capability = status['recording_capability']
        assert args.frames <= capability['limits']['max_frames']
        assert 'next_frame_boundary' in capability['origins']
        assert {'frame_boundary', 'frame_completed'} <= {x['id'] for x in capability['event_classes']}
        w.speed(dict(mode='unlimited'))
        warmup(w, status, profile.get('warmup_frames', 60))
        origin = out / 'origin.mcs'
        w.call('save_state', dict(path=str(origin)))
        origin_digest = hashlib.sha256(origin.read_bytes()).hexdigest()
        reference = None
        for index, rate in enumerate((100, 100, 50, 10000, 0)):
            # The first reference runs uninterrupted; all candidates use the same saved bytes.
            if index:
                w.call('step', dict(unit='frames', count=1))
                w.call('load_state', dict(path=str(origin)))
            policy = dict(mode='limited', percent=rate) if rate else dict(mode='unlimited')
            changed = w.speed(policy)
            assert all(changed['execution_speed'].get(k) == v for k, v in policy.items())
            before = w.call('status')
            recorded = w.call('debug', dict(operation='record_window',
                known_capability_revision=before['capability_revision'], arguments=dict(
                    output_root=str(out / 'bundles'), frames=args.frames,
                    origin='next_frame_boundary', event_classes=['frame_boundary', 'frame_completed'])))
            after = w.call('status')
            assert after['state'] == 'frozen'
            assert after['execution_speed'] == before['execution_speed']
            events, event_hash = normalized_events(Path(recorded['bundle_path']))
            check_frame_sequence(events, args.frames)
            snapshot = out / f'after-{index}.mcs'
            w.call('save_state', dict(path=str(snapshot)))
            fields = StateSections(snapshot.read_bytes()).sections
            signature = {section: {name: hashlib.sha256(value).hexdigest() for name, value in values.items()}
                         for section, values in fields.items()}
            png = out / f'after-{index}.png'
            w.call('screenshot', dict(save_path=str(png)))
            with Image.open(png) as image:
                rgb = image.convert('RGB')
                pixels = dict(size=list(rgb.size), sha256=hashlib.sha256(rgb.tobytes()).hexdigest())
            current = dict(events=events, fields=signature, image=pixels)
            if reference is None:
                reference = current
            row = dict(policy=policy, recording=recorded, event_sha256=event_hash,
                       observation=current, matches=current == reference)
            rows.append(row)
            (out / 'result.json').write_text(json.dumps(dict(
                passed=all(r['matches'] for r in rows), snapshot_sha256=origin_digest, runs=rows), indent=2))
            assert row['matches'], f'recording/state/image differs at {policy}'
    finally:
        try:
            if session:
                (out / 'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
