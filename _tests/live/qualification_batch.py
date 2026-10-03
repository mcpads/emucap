#!/usr/bin/env python3
"""Run a reviewed profile matrix, retaining each check before continuing.

Manifest: {"rows": [{"id": "guest", "profile": "guest.json",
"checks": ["observation_speed", "input_pacing", "pacing_images"]}],
"artifacts": [{"path": "runtime/emucap-mcp.exe", "sha256": "..."}]}.
Paths are relative to the manifest. Profiles contain operator-owned launch inputs.
A resume requires identical manifest, profiles, harnesses and artifact bytes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
CHECKS = {name: HERE / (name + '.py') for name in
          ('observation_speed', 'input_pacing', 'pacing_images', 'control_restore', 'capture')}
CHECKS['pacing_counters'] = HERE / 'mesen2/pacing-counters.py'
CHECKS['openmsx_media'] = HERE / 'openmsx/media_change.py'


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def prepare(manifest_path):
    manifest = json.loads(manifest_path.read_text())
    inputs = {'manifest': sha(manifest_path)}
    for name in ('qualification_batch.py', 'observation_speed.py', 'input_pacing.py',
                 'pacing_images.py', 'control_restore.py', 'capture.py', 'mesen2/support.py', 'openmsx/media_change.py',
                 'openmsx/disk_state.py', 'mesen2/pacing-counters.py'):
        inputs['harness/' + name] = sha(HERE / name)
    seen = set()
    for row in manifest['rows']:
        name = row['id']
        if not re.fullmatch(r'[a-z0-9][a-z0-9_-]*', name) or name in seen:
            raise ValueError(f'invalid or repeated row: {name}')
        seen.add(name)
        checks = row['checks']
        if not checks or len(set(checks)) != len(checks) or any(c not in CHECKS for c in checks):
            raise ValueError(f'invalid checks for {name}')
        profile = (manifest_path.parent / row['profile']).resolve(strict=True)
        inputs['profile/' + name] = sha(profile)
        row['profile'] = str(profile)
    if not seen or not manifest.get('artifacts'):
        raise ValueError('batch needs guest rows and frozen runtime artifacts')
    for index, artifact in enumerate(manifest['artifacts']):
        path = (manifest_path.parent / artifact['path']).resolve(strict=True)
        actual = sha(path)
        if actual != artifact['sha256']:
            raise ValueError(f'changed runtime artifact: {path}')
        inputs[f'artifact/{index}'] = actual
    return manifest['rows'], inputs


def persist(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    # Windows readers can briefly deny replacement while the checkpoint uploader
    # reads the previous ledger. Keep atomic publication and retain the temporary
    # checkpoint if access remains denied.
    deadline = time.monotonic() + 2
    while True:
        try:
            temporary.replace(path)
            return
        except PermissionError as error:
            if getattr(error, 'winerror', None) not in (5, 32) or time.monotonic() >= deadline:
                raise
            time.sleep(0.01)


def run_check(script, profile, out):
    # Each existing witness owns its managed launch and stops it in finally.
    # Keep the witness's bounded MCP requests intact instead of killing its
    # parent on a separate wall-clock timeout and leaving a managed child alive.
    with out.with_suffix('.log').open('w', encoding='utf-8') as log:
        result = subprocess.run([sys.executable, str(script), '--profile', str(profile),
                                 '--output', str(out)], stdout=log, stderr=subprocess.STDOUT)
    result_path, stop_path = out / 'result.json', out / 'stop.json'
    record = json.loads(result_path.read_text()) if result_path.is_file() else {}
    stopped = json.loads(stop_path.read_text()) if stop_path.is_file() else None
    return {'exit_code': result.returncode,
            'passed': result.returncode == 0 and record.get('passed') is True
                      and isinstance(stopped, dict) and stopped.get('stopped') is True,
            'result': record, 'stop': stopped}


def execute(rows, inputs, output):
    output.mkdir(parents=True, exist_ok=True)
    ledger_path = output / 'batch.json'
    ledger = json.loads(ledger_path.read_text()) if ledger_path.exists() else {'inputs': inputs, 'checks': {}}
    if ledger['inputs'] != inputs:
        raise ValueError('batch inputs changed; use a new output directory')
    for row in rows:
        for check in row['checks']:
            key = row['id'] + '/' + check
            history = ledger['checks'].setdefault(key, [])
            if history and history[-1].get('passed'):
                continue
            parent = output / row['id'] / check
            parent.mkdir(parents=True, exist_ok=True)
            attempt = parent / f'attempt-{len(history) + 1}'
            if attempt.exists() or attempt.with_suffix('.log').exists():
                raise ValueError(f'unrecorded attempt needs inspection: {attempt}')
            started = time.time()
            try:
                result = run_check(CHECKS[check], row['profile'], attempt)
            except Exception as error:
                result = {'passed': False, 'error': str(error)}
            result.update(started=started, seconds=time.time() - started, output=str(attempt))
            history.append(result)
            persist(ledger_path, ledger)
            print(json.dumps({'check': key, 'passed': result['passed'], 'seconds': result['seconds']}), flush=True)
    ledger['passed'] = all(history[-1]['passed'] for history in ledger['checks'].values())
    persist(ledger_path, ledger)
    return ledger['passed']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    rows, inputs = prepare(args.manifest.resolve(strict=True))
    raise SystemExit(0 if execute(rows, inputs, args.output.resolve()) else 1)


if __name__ == '__main__':
    main()
