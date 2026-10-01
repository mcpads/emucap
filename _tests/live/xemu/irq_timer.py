#!/usr/bin/env python3
"""Compare delivered PIT/RTC IRQ traces and busy-loop progress across pacing policies.
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
    parser.add_argument('--idle', action='store_true', help='Wait with HLT between delivered IRQs')
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    # Preserve actual execution code and its source delta, including untracked harness files.
    for name in ('irq_timer.py', 'irq_timer.S'):
        shutil.copyfile(Path(__file__).with_name(name), out / name)
    (out / 'source.diff').write_bytes(subprocess.check_output(['git', 'diff', 'HEAD'], cwd=ROOT))
    digests = {}
    for name in ('emucap', 'emucap-mcp', 'emucap-broker', 'emucap-xemu-bridge'):
        with (ROOT / 'target/release' / name).open('rb') as stream:
            digests[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    with args.content.open('rb') as stream:
        content_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    (out / 'producer.json').write_text(json.dumps({
        'source_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'binaries': digests, 'content_sha256': content_digest}, indent=2))
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
        pc, scratch = state['cpu.eip'], (state['cpu.esp'] - 4096) & ~15
        w.call('read_memory', {'memory_type': 'cpu', 'address': scratch, 'length': 2592})
        checkpoint = str(out / 'origin.json')
        w.call('save_state', {'path': checkpoint})
        assembly = f'.set IDLE, {int(args.idle)}\n.set SCRATCH, {scratch}\n.set CODE, {pc}\n.set CS, {state["cpu.cs"]}\n' + (out / 'irq_timer.S').read_text()
        (out / 'resolved.S').write_text(assembly)
        subprocess.run(['clang', '-target', 'i386-none-elf', '-x', 'assembler', '-c',
                        '-o', str(out / 'fixture.o'), '-'], input=assembly, text=True, check=True)
        subprocess.run(['llvm-objcopy', '-O', 'binary', '--only-section=.text',
                        str(out / 'fixture.o'), str(out / 'fixture.bin')], check=True)
        code = (out / 'fixture.bin').read_bytes()
        # Establish the trial origin with IF already clear. The game's pending
        # IRQ may otherwise run before the injected first CLI instruction.
        w.call('write_memory', {'memory_type': 'cpu', 'address': pc, 'hex': code.hex()})
        entry = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'cpu',
                     'start': pc+1, 'end': pc+1, 'pause_on_hit': True})
        assert w.call('step', {'unit': 'frames', 'count': 10})['status'] == 'interrupted'
        armed_cpu = w.call('get_state', {'groups': ['cpu']})['state']
        assert armed_cpu['cpu.eip'] == pc+1 and not armed_cpu['cpu.eflags'] & 0x200
        w.call('clear_breakpoint', {'id': entry['id']})
        prepared = str(out / 'prepared.json')
        w.call('save_state', {'path': prepared})
        rows, reference = [], None
        policies = [{'mode': 'limited', 'percent': p} for p in (1, 50, 100, 200, 400, 1000)]
        policies.append({'mode': 'unlimited'})
        for policy in policies:
            w.call('load_state', {'path': prepared})
            assert w.call('get_state', {'groups': ['cpu']})['state'] == armed_cpu
            w.call('write_memory', {'memory_type': 'cpu', 'address': scratch, 'hex': bytes(2592).hex()})
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
                                                           'length': 2592})['hex'])
            trace = [struct.unpack_from('<IQI', payload, 2048 + index*16) for index in range(32)]
            count, work = struct.unpack_from('<II', payload, 2560)
            assert count == 32 and work > 0, (count, work)
            assert all(row[0] in (0x20, 0x28) and row[2] == 0 for row in trace), trace
            assert all(b[1] > a[1] for a, b in zip(trace, trace[1:])), trace
            # Bound entry jitter by 128 guest instructions (including another ISR)
            # plus TSC rounding. PIT=1125001 Hz, RTC=32768/32 Hz.
            tolerance = (128 * profile['instruction-ns'] * 733333333 + 10**9-1) // 10**9 + 2
            for vector, numerator, denominator in ((0x20,1193,1125001), (0x28,1,1024)):
                ticks = [row[1] for row in trace if row[0] == vector]
                assert len(ticks) >= 10, (vector, ticks)
                for index, tick in enumerate(ticks):
                    error = abs((tick-ticks[0]) * denominator - index * numerator * 733333333)
                    assert error <= tolerance * denominator, (vector, index, error, tolerance)
            a, b = before['scheduler_diagnostics'], after['scheduler_diagnostics']
            delta = {key: b[key] - a[key] for key in a}
            instruction_time = delta['instructions'] * profile['instruction-ns']
            if args.idle:
                assert delta['virtual_ns'] > instruction_time, delta
            else:
                assert delta['virtual_ns'] == instruction_time, delta
            observation = {'cpu': cpu, 'payload': payload.hex(), 'delta': delta,
                           'frames': after['frame'] - before['frame']}
            if reference is None:
                reference = observation
            if observation != reference:
                (out / 'mismatch.json').write_text(json.dumps({'policy': policy, 'expected': reference, 'actual': observation}, indent=2))
                raise AssertionError(f'rate changed fixture outcome; see {out / "mismatch.json"}')
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
