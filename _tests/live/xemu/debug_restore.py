#!/usr/bin/env python3
"""Exercise watchpoint stops and restored JIT code. Requires clang and llvm-objcopy.

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
        obj, code = out / 'write-loop.o', out / 'write-loop.bin'
        watched = (state['cpu.esp'] - 64) & ~3
        assembly = f".code32\n.text\ncli\n1: movl $0x12345678, {watched}\njmp 1b\n"
        subprocess.run(['clang', '-target', 'i386-none-elf', '-x', 'assembler', '-c',
                        '-o', str(obj), '-'], input=assembly, text=True, check=True)
        subprocess.run(['llvm-objcopy', '-O', 'binary', '--only-section=.text',
                        str(obj), str(code)], check=True)
        rows = []
        for iteration, percent in enumerate((50, 200, 100)):
            w.call('load_state', {'path': checkpoint})
            w.call('write_memory', {'memory_type': 'cpu', 'address': pc,
                                   'hex': code.read_bytes().hex()})
            w.call('step', {'unit': 'instructions', 'count': 1})
            entered = w.call('get_state', {'groups': ['cpu']})['state']
            assert entered['cpu.eip'] == pc + 1 and not entered['cpu.eflags'] & 0x200, entered
            breakpoint = w.call('set_breakpoint', {'kind': 'write', 'memory_type': 'cpu',
                                'start': watched, 'end': watched + 3, 'pause_on_hit': True})
            w.speed({'mode': 'limited', 'percent': percent})
            if iteration == 0:
                stopped = w.call('step', {'unit': 'frames', 'count': 30})
                assert stopped['status'] == 'interrupted', stopped
            else:
                stopped = w.call('tap', {'buttons': ['a'], 'press_frames': 6, 'after_frames': 30})
            assert stopped['state'] == 'frozen', stopped
            status = w.call('status')
            assert status['state'] == 'frozen'
            assert status['input_override']['engaged'] is False
            w.call('get_state', {'groups': ['cpu']})
            w.call('read_memory', {'memory_type': 'cpu', 'address': watched, 'length': 4})
            events = w.call('poll_events')
            assert any(e.get('watch_address') == watched for e in events['events']), events
            w.call('clear_breakpoint', {'id': breakpoint['id']})
            w.call('step', {'unit': 'instructions', 'count': 1})
            assert w.call('read_memory', {'memory_type': 'cpu', 'address': watched,
                                          'length': 4})['hex'] == '78563412'
            w.call('load_state', {'path': checkpoint})
            assert w.call('get_state', {'groups': ['cpu']})['state'] == state
            step = w.call('step', {'unit': 'frames', 'count': 30})
            assert step['status'] == 'completed', step
            rows.append({'percent': percent, 'advance': stopped, 'events': events})
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
