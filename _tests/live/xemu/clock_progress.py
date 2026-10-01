#!/usr/bin/env python3
"""Opt-in Xbox clock witness. Requires a caller-owned XISO and managed machine inputs.

Read KeTickCount and KeTimeIncrement through the loaded kernel's PE export table.
Ordinal reference: https://github.com/XboxDev/xbedump/blob/master/xboxkrnl.h
Ratios are diagnostics, not a claim that every timer interrupt must be delivered.
"""
import argparse
import hashlib
import subprocess
import json
import os
from pathlib import Path
import struct
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port, warmup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--frames', type=int, default=120)
    parser.add_argument('--warmup-frames', type=int, default=300)
    parser.add_argument('--same-state', action='store_true')
    parser.add_argument('--screenshots', action='store_true')
    parser.add_argument('--check-reset', action='store_true')
    parser.add_argument('--sample-on-failure', action='store_true')
    parser.add_argument('--display', action=argparse.BooleanOptionalAction, default=False)
    parser.add_argument('--sound', action=argparse.BooleanOptionalAction, default=False)
    parser.add_argument("--clock-shift", type=int, choices=range(4), default=0)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_XEMU_QUALIFICATION_SHIFT=str(args.clock_shift),
                         EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out / 'home')))
    binaries = {}
    for name in ("emucap", "emucap-mcp", "emucap-broker", "emucap-xemu-bridge"):
        with (ROOT / "target/release" / name).open("rb") as stream:
            binaries[name] = hashlib.file_digest(stream, "sha256").hexdigest()
    (out / "producer.json").write_text(json.dumps({
        "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "binaries": binaries}, indent=2))
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system': 'xbox', 'content_path': str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], 'start_frozen': True,
                                  'display': args.display, 'sound': args.sound})
        status = w.call('status')
        if 'clock_profile' in status:
            assert status['clock_profile']['instruction-ns'] == 1 << args.clock_shift
        else:
            assert args.clock_shift == 0, 'producer did not report the requested profile'
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        assert launch['display'] == args.display and launch['sound'] == args.sound, launch
        if 'execution_speed' in status['methods']:
            w.speed({'mode': 'unlimited'})
        warmup(w, status, args.warmup_frames)
        def read(address, length):
            return bytes.fromhex(w.call('read_memory', {'memory_type': 'main', 'address': address, 'length': length})['hex'])
        def u32(buf, offset):
            return struct.unpack_from('<I', buf, offset)[0]
        base = 0x10000
        header = read(base, 4096)
        assert header[:2] == b'MZ'
        pe = u32(header, 0x3c)
        assert header[pe:pe+4] == b'PE\0\0'
        exports = read(base + u32(header, pe + 24 + 96), 40)
        ordinal_base, count = struct.unpack_from('<II', exports, 16)
        functions = u32(exports, 28)
        def symbol(ordinal):
            assert ordinal_base <= ordinal < ordinal_base + count
            return base + u32(read(base + functions + 4 * (ordinal - ordinal_base), 4), 0)
        addresses = [symbol(156), symbol(157)]
        ranges = [{'memory_type': 'main', 'address': a, 'length': 4} for a in addresses]
        def sample():
            batch = w.call('read_memory_batch', {'ranges': ranges})
            clocks = {c['domain']: c['value'] for c in batch['boundary']['clocks']}
            return {'ticks': int.from_bytes(bytes.fromhex(batch['reads'][0]['hex']), 'little'),
                    'increment_100ns': int.from_bytes(bytes.fromhex(batch['reads'][1]['hex']), 'little'),
                    'clocks': clocks}
        checkpoint = str(out / 'clock-checkpoint.json')
        if args.same_state:
            w.call('save_state', {'path': checkpoint})
        if args.screenshots:
            w.call('screenshot', {'save_path': str(out / 'before.png')})
        rows = []
        restored = None
        restored_instructions = None
        restored_endpoint = None
        policies = ([{'mode': 'limited', 'percent': p} for p in (100, 50, 200, 400)]
                    + [{'mode': 'unlimited'}] if 'execution_speed' in status['methods']
                    else [{'mode': 'native'}])
        for policy in policies:
            if policy['mode'] != 'native':
                w.speed(policy)
            if args.same_state:
                w.call('load_state', {'path': checkpoint})
                if policy['mode'] != 'native':
                    assert w.speed({})['percent'] == policy.get('percent')
            before = sample()
            before_status = w.call('status')
            assert before_status.get('clock_profile') == status.get('clock_profile')
            scheduler_before = before_status.get('scheduler_diagnostics')
            if args.same_state and scheduler_before:
                if restored_instructions is None:
                    restored_instructions = scheduler_before['instructions']
                assert scheduler_before['instructions'] == restored_instructions
            if args.same_state:
                identity = (before['ticks'], before['increment_100ns'],
                            before['clocks']['qemu.virtual_ns'])
                if restored is None:
                    restored = identity
                assert identity == restored, 'load did not restore the same guest clock and ticks'
            time.sleep(.1)
            assert sample() == before, 'guest clock or RAM moved while frozen'
            if scheduler_before:
                assert w.call('status')['scheduler_diagnostics'] == scheduler_before
            start = time.monotonic()
            step = w.call('step', {'unit': 'frames', 'count': args.frames})
            elapsed = time.monotonic() - start
            assert step['status'] == 'completed', step
            after = sample()
            after_status = w.call('status')
            assert after_status.get('clock_profile') == status.get('clock_profile')
            scheduler_after = after_status.get('scheduler_diagnostics')
            if scheduler_before:
                assert scheduler_after['instructions'] >= scheduler_before['instructions']
                for key in ('apu_timer_calls', 'apu_retry_calls', 'apu_completed_quanta'):
                    assert scheduler_after[key] >= scheduler_before[key]
            virtual = after['clocks']['qemu.virtual_ns'] - before['clocks']['qemu.virtual_ns']
            ticks = (after['ticks'] - before['ticks']) & 0xffffffff
            if args.same_state and scheduler_after:
                endpoint = (after['clocks']['qemu.virtual_ns'],
                            scheduler_after['instructions'], after['ticks'])
                if restored_endpoint is None:
                    restored_endpoint = endpoint
                assert endpoint == restored_endpoint, ('rate-dependent stop boundary',
                                                       restored_endpoint, endpoint)

            rows.append({'policy': policy, 'before': before, 'after': after,
                         'host_seconds': elapsed, 'virtual_ns': virtual, 'ticks': ticks,
                         'tick_time_ratio': ticks * after['increment_100ns'] * 100 / virtual,
                         'scheduler_before': scheduler_before, 'scheduler_after': scheduler_after})
            (out / 'result.json').write_text(json.dumps(rows, indent=2))
        if args.screenshots:
            w.call('screenshot', {'save_path': str(out / 'after.png')})
        if args.check_reset:
            policy_before = w.speed({})
            w.call("reset")
            assert w.call("status").get("clock_profile") == status.get("clock_profile")
            assert w.speed({}) == policy_before
        print(json.dumps(rows, indent=2))
    except Exception:
        if args.sample_on_failure and launch and sys.platform == 'darwin':
            try:
                subprocess.run(['sample', str(launch['xemu_pid']), '1', '1',
                                '-file', str(out / 'failure.sample.txt')],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               timeout=10, check=False)
            except (OSError, subprocess.TimeoutExpired):
                pass
        raise
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
