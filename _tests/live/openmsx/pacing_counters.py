#!/usr/bin/env python3
"""Opt-in exact frame/input-counter witness for the typed pacing-fixture cartridge.

The profile must launch that cartridge. Counters C100/C102 are little-endian u16 values;
C104 is the alternating border colour. A frame tap must consume exactly its held frames.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observation_speed import ROOT, Witness, free_port, warmup


def check_completed_field(path, current_border):
    from PIL import Image
    with Image.open(path) as image:
        rgb = image.convert('RGB').getpixel((image.width // 2, image.height // 2))
    # The guest updates the border in VBlank; the completed field's center precedes that write.
    assert (min(rgb) > 240 if current_border == 1 else max(rgb) < 16), (current_border, rgb)
    return rgb


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(out/'home'),
               EMUCAP_PORT=str(free_port()))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    launch = None

    def counters():
        data = bytes.fromhex(w.call('read_memory', {
            'memory_type': 'memory', 'address': 0xc100, 'length': 5})['hex'])
        return [int.from_bytes(data[:2], 'little'), int.from_bytes(data[2:4], 'little'), data[4]]

    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = w.call('status')
        (out/'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        breakpoint = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'memory',
            'start': '0x4010', 'end': '0x4010', 'pause_on_hit': True})
        admitted = w.call('step', {'unit': 'frames', 'count': 5000})
        assert admitted['status'] == 'interrupted' and admitted['reason'] == 'breakpoint', admitted
        expected_rom = Path(profile['launch_plan']['content_path']).read_bytes()[:256]
        mapped_rom = bytes.fromhex(w.call('read_memory', {
            'memory_type': 'memory', 'address': 0x4000, 'length': 256})['hex'])
        assert mapped_rom == expected_rom, 'entry PC must belong to the fixture cartridge'
        w.call('clear_breakpoint', {'id': breakpoint['id']})
        w.call('poll_events')
        warmup(w, status, 60)
        # Move past the frame trap to an instruction boundary, then retain one origin.
        w.call('step', {'unit': 'instructions', 'count': 1})
        origin = out/'origin.state'
        w.call('save_state', {'path': str(origin)})
        base = counters()
        assert base[0] > 0 and base[1] == 0 and base[2] in (1, 15), base
        rows = []
        registers_by_case = {}
        cases = [(100, 2, 30, True), (100, 2, 30, False), (50, 2, 30, True),
                 (400, 2, 30, True), (10000, 2, 30, True), (None, 2, 30, True),
                 (100, 1, 2, True), (1, 1, 2, True)]
        for index, (percent, press, after, held) in enumerate(cases):
            policy = {'mode': 'limited', 'percent': percent} if percent else {'mode': 'unlimited'}
            w.speed(policy)
            w.call('load_state', {'path': str(origin)})
            observed_policy = w.speed({})
            assert observed_policy['mode'] == policy['mode']
            assert observed_policy['percent'] == policy.get('percent')
            assert counters() == base
            frames = press + 1 + after
            before_frame = w.call('status')['frame']
            if held:
                advanced = w.call('tap', {'buttons': ['a'], 'press_frames': press, 'after_frames': after})
            else:
                advanced = w.call('step', {'unit': 'frames', 'count': frames})
            current = counters()
            delta = [(current[i] - base[i]) & 0xffff for i in (0, 1)]
            assert delta == [frames, press if held else 0], (policy, held, base, current, delta)
            assert current[2] == base[2] ^ (14 if frames % 2 else 0)
            status = w.call('status')
            assert status['frame'] - before_frame == frames and status['state'] == 'frozen'
            assert status['diagnostics']['audio_muted'] == (not profile.get('launch', {}).get('sound', False))
            row = {'policy': policy, 'held': held, 'frames': frames, 'counter_delta': delta,
                   'result': advanced}
            cpu = w.call('get_state', {'groups': ['cpu']})
            key = (frames, held)
            # Frame sequence is generation-scoped and monotonic across loads. Compare only
            # architectural registers here; the exact guest/frame deltas were checked above.
            assert cpu['state'] == registers_by_case.setdefault(key, cpu['state'])
            row['cpu_state'] = cpu['state']
            if profile.get('launch', {}).get('display', False):
                path = out/f'{index}.png'
                provenance = w.call('screenshot', {'save_path': str(path)})['provenance']
                assert w.call('get_state', {'groups': ['cpu']}) == cpu
                assert provenance['freshness'] == 'latest_completed_frame'
                assert provenance['frame_before'] == provenance['frame_after'] == provenance['frame']
                assert provenance['raster_boundary']['frame'] + 1 == provenance['capture_boundary']['frame']
                assert provenance['raster_boundary']['launch_id'] == launch['launch_id']
                assert hashlib.sha256(path.read_bytes()).hexdigest() == provenance['sha256']
                row['capture'] = provenance
                row['center_rgb'] = check_completed_field(path, current[2])
            rows.append(row)
        (out/'result.json').write_text(json.dumps({'passed': True, 'entry_frame': admitted['frame'], 'origin': base, 'runs': rows}, indent=2))
        print('Exact field/input counters, policy restoration and capture passed')
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
