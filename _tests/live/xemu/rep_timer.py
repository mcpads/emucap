#!/usr/bin/env python3
"""Compare REP work, TSC and native timer counts across pacing policies from one saved origin.

This is a masked-IRQ fixture, not proof of interrupt delivery/order or whole-game determinism.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--clock-shift', type=int, choices=range(4), required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    # Preserve actual execution code and its source delta, including untracked harness files.
    for name in ('rep_timer.py', 'rep_timer.S'):
        shutil.copyfile(Path(__file__).with_name(name), out / name)
    (out / 'source.diff').write_bytes(subprocess.check_output(['git', 'diff', 'HEAD'], cwd=ROOT))
    digests = {}
    for name in ('emucap', 'emucap-mcp', 'emucap-broker', 'emucap-xemu-bridge'):
        with (ROOT / 'target/release' / name).open('rb') as stream:
            digests[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    (out / 'producer.json').write_text(json.dumps({
        'source_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'binaries': digests}, indent=2))
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_XEMU_QUALIFICATION_SHIFT=str(args.clock_shift),
                         EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home')))
    patch_copy = out / 'native-patches'
    patch_copy.mkdir()
    for patch in (ROOT / 'adapters/xemu/patches').glob('*.patch'):
        shutil.copyfile(patch, patch_copy / patch.name)
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system': 'xbox', 'content_path': str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], 'start_frozen': True})
        status = w.call('status')
        profile = status['clock_profile']
        assert profile['instruction-ns'] == 1 << args.clock_shift
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, 300)
        state = w.call('get_state', {'groups': ['cpu']})['state']
        assert state['cpu.cs'] & 3 == 0 and state['cpu.ds'] == state['cpu.es'], state
        pc, scratch = state['cpu.eip'], (state['cpu.esp'] - 1024) & ~15
        w.call('read_memory', {'memory_type': 'cpu', 'address': scratch, 'length': 528})
        checkpoint = str(out / 'origin.json')
        w.call('save_state', {'path': checkpoint})
        iterations = 3_000_000
        assembly = f'.set SCRATCH, {scratch}\n.set ITERATIONS, {iterations}\n' + (out / 'rep_timer.S').read_text()
        (out / 'resolved.S').write_text(assembly)
        subprocess.run(['clang', '-target', 'i386-none-elf', '-x', 'assembler', '-c',
                        '-o', str(out / 'fixture.o'), '-'], input=assembly, text=True, check=True)
        subprocess.run(['llvm-objcopy', '-O', 'binary', '--only-section=.text',
                        str(out / 'fixture.o'), str(out / 'fixture.bin')], check=True)
        code = (out / 'fixture.bin').read_bytes()
        rows, reference = [], None
        policies = [{'mode': 'limited', 'percent': p} for p in (1, 50, 100, 200, 400, 1000)]
        policies.append({'mode': 'unlimited'})
        for policy in policies:
            w.call('load_state', {'path': checkpoint})
            w.call('write_memory', {'memory_type': 'cpu', 'address': pc, 'hex': code.hex()})
            w.call('write_memory', {'memory_type': 'cpu', 'address': scratch, 'hex': bytes(528).hex()})
            stop = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'cpu',
                                           'start': pc + 256, 'end': pc + 256, 'pause_on_hit': True})
            w.speed(policy)
            before = w.call('status')
            advanced = w.call('step', {'unit': 'frames', 'count': 10})
            assert advanced['status'] == 'interrupted' and advanced['state'] == 'frozen', advanced
            after = w.call('status')
            cpu = w.call('get_state', {'groups': ['cpu']})['state']
            assert cpu['cpu.eip'] == pc + 256 and not cpu['cpu.eflags'] & 0x200, cpu
            payload = bytes.fromhex(w.call('read_memory', {'memory_type': 'cpu', 'address': scratch,
                                                           'length': 528})['hex'])
            expected = struct.pack('<I', 0x13579bdf) * 64
            assert payload[:256] == payload[256:512] == expected
            start_tsc, end_tsc = struct.unpack_from('<QQ', payload, 512)
            assert end_tsc > start_tsc
            a, b = before['scheduler_diagnostics'], after['scheduler_diagnostics']
            delta = {key: b[key] - a[key] for key in a}
            assert delta['virtual_ns'] == delta['instructions'] * profile['instruction-ns'], delta
            assert end_tsc - start_tsc >= (2 * iterations * profile['instruction-ns'] * 733333333) // 10**9 - 1
            observation = {'cpu': cpu, 'payload': payload.hex(), 'delta': delta,
                           'frames': after['frame'] - before['frame']}
            if reference is None:
                reference = observation
            assert observation == reference, ('rate changed fixture outcome', policy, reference, observation)
            assert before['clock_profile'] == after['clock_profile'] == profile
            rows.append({'policy': policy, **observation})
            (out / 'result.json').write_text(json.dumps(rows, indent=2))
            w.call('clear_breakpoint', {'id': stop['id']})
            w.call('poll_events')
        w.call('load_state', {'path': checkpoint})
        assert w.call('get_state', {'groups': ['cpu']})['state'] == state
        print(json.dumps({'passed': True, 'policies': len(rows), 'delta': reference['delta']}, indent=2))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
