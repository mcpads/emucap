#!/usr/bin/env python3
"""Compare owned Xbox boot milestones; frame domains must be interpreted per producer.

Each run records CPU, kernel ticks, virtual time, host duration, and screenshots. Native display
frames and instruction-clock vblanks are different boundaries, not equivalent work counters.
"""
import argparse
import hashlib
import json
import os
import shutil
from pathlib import Path
import struct
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
    parser.add_argument('--producer-root', type=Path, default=ROOT)
    parser.add_argument('--milestones', type=int, nargs='+', default=[300, 900, 1800])
    parser.add_argument('--diagnostics', action='store_true')
    parser.add_argument('--menu-input', action='store_true',
                        help='After the final observed menu: down/up, restore, then accept')
    parser.add_argument('--percent', type=int,
                        help='Limited pacing percent; omitted means unlimited on capable hosts')
    parser.add_argument("--clock-shift", type=int, choices=range(4), default=0)
    args = parser.parse_args()
    if args.percent is not None and not 1 <= args.percent <= 1000:
        parser.error("--percent must be between 1 and 1000")
    assert args.milestones == sorted(set(args.milestones)) and args.milestones[0] > 0
    producer = args.producer_root.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(__file__, out / 'boot_progress.py')
    (out / 'source.diff').write_bytes(subprocess.check_output(
        ['git', 'diff', 'HEAD'], cwd=producer))
    patch_copy = out / 'native-patches'
    patch_copy.mkdir()
    for patch in (producer / 'adapters/xemu/patches').glob('*.patch'):
        shutil.copyfile(patch, patch_copy / patch.name)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(producer),
               EMUCAP_XEMU_QUALIFICATION_SHIFT=str(args.clock_shift), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'),
               EMUCAP_XEMU_BIN=str(producer / 'adapters/xemu/work/xemu/dist/xemu.app/Contents/MacOS/xemu'),
               EMUCAP_XEMU_BRIDGE_BIN=str(producer / 'target/release/emucap-xemu-bridge'))
    w = Witness(out, env, producer / 'target/release/emucap-mcp')
    artifacts = {}
    for name in ('emucap', 'emucap-mcp', 'emucap-broker', 'emucap-xemu-bridge'):
        binary = producer / 'target/release' / name
        with binary.open('rb') as stream:
            artifacts[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    with args.content.open('rb') as stream:
        content_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    content_stat = args.content.stat()
    provenance = {'content_sha256': content_digest,
        'content_size': content_stat.st_size, 'revision': subprocess.check_output(
        ['git', '-C', str(producer), 'rev-parse', 'HEAD'], text=True).strip(),
        'binaries': artifacts}
    (out / 'producer.json').write_text(json.dumps(provenance, indent=2))
    launch = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', {'system': 'xbox', 'content_path': str(args.content.resolve())})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'], 'start_frozen': True})
        status = w.call('status')
        if 'clock_profile' in status:
            assert status['clock_profile']['instruction-ns'] == 1 << args.clock_shift
        else:
            assert args.clock_shift == 0, 'producer did not report the requested profile'
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status}, indent=2))
        policy = 'native'
        if 'execution_speed' in status['methods']:
            requested = ({'mode': 'limited', 'percent': args.percent} if args.percent
                         else {'mode': 'unlimited'})
            w.speed(requested)
            policy = requested
        rows, previous, host_total, addresses = [], 0, 0, None
        def read(address, length):
            return bytes.fromhex(w.call('read_memory', {'memory_type': 'main', 'address': address,
                                                       'length': length})['hex'])
        def u32(data, offset=0):
            return struct.unpack_from('<I', data, offset)[0]
        for frame in args.milestones:
            started = time.monotonic()
            warmup(w, status, frame - previous)
            elapsed = time.monotonic() - started
            host_total += elapsed
            if addresses is None:
                base = 0x10000
                header = read(base, 4096)
                pe = u32(header, 0x3c)
                exports = read(base + u32(header, pe + 24 + 96), 40)
                ordinal_base, count = struct.unpack_from('<II', exports, 16)
                functions = u32(exports, 28)
                addresses = [base + u32(read(base + functions + 4 * (o - ordinal_base), 4))
                             for o in (156, 157)]
            batch = w.call('read_memory_batch', {'ranges': [
                {'memory_type': 'main', 'address': a, 'length': 4} for a in addresses]})
            state = w.call('get_state', {'groups': ['cpu']})
            w.call('screenshot', {'save_path': str(out / f'frame-{frame}.png')})
            assert w.call('get_state', {'groups': ['cpu']}) == state, 'capture moved CPU state'
            observed = w.call('read_memory_batch', {'ranges': [
                {'memory_type': 'main', 'address': a, 'length': 4} for a in addresses]})
            assert observed == batch, 'capture changed frozen clocks or kernel tick bytes'
            row = {'frame': frame, 'policy': policy, 'interval_host_seconds': elapsed,
                   'total_host_seconds': host_total, 'boundary': batch['boundary'],
                   'ticks': u32(bytes.fromhex(batch['reads'][0]['hex'])),
                   'increment_100ns': u32(bytes.fromhex(batch['reads'][1]['hex'])), 'cpu': state}
            if args.diagnostics:
                row['scheduler'] = w.call('status').get('scheduler_diagnostics')
                row['code'] = w.call('disassemble', {
                    'address': state['state']['cpu.eip'], 'count': 12})
            rows.append(row)
            (out / 'result.json').write_text(json.dumps(rows, indent=2))
            print(json.dumps(row), flush=True)
            previous = frame
        current_stat = args.content.stat()
        assert (current_stat.st_size, current_stat.st_mtime_ns) == (
            content_stat.st_size, content_stat.st_mtime_ns), 'content changed during trial'
        if args.menu_input:
            checkpoint = str(out / 'menu.json')
            w.call('save_state', {'path': checkpoint})
            for button in ('down', 'up'):
                result = w.call('tap', {'buttons': [button], 'press_frames': 6, 'after_frames': 30})
                assert result['state'] == 'frozen' and result['press_frames'] == 6, result
                w.call('screenshot', {'save_path': str(out / f'menu-{button}.png')})
            w.call('load_state', {'path': checkpoint})
            w.call('screenshot', {'save_path': str(out / 'menu-restored.png')})
            result = w.call('tap', {'buttons': ['a'], 'press_frames': 6, 'after_frames': 120})
            assert result['state'] == 'frozen' and result['press_frames'] == 6, result
            w.call('screenshot', {'save_path': str(out / 'menu-accepted.png')})
    except Exception:
        if launch and sys.platform == 'darwin' and shutil.which('sample'):
            # Preserve the owned host's stack before finally stops this generation.
            with (out / 'failure-sample.log').open('w') as diagnostic:
                try:
                    subprocess.run(['sample', str(launch['pid']), '1', '-file',
                                    str(out / 'failure-sample.txt')], stdout=diagnostic,
                                   stderr=subprocess.STDOUT, timeout=10, check=False)
                except subprocess.TimeoutExpired:
                    diagnostic.write('sample command exceeded ten seconds\n')
        raise
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
