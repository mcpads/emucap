#!/usr/bin/env python3
"""Compare ordered Wii fixture images/CPU records across restored pacing policies."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port
from dolphin_frame_restore import runtime_binary
from presentation_state import inspect_fixture_state


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--address', type=lambda value: int(value, 0), required=True)
    parser.add_argument('--record-address', type=lambda value: int(value, 0), required=True)
    parser.add_argument('--backend', choices=['Metal', 'OGL'], required=True)
    parser.add_argument('--cpu', type=int, choices=[0, 4, 5], default=4)
    parser.add_argument('--dual-core', action='store_true')
    parser.add_argument('--lle', action='store_true')
    parser.add_argument('--display', action='store_true')
    parser.add_argument('--sound', action='store_true')
    parser.add_argument('--samples', type=int, choices=[1, 2, 4], default=1)
    parser.add_argument('--layers', type=int, choices=[1, 2], default=1)
    parser.add_argument('--immediate-xfb', action='store_true')
    parser.add_argument('--save-texture-cache', action='store_true')
    args = parser.parse_args()
    content = args.content.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    port = free_port()
    home = out / 'home'
    config_dir = home / 'dolphin' / str(port) / 'user/Config'
    config_dir.mkdir(parents=True)
    config = out / 'native-config.ini'
    config.write_text(f'[Core]\nCPUCore = {args.cpu}\nCPUThread = {args.dual_core}\n'
                      f'DSPHLE = {not args.lle}\nGFXBackend = {args.backend}\n'
                      'GPUDeterminismMode = auto\n')
    graphics = out / 'graphics-config.ini'
    graphics.write_text(f'[Settings]\nMSAA = {args.samples}\n'
                        f'SaveTextureCacheToState = {args.save_texture_cache}\n'
                        f'[Stereoscopy]\nStereoMode = {1 if args.layers == 2 else 0}\n'
                        f'[Hacks]\nImmediateXFBEnable = {args.immediate_xfb}\n')
    (config_dir / 'Dolphin.ini').write_bytes(config.read_bytes())
    (config_dir / 'GFX.ini').write_bytes(graphics.read_bytes())
    inputs = [content, config, graphics, Path(__file__).resolve(),
              Path(__file__).with_name('presentation_state.py').resolve(),
              ROOT / '_tests/live/observation_speed.py', ROOT / '_tests/live/mesen2/support.py',
              ROOT / '_tests/live/dolphin_frame_restore.py', ROOT / 'target/release/emucap-mcp',
              ROOT / 'adapters/dolphin/upstream.lock']
    hashes = {str(path): sha(path) for path in inputs}
    witness = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                               EMUCAP_EMU_HOME=str(home), EMUCAP_PORT=str(port)))
    launch = None
    rows = []
    try:
        witness.process.initialize()
        plan = witness.call('launch_plan', {'content_path': str(content), 'system': 'wii'})
        assert plan['ready_to_launch'], plan
        launch = witness.call('launch', {**plan['preferred_launcher']['args'],
                                        'display': args.display, 'sound': args.sound})
        (out / 'launch.json').write_text(json.dumps(launch, indent=2))
        assert sha(launch['binary']) == sha(runtime_binary(launch))
        hashes[launch['binary']] = sha(launch['binary'])
        witness.call('pause')
        components = witness.call('get_state', {'groups': ['cpu']})['runtime_components']
        assert components['video_backend'] == args.backend, components
        assert components['dual_core'] == args.dual_core, components
        assert not components['deterministic_gpu_thread'], components
        assert components['dsp_engine'] == ('lle' if args.lle else 'hle'), components
        witness.call('status')
        witness.speed({'mode': 'unlimited'})
        warmup = witness.call('step', {'count': 120, 'unit': 'frames'})
        assert warmup['status'] == 'completed', warmup
        bp = witness.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'main',
                                           'start': args.address, 'end': args.address,
                                           'pause_on_hit': True})
        assert witness.call('step', {'count': 10, 'unit': 'frames'})['reason'] == 'breakpoint'
        witness.call('clear_breakpoint', {'id': bp['id']})
        witness.call('poll_events')

        def record():
            result = witness.call('read_memory', {'memory_type': 'main',
                                                 'address': args.record_address, 'length': 56})
            values = list(struct.unpack('>14I', bytes.fromhex(result['hex'])))
            assert values[:2] == [0x57504144, 1] and values[13] >= 60, values
            return values

        record()
        origin = out / 'origin.state'
        before = witness.call('get_state', {'groups': ['cpu']})
        witness.call('save_state', {'path': str(origin)})
        assert witness.call('get_state', {'groups': ['cpu']}) == before
        hashes[str(origin)] = sha(origin)
        info = inspect_fixture_state(origin, args.layers, args.samples)
        if args.immediate_xfb:
            assert info['immediate_field'] == 1, 'select a halt with an active immediate field'
        for run, percent in enumerate([100, 100, 50, 400, None, 1]):
            if run:
                result = witness.call('load_state', {'path': str(origin)})
                assert result['presentation_history'] == 'restored', result
                assert witness.call('get_state', {'groups': ['cpu']}) == before
            witness.call('status')
            witness.revision = None
            witness.speed({'mode': 'unlimited'} if percent is None else
                          {'mode': 'limited', 'percent': percent})
            frames = []
            for index in range(6):
                advanced = witness.call('step', {'count': 1, 'unit': 'frames'})
                assert advanced['status'] == 'completed' and advanced['count'] == 1, advanced
                snapshot = out / f'{run}-{index}.state'
                witness.call('save_state', {'path': str(snapshot)})
                hashes[str(snapshot)] = sha(snapshot)
                image = inspect_fixture_state(snapshot, args.layers, args.samples)
                frames.append({'record': record(), 'image': image['image_sha256'],
                               'cpu': witness.call('get_state', {'groups': ['cpu']})})
            rows.append(frames)
            (out / 'runs.json').write_text(json.dumps(rows, indent=2))
            (out / 'producer.json').write_text(json.dumps(hashes, indent=2))
        assert all(row == rows[0] for row in rows[1:]), 'restored continuation differs'
        assert len({frame['image'] for frame in rows[0]}) > 1
        loops = [frame['record'][2] for frame in rows[0]]
        assert all(right > left for left, right in zip(loops, loops[1:])), loops
        (out / 'completed.json').write_text(json.dumps({'passed': True, 'policies': 6,
                                                       'frames': 6, 'runtime': components}))
    finally:
        try:
            if launch:
                witness.call('stop', {'launch_id': launch['launch_id']})
        finally:
            witness.process.close()
    assert all(sha(path) == digest for path, digest in hashes.items())
    assert subprocess.run(['ps', '-p', str(launch['pid'])],
                          stdout=subprocess.DEVNULL).returncode != 0
    (out / 'audit.json').write_text(json.dumps({'inputs_unchanged': True,
                                               'owned_process_absent': True}))


if __name__ == '__main__':
    main()
