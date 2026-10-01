#!/usr/bin/env python3
"""Bounded snapshot/render comparison between an explicit native producer and a candidate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--producer-root', type=Path, default=ROOT)
    parser.add_argument('--clock-shift', type=int, choices=range(4), default=0)
    parser.add_argument('--count', type=int, default=30)
    parser.add_argument('--warmup-frames', type=int, default=300)
    args = parser.parse_args()
    assert 1 <= args.count <= 100 and args.warmup_frames > 0
    producer, out = args.producer_root.resolve(), args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(__file__, out / 'restore_stress.py')
    identities = {}
    for name in ('emucap', 'emucap-mcp', 'emucap-broker', 'emucap-xemu-bridge'):
        with (producer / 'target/release' / name).open('rb') as stream:
            identities[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    (out / 'producer.json').write_text(json.dumps({'binaries': identities, 'source_revision':
        subprocess.check_output(['git','rev-parse','HEAD'], cwd=producer, text=True).strip()}, indent=2))
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(producer),
        EMUCAP_XEMU_BIN=str(producer / 'adapters/xemu/work/xemu/dist/xemu.app/Contents/MacOS/xemu'),
        EMUCAP_XEMU_BRIDGE_BIN=str(producer / 'target/release/emucap-xemu-bridge'),
        EMUCAP_XEMU_QUALIFICATION_SHIFT=str(args.clock_shift),
        EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home')),
        producer / 'target/release/emucap-mcp')
    patch_copy = out / 'native-patches'
    patch_copy.mkdir()
    for patch in (producer / 'adapters/xemu/patches').glob('*.patch'):
        shutil.copyfile(patch, patch_copy / patch.name)
    with args.content.open('rb') as content:
        (out/'content.sha256').write_text(hashlib.file_digest(content,'sha256').hexdigest()+'\n')
    (out/'source.diff').write_bytes(subprocess.check_output(['git','diff','HEAD'],cwd=producer))
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system':'xbox','content_path':str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'],'start_frozen':True})
        status = w.call('status')
        (out / 'identity.json').write_text(json.dumps({'launch':launch,'status':status},indent=2))
        if 'execution_speed' in status['methods']:
            w.speed({'mode':'unlimited'})
        warmup(w,status,args.warmup_frames)
        checkpoint = str(out / 'origin.json')
        w.call('save_state',{'path':checkpoint})
        origin_state = w.call('get_state',{'groups':['cpu']})
        origin_image = out / 'origin.png'
        w.call('screenshot',{'save_path':str(origin_image)})
        origin_digest = hashlib.sha256(origin_image.read_bytes()).hexdigest()
        (out/'expected.json').write_text(json.dumps({'state':origin_state,
            'png_sha256':origin_digest,'warmup_frames':args.warmup_frames},indent=2))
        rows = []
        for index in range(args.count):
            w.call('load_state',{'path':checkpoint})
            screenshot = out / f'restored-{index:02}.png'
            w.call('screenshot',{'save_path':str(screenshot)})
            state = w.call('get_state',{'groups':['cpu']})
            digest = hashlib.sha256(screenshot.read_bytes()).hexdigest()
            rows.append({'index':index,'state':state,'png_sha256':digest,
                         'cpu_matches':state == origin_state,'png_matches':digest == origin_digest})
            (out / 'result.json').write_text(json.dumps(rows,indent=2))
            assert rows[-1]['cpu_matches'], f'restore {index} changed saved CPU state'
            assert rows[-1]['png_matches'], f'restore {index} changed saved image; see retained PNGs'
        print(json.dumps({'passed':True,'restores':len(rows)}))
    finally:
        try:
            if launch:
                w.call('stop',{'launch_id':launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
