#!/usr/bin/env python3
"""Opt-in direct Mesen ownership/EOF witness; user supplies private launch content."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time

from support import ROOT, LAUNCHER, Session, default_binary, require_ok, terminate_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    system = profile['launch_plan']['system']
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    home = out / 'home'
    listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(5)
    port = listener.getsockname()[1]
    runtime = 'native-owned-control-witness'
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_EMU_HOME=str(home), MESEN_BIN=str(default_binary()),
               EMUCAP_LAUNCH_ID=runtime, EMUCAP_START_FROZEN='1',
               EMUCAP_LAUNCH_WAIT='10', EMUCAP_POST_CONNECT_GRACE='0',
               EMUCAP_LOG=str(out / 'launch.log'))
    session, pid, fingerprint = None, None, None
    rows = []

    def call(method, params=None):
        result = require_ok(session.request(method, params), method)
        rows.append(dict(method=method, params=params, result=result))
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        return result

    def reconnect():
        nonlocal session
        session.close()
        session = Session(listener.accept()[0])
        assert call('hello')['control_session_lifecycle'] is True
        return call('status')

    def parent(name):
        return dict(runtime=runtime, owner_id='witness', operation_id=name)

    try:
        launch = subprocess.run(['bash', str(LAUNCHER), profile['launch_plan']['content_path'],
                                 str(port), runtime, system], env=env,
                                capture_output=True, text=True, timeout=30)
        (out / 'launch-output.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mesen2' / str(port) / 'mesen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0])
        assert call('hello')['control_session_lifecycle'] is True
        call('pause'); assert call('status')['state'] == 'frozen'
        call('execution_speed', {'mode': 'unlimited'})

        call('set_input', {'buttons': ['a']})
        assert reconnect()['input_override']['engaged'], 'unscoped input lost on EOF'
        call('set_input', {'buttons': []})

        key = parent('between-phases')
        call('begin_temporal_operation', {'parent': key})
        call('set_input', {'buttons': ['a'], '_temporal_owner': key})
        state = reconnect()
        assert state['state'] == 'frozen' and not state['input_override']['engaged']

        call('set_input', {'buttons': ['b']})
        key = parent('step-only')
        call('begin_temporal_operation', {'parent': key})
        result = call('step', {'frames': 1, '_temporal_owner': key, '_control': parent('child')})
        assert result['status'] == 'completed' and result['count'] == 1
        assert reconnect()['input_override']['engaged'], 'step-only parent released unrelated input'
        call('set_input', {'buttons': []})

        key = parent('oversized-traffic')
        call('begin_temporal_operation', {'parent': key})
        call('set_input', {'buttons': ['a'], '_temporal_owner': key})
        started = time.monotonic()
        session.socket.sendall(b'x' * 4097)
        assert session.file.readline() == b'', 'oversized owned frame did not close transport'
        state = reconnect()
        elapsed = time.monotonic() - started
        assert elapsed < 2 and not state['input_override']['engaged'] and state['state'] == 'frozen'

        key = parent('finish-retry')
        call('begin_temporal_operation', {'parent': key})
        call('set_input', {'buttons': ['a'], '_temporal_owner': key})
        first = call('finish_temporal_operation', {'parent': key})
        assert first['cleanup_verified'] and first['released_ports'] == [0]
        assert call('finish_temporal_operation', {'parent': key}) == first
        key = parent('selection-and-late-cancel')
        call('begin_temporal_operation', {'parent': key})
        rejected = session.request('step', {'frames': 1, 'cpu': 'not-a-cpu',
            '_temporal_owner': key, '_control': parent('invalid-target')})
        assert rejected['ok'] is False
        call('set_input', {'buttons': ['a'], '_temporal_owner': key})
        assert call('cancel_operation', key)['status'] == 'not_active'
        call('set_input', {'buttons': [], '_temporal_owner': key})
        result = call('step_instructions', {'count': 3.0,
            '_temporal_owner': key, '_control': parent('instruction-child')})
        assert result['status'] == 'completed' and result['count'] == 3
        assert call('finish_temporal_operation', {'parent': key})['cleanup_verified']

        # A completed pause has its own stop budget, independent of later work.
        call('resume')
        key = parent('initial-pause-budget')
        call('begin_temporal_operation', {'parent': key})
        call('pause', {'_temporal_owner': key})
        time.sleep(5.1)
        result = call('step', {'frames': 1, '_temporal_owner': key, '_control': parent('later-child')})
        assert result['status'] == 'completed'
        assert call('finish_temporal_operation', {'parent': key})['cleanup_verified']

        call('execution_speed', {'mode': 'limited', 'percent': 100})
        files = list((ROOT / 'adapters/mesen2').glob('*.lua'))
        files += [p for p in home.rglob('*') if p.is_file() and p.name in ('Mesen', 'Mesen.dll', 'MesenCore.dylib')]
        (out / 'artifacts.json').write_text(json.dumps({str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}, indent=2))
        (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
            launch_route='compatibility-shell', profile_launch_overrides_applied=False,
            oversized_loss_cleanup_seconds=elapsed), indent=2))
    finally:
        if session: session.close()
        listener.close()
        if pid:
            actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
            if actual.returncode == 0:
                assert actual.stdout == fingerprint if fingerprint else str(home) in actual.stdout
                terminate_owned(pid)
            absent = subprocess.run(['ps', '-p', str(pid)], capture_output=True).returncode != 0
            (out / 'exit.json').write_text(json.dumps(dict(pid=pid, exit_verified=absent)))
            assert absent


if __name__ == '__main__':
    main()
