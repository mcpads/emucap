#!/usr/bin/env python3
"""Qualify pacing while a privileged CPU is halted. Requires clang and llvm-objcopy.

Each fixture is assembled, installed in an isolated managed session, and discarded by loading
its checkpoint. The caller-owned media is never modified.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    obj, code = out / 'halted.o', out / 'halted.bin'
    subprocess.run(['clang', '-target', 'i386-none-elf', '-c',
                    str(Path(__file__).with_name('halted_cpu.S')), '-o', str(obj)], check=True)
    subprocess.run(['llvm-objcopy', '-O', 'binary', '--only-section=.text', str(obj), str(code)], check=True)
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home')))
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system': 'xbox', 'content_path': str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], 'start_frozen': True})
        status = w.call('status')
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, 300)
        state = w.call('get_state', {'groups': ['cpu']})['state']
        assert state['cpu.cs'] & 3 == 0, 'fixture requires a privileged instruction boundary'
        pc = state['cpu.eip']
        checkpoint = str(out / 'checkpoint.json')
        w.call('save_state', {'path': checkpoint})
        rows = []
        for percent, frames in [(1, 3), (50, 30), (100, 30), (200, 30)]:
            w.call('load_state', {'path': checkpoint})
            w.call('write_memory', {'memory_type': 'cpu', 'address': pc, 'hex': code.read_bytes().hex()})
            w.speed({'mode': 'limited', 'percent': percent})
            start = time.monotonic()
            step = w.call('step', {'unit': 'frames', 'count': frames})
            elapsed = time.monotonic() - start
            assert step['status'] == 'completed', step
            after = w.call('get_state', {'groups': ['cpu']})['state']
            assert after['cpu.eip'] == pc + 2 and not after['cpu.eflags'] & 0x200, after
            target = frames / 60 * 100 / percent
            assert elapsed >= target * .8, 'halted clock bypassed host pacing'
            rows.append({'percent': percent, 'frames': frames, 'host_seconds': elapsed,
                         'target_seconds': target, 'pc_after_hlt': after['cpu.eip']})
        w.call('load_state', {'path': checkpoint})
        assert w.call('get_state', {'groups': ['cpu']})['state'] == state
        (out / 'result.json').write_text(json.dumps(rows, indent=2))
        print(json.dumps(rows, indent=2))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
