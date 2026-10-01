#!/usr/bin/env python3
"""Opt-in APU pending-voice snapshot witness using native frozen continuation observations.

Requires clang and llvm-objcopy. All fixture mutations stay within one disposable managed guest.
"""
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

def audio_state(w, out, name):
    before = w.call('status')
    assert before['state'] == 'frozen'
    view = before.get('apu_continuation')
    assert isinstance(view, dict), 'native audio continuation observation unavailable'
    after = w.call('status')
    assert after['scheduler_diagnostics'] == before['scheduler_diagnostics']
    assert after['apu_continuation'] == view
    (out / f'{name}.json').write_text(json.dumps(view, indent=2))
    return view


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    states = out / 'states'
    states.mkdir()
    patches = out / 'native-patches'
    patches.mkdir()
    for patch in (ROOT / 'adapters/xemu/patches').glob('*.patch'):
        shutil.copyfile(patch, patches / patch.name)
    (out / 'source.diff').write_bytes(subprocess.check_output(['git', 'diff', 'HEAD'], cwd=ROOT))
    for name in ('apu_pending.py', 'apu_pending.S'):
        shutil.copyfile(Path(__file__).with_name(name), out / name)
    binaries = {}
    for name in ('emucap', 'emucap-mcp', 'emucap-broker', 'emucap-xemu-bridge'):
        with (ROOT / 'target/release' / name).open('rb') as stream:
            binaries[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    with args.content.open('rb') as stream:
        content_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    (out / 'producer.json').write_text(json.dumps({'binaries': binaries,
        'content_sha256': content_digest, 'revision': subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()}, indent=2))
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
        EMUCAP_XEMU_QUALIFICATION_SHIFT='3', EMUCAP_PORT=str(free_port()),
        EMUCAP_EMU_HOME=str(out / 'home')))
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
        warmup(w, status, 2400)
        # Prove that native observations are available before guest fixture mutation.
        audio_state(w, out, 'initial')
        cpu = w.call('get_state', {'groups': ['cpu']})['state']
        assert cpu['cpu.cs'] & 3 == 0
        pc, scratch = cpu['cpu.eip'], (cpu['cpu.esp'] - 1024) & ~15
        w.call('read_memory', {'memory_type': 'cpu', 'address': pc, 'length': 516})
        w.call('read_memory', {'memory_type': 'cpu', 'address': scratch, 'length': 4})
        origin = str(states / 'origin.json')
        w.call('save_state', {'path': origin})
        assembly = f'.set SCRATCH, {scratch}\n' + (out / 'apu_pending.S').read_text()
        (out / 'resolved.S').write_text(assembly)
        subprocess.run(['clang', '-target', 'i386-none-elf', '-x', 'assembler', '-c',
                        '-o', str(out / 'fixture.o'), '-'], input=assembly, text=True, check=True)
        subprocess.run(['llvm-objcopy', '-O', 'binary', '--only-section=.text',
                        str(out / 'fixture.o'), str(out / 'fixture.bin')], check=True)
        w.call('write_memory', {'memory_type': 'cpu', 'address': pc,
                               'hex': (out / 'fixture.bin').read_bytes().hex()})
        bp = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'cpu',
                   'start': pc+256, 'end': pc+256, 'pause_on_hit': True})
        step = w.call('step', {'unit': 'frames', 'count': 10})
        assert step['status'] == 'interrupted', step
        assert w.call('get_state', {'groups': ['cpu']})['state']['cpu.eip'] == pc+256
        pending = audio_state(w, out, 'pending')
        assert pending['pending'] and pending['queue-length'] > 0 and pending['samples-sha256'] == hashlib.sha256(bytes(256)).hexdigest(), pending
        assert pending['locks-sha256'] == hashlib.sha256(bytes([255])*32).hexdigest(), pending
        checkpoint = str(states / 'pending.json')
        w.call('save_state', {'path': checkpoint})
        w.call('clear_breakpoint', {'id': bp['id']})
        bp = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'cpu',
                   'start': pc+512, 'end': pc+512, 'pause_on_hit': True})
        rows, reference = [], None
        for index, policy in enumerate([{'mode': 'limited', 'percent': p} for p in
                (1, 50, 100, 200, 400, 1000)] + [{'mode': 'unlimited'}]):
            w.call('load_state', {'path': checkpoint})
            restored = audio_state(w, out, f'restored-{index}')
            assert restored == pending, ('pending queue or phase changed', pending, restored)
            w.speed(policy)
            before = w.call('status')['scheduler_diagnostics']
            step = w.call('step', {'unit': 'frames', 'count': 10})
            assert step['status'] == 'interrupted', step
            after = w.call('status')['scheduler_diagnostics']
            state = w.call('get_state', {'groups': ['cpu']})['state']
            assert state['cpu.eip'] == pc+512
            completed = audio_state(w, out, f'completed-{index}')
            assert not completed['pending'] and completed['queue-length'] == 0 and completed['locks-sha256'] == hashlib.sha256(bytes(32)).hexdigest()
            delta = {key: after[key]-before[key] for key in before}
            advance = ((completed['deadline'] - pending['deadline']) * 48000
                       + completed['fraction'] - pending['fraction'])
            assert advance > 0 and advance % 32000000000 == 0
            assert delta['apu_completed_quanta'] == advance // 32000000000, delta
            assert completed['ep-frame-div'] == (pending['ep-frame-div'] + delta['apu_completed_quanta']) % 8
            observed = {'cpu': state, 'delta': delta, 'native': completed}
            if reference is None:
                reference = observed
            assert observed == reference, ('pacing changed resumed audio work', policy)
            rows.append({'policy': policy, **observed})
            (out / 'result.json').write_text(json.dumps({'pending': pending, 'rows': rows}, indent=2))
        w.call('clear_breakpoint', {'id': bp['id']})
        w.call('load_state', {'path': origin})
        assert w.call('get_state', {'groups': ['cpu']})['state'] == cpu
        print(json.dumps({'passed': True, 'policies': len(rows), 'pending_voices': pending['queue-length']}))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
