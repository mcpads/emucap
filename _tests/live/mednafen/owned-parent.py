#!/usr/bin/env python3
"""Opt-in native parent ownership, input cleanup and transport identity witness."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import Session, terminate_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cpu-park', action='store_true')
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = ROOT / 'adapters/mednafen/work/mednafen/src/mednafen'
    listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(15)
    port = listener.getsockname()[1]; home = out / 'home'
    runtime = 'native-parent-' + uuid.uuid4().hex
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_EMU_HOME=str(home), EMUCAP_START_FROZEN='1', EMUCAP_HEADLESS='1',
               MEDNAFEN_SOUND='0', EMUCAP_POST_CONNECT_GRACE='0', EMUCAP_LAUNCH_ID=runtime)
    session = None; pid = None; fingerprint = None; rows = []; attachment = None
    origin_frame = None; origin_native = None
    system = profile['launch_plan']['system']; module = 'ss' if system == 'saturn' else system
    a, b = ('cross', 'circle') if system == 'psx' else ('a', 'b')

    def call(method, params=None, ok=True, stamp=True):
        params = dict(params or {})
        if stamp and attachment is not None: params['_control_attachment'] = attachment
        response = session.request(method, params)
        rows.append(dict(method=method, params=params, response=response))
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        assert response['ok'] == ok, response
        return response.get('result', response.get('error'))

    def parent(name):
        return dict(runtime=runtime, owner_id='witness', operation_id=name)

    def begin(key):
        assert call('begin_temporal_operation', {'parent': key}) == dict(status='admitted', parent=key)

    def set_input(button, key=None):
        params = {'buttons': [button] if button else []}
        if key: params['_temporal_owner'] = key
        call('set_input', params)

    def status(mask=None):
        nonlocal origin_frame, origin_native
        state = call('status')
        if origin_frame is None:
            origin_frame = state['frame']; origin_native = state['native_control']
        assert state['state'] == 'frozen' and state['frame'] == origin_frame, state
        assert state['native_control'] == origin_native, state
        assert state['native_control']['parked'], state
        assert state['native_control']['context'] == ('cpu_callback' if args.cpu_park else 'frame_boundary'), state
        if mask is not None: assert state['native_input_mask'] == mask, state
        return state['native_input_mask']

    def event(kind, value):
        data = {'_control_session': dict(kind=kind, runtime=runtime, attachment=value)}
        session.socket.sendall(json.dumps(data).encode() + b'\n')
        rows.append(dict(event=data))

    def reconnect():
        nonlocal session, attachment
        session.close(); session = Session(listener.accept()[0]); attachment = None
        call('hello')

    try:
        launch = subprocess.run(['bash', str(ROOT / 'adapters/mednafen/launch.sh'),
                                 profile['launch_plan']['content_path'], str(port), 'native-parent', module],
                                env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mednafen' / str(port) / 'mednafen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0]); identity = call('hello')
        assert identity.get('control_session_lifecycle') is True
        assert identity['temporal_cancellation_capability'] == dict(
            methods=['step', 'step_instructions'], control_service_ms=50, stop_host_ms=5000)
        (out / 'artifacts.json').write_text(json.dumps(dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
            hello=identity, sidecar=json.loads(Path(str(binary) + '.emucap-build.json').read_text())), indent=2))
        if args.cpu_park: call('step_instructions', {'count': 1})
        set_input(b); held_b = status(); assert held_b
        key = parent('no-effects'); begin(key)
        terminal = call('finish_temporal_operation', {'parent': key})
        assert terminal['cleanup_verified'] and terminal['released_ports'] == [] and not terminal['effects_started']
        status(held_b)
        key = parent('pause-only'); begin(key)
        call('pause', {'_temporal_owner': key})
        terminal = call('finish_temporal_operation', {'parent': key})
        assert terminal['cleanup_verified'] and terminal['released_ports'] == [] and terminal['effects_started']
        status(held_b)
        key = parent('invalid-input'); begin(key)
        for bad in ({'buttons': [1]}, {'port': 1, 'buttons': [a]}):
            call('set_input', dict(bad, _temporal_owner=key), ok=False)
        terminal = call('finish_temporal_operation', {'parent': key})
        assert terminal['released_ports'] == [] and not terminal['effects_started']; status(held_b)
        key = parent('finish'); begin(key); set_input(a, key); held_a = status(); assert held_a and held_a != held_b
        for method, params in [('resume', {}), ('reset', {}), ('step', {'frames': 1}),
                               ('set_input', {'buttons': []}), ('execution_speed', {'mode': 'unlimited'})]:
            assert call(method, params, ok=False)['kind'] == 'busy'; status(held_a)
        child = dict(key, operation_id='child')
        call('step', {'frames': 0, '_temporal_owner': key, '_control': child}, ok=False); status(held_a)
        assert call('cancel_operation', key)['status'] == 'not_active'; status(held_a)
        session.next_id = (1 << 64) - 1
        terminal = call('finish_temporal_operation', {'parent': key}); session.next_id = 100
        assert terminal['cleanup_verified'] and terminal['released_ports'] == [0]; status(0)
        assert call('finish_temporal_operation', {'parent': key}) == terminal
        replacement = parent('replacement'); begin(replacement); set_input(b, replacement)
        call('finish_temporal_operation', {'parent': key}, ok=False); status(held_b)
        call('finish_temporal_operation', {'parent': replacement}); status(0)
        set_input(b); reconnect(); status(held_b)
        key = parent('no-effect-eof'); begin(key); reconnect(); status(held_b)
        key = parent('input-eof'); begin(key); set_input(a, key); reconnect(); status(0)
        key = parent('oversize'); begin(key); set_input(a, key)
        session.socket.sendall(b'x' * 4097)
        assert session.file.readline() == b''
        reconnect(); status(0)
        key = parent('unread-replies'); begin(key); set_input(a, key)
        session.socket.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
        requests = b''.join((json.dumps(dict(v=1, id=1000+i, method='status', params={})).encode() + b'\n') for i in range(512))
        started = time.monotonic(); session.socket.sendall(requests); session.socket.shutdown(socket.SHUT_WR)
        old = session; session = Session(listener.accept()[0]); old.close()
        elapsed = time.monotonic() - started
        assert elapsed < 5, elapsed
        call('hello'); status(0)
        # Broker bootstrap, exact full-width attachment, stale detach and stale stamp.
        attachment = dict(broker_instance='fixture-broker', registration=(1 << 64)-1, session=(1 << 53)+1)
        event('attach', attachment); call('hello')
        key = parent('broker'); begin(key); set_input(a, key)
        stale = dict(attachment, session=attachment['session'] + 1)
        event('detach', stale); status(held_a)
        call('set_input', {'buttons': [], '_temporal_owner': key, '_control_attachment': stale}, ok=False, stamp=False)
        status(held_a)
        event('detach', attachment)
        attachment = stale; event('attach', attachment); status(0)
        replacement = parent('broker-next'); begin(replacement); set_input(b, replacement)
        call('finish_temporal_operation', {'parent': key}, ok=False); status(held_b)
        reconnect(); status(0)
        key = parent('after-reconnect'); begin(key); call('finish_temporal_operation', {'parent': key})
        (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
            unread_reply_count=512, half_close_cleanup_seconds=elapsed, frozen_frame=origin_frame, cpu_park=args.cpu_park), indent=2))
    finally:
        if session: session.close()
        listener.close()
        if pid:
            actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
            if actual.returncode == 0:
                assert actual.stdout == fingerprint if fingerprint else str(home) in actual.stdout
                terminate_owned(pid)
            absent = subprocess.run(['ps', '-p', str(pid)], stdout=subprocess.DEVNULL).returncode != 0
            (out / 'exit.json').write_text(json.dumps(dict(pid=pid, exit_verified=absent))); assert absent


if __name__ == '__main__':
    main()
