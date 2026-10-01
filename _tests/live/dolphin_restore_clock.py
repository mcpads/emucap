#!/usr/bin/env python3
"""Compare Dolphin instruction continuation across fresh and previously stepped runtimes.

Provide a private launch profile and an existing instruction-boundary checkpoint. An optional
rejected checkpoint exercises native failure/undo before the valid checkpoint is loaded. Use a
late decode failure to cover post-device restoration; an unreadable file does not cover that path.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

from observation_speed import ROOT, Witness, free_port


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--checkpoint', type=Path, required=True)
    parser.add_argument('--rejected-checkpoint', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--generations', type=int, default=3)
    parser.add_argument('--cycles', type=int, default=3)
    parser.add_argument('--instructions', type=int, default=101)
    args = parser.parse_args()
    if min(args.generations, args.cycles, args.instructions) < 1:
        parser.error('generations, cycles and instructions must be positive')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    profile = json.loads(args.profile.read_text())
    checkpoint = args.checkpoint.resolve(strict=True)
    rejected = args.rejected_checkpoint.resolve(strict=True) if args.rejected_checkpoint else None
    artifacts = [checkpoint, args.profile.resolve(strict=True), Path(__file__).resolve(),
                 ROOT / 'target/release/emucap-mcp']
    if rejected:
        artifacts.append(rejected)
    hashes = {str(p): digest(p) for p in artifacts}
    producer = {'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT,
                                                   text=True).strip(), 'sha256': hashes}
    reference = None
    results = []
    for generation in range(args.generations):
        for preload in (0, 1):
            dest = out / f'{generation}-{preload}'
            dest.mkdir()
            w = Witness(dest, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT),
                                   EMUCAP_EMU_HOME=str(dest / 'home'),
                                   EMUCAP_PORT=str(free_port())))
            launch = None
            try:
                w.process.initialize()
                plan = w.call('launch_plan', profile['launch_plan'])
                launch = w.call('launch', {**plan['preferred_launcher']['args'],
                                          **profile.get('launch', {})})
                assert launch['adapter'] == 'dolphin', launch
                native = launch['binary']
                native_hash = digest(native)
                if native in hashes:
                    assert hashes[native] == native_hash, 'producer changed between generations'
                hashes[native] = native_hash
                (out / 'producer.json').write_text(json.dumps(producer, indent=2))
                w.call('pause')
                w.call('status')
                w.speed({'mode': 'unlimited'})
                if preload:
                    step = w.call('step', {'unit': 'instructions', 'count': preload})
                    assert step['status'] == 'completed' and step['count'] == preload, step
                if rejected:
                    before = w.call('get_state', {'groups': ['cpu']})
                    for _ in range(3):
                        w.call('load_state', {'path': str(rejected)}, error=True)
                        assert w.call('get_state', {'groups': ['cpu']}) == before
                        assert w.call('status')['state'] == 'frozen'
                for cycle in range(args.cycles):
                    w.call('load_state', {'path': str(checkpoint)})
                    # Exercise the single-instruction completion event and a subsequent batch.
                    for count in (1, args.instructions - 1):
                        if count:
                            step = w.call('step', {'unit': 'instructions', 'count': count})
                            assert step['status'] == 'completed' and step['count'] == count, step
                    cpu = w.call('get_state', {'groups': ['cpu']})
                    if reference is None:
                        reference = cpu
                    assert cpu == reference, (generation, preload, cycle, reference, cpu)
                results.append({'launch': launch, 'preload_instructions': preload,
                                'cycles': args.cycles, 'instructions_per_cycle': args.instructions})
            finally:
                try:
                    if launch:
                        stopped = w.call('stop', {'launch_id': launch['launch_id']})
                        assert stopped['stopped'], stopped
                finally:
                    w.process.close()
    assert all(digest(p) == value for p, value in hashes.items()), 'producer or checkpoint changed'
    result = {'passed': True, 'runs': results, 'reference_cpu': reference}
    (out / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps({'passed': True, 'runs': len(results)}))


if __name__ == '__main__':
    main()
