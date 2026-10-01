#!/usr/bin/env python3
"""Check executable halt sites after warmup, re-arming and snapshot restoration."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port
from dolphin_frame_restore import runtime_binary


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--system', choices=['wii', 'gamecube'], required=True)
    parser.add_argument('--cpu', type=int, choices=[0, 4, 5], required=True)
    parser.add_argument('--address', type=lambda value: int(value, 0), action='append', required=True)
    parser.add_argument('--display', action='store_true')
    parser.add_argument('--video-backend', help='Require this native graphics backend')
    parser.add_argument('--save-texture-cache', action='store_true',
                        help='Exercise native GPU readback during state capture')
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    content = args.content.resolve(strict=True)
    port = free_port()
    home = out / 'home'
    cfg = home / 'dolphin' / str(port) / 'user/Config/Dolphin.ini'
    cfg.parent.mkdir(parents=True)
    cfg.write_text(f'[Core]\nCPUCore = {args.cpu}\nCPUThread = False\nDSPHLE = True\nGPUDeterminismMode = auto\n')
    if args.video_backend:
        with cfg.open('a') as config:
            config.write(f'GFXBackend = {args.video_backend}\n')
    gfx_copy = out / 'graphics-config.ini'
    gfx_copy.write_text(f'[Settings]\nSaveTextureCacheToState = {args.save_texture_cache}\n')
    (cfg.parent / 'GFX.ini').write_bytes(gfx_copy.read_bytes())
    config_copy = out / 'native-config.ini'
    config_copy.write_text(cfg.read_text())
    inputs = [content, Path(__file__).resolve(), ROOT / '_tests/live/observation_speed.py',
              ROOT / '_tests/live/mesen2/support.py', ROOT / '_tests/live/dolphin_frame_restore.py',
              ROOT / 'target/release/emucap-mcp', ROOT / 'adapters/dolphin/upstream.lock', config_copy,
              gfx_copy]
    hashes = {str(p): sha(p) for p in inputs}
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_EMU_HOME=str(home), EMUCAP_PORT=str(port)))
    launch = None
    rows = []
    try:
        w.process.initialize()
        plan = w.call('launch_plan', {'content_path': str(content), 'system': args.system})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'],
                                  'display': args.display, 'sound': False})
        (out / 'launch.json').write_text(json.dumps(launch, indent=2))
        assert sha(launch['binary']) == sha(runtime_binary(launch))
        hashes[launch['binary']] = sha(launch['binary'])
        (out / 'producer.json').write_text(json.dumps(hashes, indent=2))
        w.call('pause')
        w.call('status')
        w.speed({'mode': 'unlimited'})
        w.call('step', {'count': 120, 'unit': 'frames'})
        state = w.call('get_state', {'groups': ['cpu']})
        components = state['runtime_components']
        assert components['cpu_engine'] == {0: 'Interpreter64', 4: 'JITARM64', 5: 'Cached Interpreter'}[args.cpu]
        assert not components['dual_core']
        if args.video_backend:
            assert components['video_backend'] == args.video_backend, components
        for address in args.address:
            for attempt in range(2):
                bp = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'main',
                            'start': address, 'end': address, 'pause_on_hit': True})
                result = w.call('step', {'count': 10, 'unit': 'frames'})
                state = w.call('get_state', {'groups': ['cpu']})
                row = {'address': address, 'attempt': attempt, 'breakpoint': bp,
                       'result': result, 'state': state}
                rows.append(row)
                (out / 'runs.json').write_text(json.dumps(rows, indent=2))
                assert result['status'] == 'interrupted' and result['reason'] == 'breakpoint', row
                assert state['state']['cpu.pc'] == address, row
                origin = out / f'{address:08x}-{attempt}.state'
                frame_before_save = w.call('status')['frame']
                w.call('save_state', {'path': str(origin)})
                assert w.call('get_state', {'groups': ['cpu']}) == state
                assert w.call('status')['frame'] == frame_before_save
                hashes[str(origin)] = sha(origin)
                w.call('clear_breakpoint', {'id': bp['id']})
                w.call('poll_events')
                advanced = w.call('step', {'count': 2, 'unit': 'frames'})
                assert advanced['status'] == 'completed', advanced
                w.call('load_state', {'path': str(origin)})
                restored = w.call('get_state', {'groups': ['cpu']})
                assert restored == state, restored
                resumed = w.call('step', {'count': 1, 'unit': 'instructions'})
                assert resumed['status'] == 'completed', resumed
                advanced = w.call('step', {'count': 2, 'unit': 'frames'})
                assert advanced['status'] == 'completed', advanced
                print(hex(address), attempt, 'passed', flush=True)
        (out / 'producer.json').write_text(json.dumps(hashes, indent=2))
        (out / 'completed.json').write_text(json.dumps({'passed': True, 'cases': len(rows)}))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()
    assert all(sha(p) == h for p, h in hashes.items()), 'producer inputs changed'
    assert subprocess.run(['ps', '-p', str(launch['pid'])], stdout=subprocess.DEVNULL).returncode != 0
    (out / 'audit.json').write_text(json.dumps({'inputs_unchanged': True, 'owned_process_absent': True}))


if __name__ == '__main__':
    main()
