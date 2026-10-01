#!/usr/bin/env python3
"""Check native presented-frame continuation from identical Dolphin checkpoint bytes.

The private profile supplies launch_plan, launch overrides, optional dolphin_config INI text,
optional dolphin_user_files mapping relative user paths to owned source files, and optional paces.
expected_runtime_components may specify the native component fields this profile must observe.
Keep pre-first-XFB checkpoints intact: this witness never advances to normalize their origin.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

from observation_speed import ROOT, Witness, free_port


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        while block := f.read(1048576):
            h.update(block)
    return h.hexdigest()


def runtime_binary(launch):
    source = Path(launch['binary'])
    runtime = Path(launch['emucap_home']) / 'runtime'
    bundle = next((p for p in source.parents if p.suffix == '.app'), None)
    return runtime / bundle.name / source.relative_to(bundle) if bundle else runtime / source.name


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--checkpoint', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=4)
    parser.add_argument('--frames', type=int, default=1)
    args = parser.parse_args()
    if args.repeats < 2 or not 1 <= args.frames <= 5000:
        parser.error('repeats must be at least 2 and frames must be in 1..5000')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    profile = json.loads(args.profile.read_text())
    checkpoint = args.checkpoint.resolve(strict=True)
    port = free_port()
    home = out / 'home'
    user = home / 'dolphin' / str(port) / 'user'
    inputs = [checkpoint, args.profile.resolve(strict=True),
              Path(profile['launch_plan']['content_path']).resolve(strict=True), Path(__file__).resolve(),
              ROOT / 'target/release/emucap-mcp', ROOT / 'adapters/dolphin/upstream.lock']
    if 'dolphin_config' in profile:
        (user / 'Config').mkdir(parents=True)
        (user / 'Config/Dolphin.ini').write_text(profile['dolphin_config'])
    for relative, source in profile.get('dolphin_user_files', {}).items():
        relative = Path(relative)
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('dolphin_user_files destinations must stay inside the owned user tree')
        source = Path(source).resolve(strict=True)
        target = user / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        inputs.append(source)
    hashes = {str(p): digest(p) for p in inputs}
    manifest = {'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT,
                                                   text=True).strip(), 'sha256': hashes}
    (out / 'producer.json').write_text(json.dumps(manifest, indent=2))
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                         EMUCAP_EMU_HOME=str(home), EMUCAP_PORT=str(port)))
    launch = None
    runs = []
    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan'])
        w.call('status')
        launch = w.call('launch', {**plan['preferred_launcher']['args'],
                                  **profile.get('launch', {})})
        assert launch.get('launched') and launch['adapter'] == 'dolphin', launch
        native = Path(launch['binary'])
        copied = runtime_binary(launch)
        assert digest(native) == digest(copied), 'runtime binary differs from selected producer'
        hashes.update({str(native): digest(native), str(copied): digest(copied)})
        (out / 'producer.json').write_text(json.dumps(manifest, indent=2))
        w.call('pause')
        status = w.call('status')
        cap = status['memory_batch_capability']
        windows = cap['windows'][:cap['max_ranges']]
        length = min(16384, cap['max_range_bytes'], cap['max_total_bytes'] // len(windows))
        ranges = [{'memory_type': x['memory_type'], 'address': x['address'],
                   'length': min(length, x['length'])} for x in windows]
        policies = [{'mode': 'limited', 'percent': 100}] * args.repeats
        policies += profile.get('paces', [{'mode': 'limited', 'percent': 50},
                                         {'mode': 'limited', 'percent': 400}, {'mode': 'unlimited'}])
        for policy in policies:
            w.call('load_state', {'path': str(checkpoint)})
            before = w.call('status')
            w.revision = None
            w.speed(policy)
            cpu_before = w.call('get_state', {'groups': ['cpu']})
            components = cpu_before['runtime_components']
            assert components is not None, cpu_before
            for key, expected in profile.get('expected_runtime_components', {}).items():
                assert components[key] == expected, components
            assert w.call('get_state', {'groups': ['cpu']}) == cpu_before
            assert w.call('status')['frame'] == before['frame']
            step = w.call('step', {'unit': 'frames', 'count': args.frames})
            elapsed = w.seconds()
            assert step['status'] == 'completed' and step['count'] == args.frames, step
            cpu = w.call('get_state', {'groups': ['cpu']})
            after = w.call('status')
            assert after['state'] == 'frozen', after
            memory = w.call('read_memory_batch', {'ranges': ranges})
            value = hashlib.sha256(''.join(r['hex'] for r in memory['reads']).encode()).hexdigest()
            run = {'policy': policy, 'seconds': elapsed, 'cpu_before': cpu_before, 'cpu': cpu,
                   'vi_delta': after['frame'] - before['frame'], 'memory_sha256': value}
            runs.append(run)
            (out / 'runs.json').write_text(json.dumps(runs, indent=2))
            assert all(run[k] == runs[0][k] for k in ('cpu_before', 'cpu', 'vi_delta', 'memory_sha256')), runs
        w.call('resume')
        running = w.call('get_state', {'groups': ['cpu']})
        assert running['runtime_components'] == components, running
        assert w.call('status')['state'] == 'running'
        w.call('pause')
    finally:
        try:
            if launch and launch.get('launched'):
                assert w.call('stop', {'launch_id': launch['launch_id']})['stopped']
        finally:
            w.process.close()
    try:
        os.kill(launch['pid'], 0)
        raise AssertionError('owned emulator is still alive')
    except ProcessLookupError:
        pass
    assert all(digest(p) == value for p, value in hashes.items()), 'bound input changed'
    (out / 'result.json').write_text(json.dumps({'passed': True, 'runs': runs, 'ranges': ranges,
                                               'launch': launch, 'exited': True}, indent=2))
    print(json.dumps({'passed': True, 'runs': len(runs)}))


if __name__ == '__main__':
    main()
