#!/usr/bin/env python3
"""Exact VBlank/input counter oracle for the synthetic NES/SMS/GG/GB/GBC/GBA cartridges."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from input_pacing import Origin, Session
from observation_speed import ROOT, Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    session = None
    system = profile['launch_plan'].get('system')
    is_nes = system == 'nes'
    is_gba = system == 'gba'
    is_gb = system in ('gb', 'gbc')
    width = 4 if is_gba else 2
    mask = (1 << (8 * width)) - 1

    def counters():
        data = bytes.fromhex(w.call('read_memory', {
            'memory_type': 'nesInternalRam' if is_nes else 'gbaExtWorkRam' if is_gba else 'gbWorkRam' if is_gb else 'smsWorkRam',
            'address': 0 if is_gba else 0x100, 'length': 2 * width})['hex'])
        return [int.from_bytes(data[i:i+width], 'little') for i in (0, width)]

    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        rom = Path(profile['launch_plan']['content_path']).read_bytes()
        mapped = bytes.fromhex(w.call('read_memory', {
            'memory_type': 'nesMemory' if is_nes else 'gbaMemory' if is_gba else 'gameboyMemory' if is_gb else 'smsMemory',
            'address': 0x8000 if is_nes else 0x08000000 if is_gba else 0x100 if is_gb else 0, 'length': 128})['hex'])
        rom_offset = 16 if is_nes else 0x100 if is_gb else 0
        assert mapped == rom[rom_offset:rom_offset+128], 'fixture identity differs at cartridge entry'
        (out/'identity.json').write_text(json.dumps({'launch': session.launch, 'status': status,
            'fixture_sha256': hashlib.sha256(rom).hexdigest()}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 60))
        origin = Origin(w, out, status, 60, session)
        assert origin.path, origin.save_error
        base = counters()
        assert base[0] > 0 and base[1] == 0, base
        rows = []
        for percent, press, after, held in [(100, 2, 30, True), (100, 2, 30, False),
                (50, 2, 30, True), (400, 2, 30, True), (10000, 2, 30, True),
                (None, 2, 30, True),
                (100, 1, 2, True), (1, 1, 2, True)]:
            origin.restore()
            assert counters() == base
            policy = {'mode': 'limited', 'percent': percent} if percent else {'mode': 'unlimited'}
            w.speed(policy)
            before = w.call('status')['frame']
            frames = press + 1 + after
            if held:
                w.call('tap', {'buttons': ['a'], 'press_frames': press, 'after_frames': after})
            else:
                w.call('step', {'unit': 'frames', 'count': frames})
            current = counters()
            delta = [(current[i] - base[i]) & mask for i in (0, 1)]
            assert delta == [frames, press if held else 0], (policy, base, current, delta)
            after_status = w.call('status')
            assert after_status['frame'] - before == frames and after_status['state'] == 'frozen'
            rows.append({'policy': policy, 'held': held, 'delta': delta})
        (out/'result.json').write_text(json.dumps({'passed': True, 'origin': base, 'runs': rows}, indent=2))
    finally:
        try:
            if session:
                stopped = session.stop()
                (out/'stop.json').write_text(json.dumps(stopped, indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
