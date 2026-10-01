#!/usr/bin/env python3
"""Qualify Xbox frozen batches through public MCP in an isolated managed generation."""
import argparse
import json
import os
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, check_batch, free_port, warmup


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
        warmup(w, status, 300)
        ranges = [{'memory_type': 'main', 'address': a, 'length': n}
                  for a, n in [(0x10000, 64), (0x10004, 16), (0x10000, 64)]]
        rejected = [{'memory_type': 'main', 'address': 0x4000000 - 1, 'length': 2},
                    {'memory_type': 'cpu', 'address': 0x80010000, 'length': 4}]
        result = check_batch(w, ranges, rejected, ['cpu'])
        before = w.call('read_memory_batch', {'ranges': ranges})
        time.sleep(.2)
        assert w.call('read_memory_batch', {'ranges': ranges}) == before
        assert before['reads'][0]['hex'] == before['reads'][2]['hex']
        full = w.call('read_memory_batch', {'ranges': [dict(ranges[0], length=65536)]})
        assert full['total_bytes'] == 65536
        w.call('resume')
        w.call('read_memory_batch', {'ranges': ranges}, error=True)
        w.call('pause')
        assert w.call('read_memory_batch', {'ranges': ranges})['boundary'] != before['boundary']
        state = str(out / 'checkpoint.json')
        w.call('save_state', {'path': state})
        saved = w.call('read_memory_batch', {'ranges': ranges})
        w.call('step', {'unit': 'frames', 'count': 1})
        w.call('load_state', {'path': state})
        restored = w.call('read_memory_batch', {'ranges': ranges})
        assert restored['reads'] == saved['reads']
        assert restored['boundary']['stop_epoch'] != saved['boundary']['stop_epoch']
        result.update({'passed': True, 'load_preserved_bytes': True,
                       'advertised_methods': status['methods']})
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    finally:
        try:
            if launch:
                w.call('stop', {'launch_id': launch['launch_id']})
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
