#!/usr/bin/env python3
"""Opt-in MD/PSX native boundary, generation and completed-progress witness."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import Session, require_ok, terminate_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--system', choices=('md', 'psx'), default='md')
    parser.add_argument('--rom', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=ROOT / 'adapters/mednafen/work/mednafen/src/mednafen')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve()
    listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(15)
    port = listener.getsockname()[1]
    home = out / 'home'
    env = dict(os.environ, EMUCAP_EMU_HOME=str(home), EMUCAP_START_FROZEN='1',
               EMUCAP_HEADLESS='1', MEDNAFEN_SOUND='0', EMUCAP_POST_CONNECT_GRACE='0', MEDNAFEN_BIN=str(binary))
    session = None; pid = None; fingerprint = None; rows = []

    def call(method, params=None):
        result = require_ok(session.request(method, params), method)
        rows.append(dict(method=method, params=params, result=result))
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        return result

    try:
        launch = subprocess.run(['bash', str(ROOT / 'adapters/mednafen/launch.sh'), str(args.rom.resolve()),
                                 str(port), 'native-boundary', args.system], env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mednafen' / str(port) / 'mednafen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0]); identity = call('hello')
        def observation(context, parked=True):
            status = call('status')
            native = status['native_control']
            assert native['valid'] and native['context'] == context, status
            assert native['parked'] == parked, status
            return native

        cold = observation('frame_boundary')
        assert cold['completed_frames'] == 0 and cold['cpu_callbacks'] == 0, cold
        call('execution_speed', {'mode': 'unlimited'})
        call('step', {'frames': 2})
        frame = observation('frame_boundary')
        assert frame['completed_frames'] == 2 and frame['generation'] == cold['generation']
        snapshot = out / 'checkpoint.mcs'
        call('save_state', {'path': str(snapshot)})
        assert observation('frame_boundary') == frame
        call('step_instructions', {'count': 3})
        cpu = observation('cpu_callback')
        assert cpu['completed_frames'] == frame['completed_frames']
        assert cpu['cpu_callbacks'] - frame['cpu_callbacks'] == 4, cpu
        call('step_instructions', {'count': 2})
        chained = observation('cpu_callback')
        assert chained['cpu_callbacks'] - cpu['cpu_callbacks'] == 2, chained
        # PSX refreshes the CPU callback before replying; MD remains on its old stack.
        call('reset')
        reset = observation('cpu_callback', parked=args.system == 'psx')
        assert reset['generation'] > chained['generation']
        if args.system == 'psx':
            # PSX load completes at a fresh restored callback, before guest execution.
            call('load_state', {'path': str(snapshot)})
            refreshed = observation('cpu_callback')
            assert refreshed['generation'] > reset['generation']
            assert refreshed['cpu_callbacks'] == reset['cpu_callbacks'] + 1
            assert refreshed['completed_frames'] == reset['completed_frames']
        call('step', {'frames': 1})
        resumed = observation('frame_boundary')
        assert resumed['completed_frames'] > reset['completed_frames']
        call('load_state', {'path': str(snapshot)})
        restored = observation('frame_boundary')
        assert restored['generation'] > resumed['generation']
        assert restored['completed_frames'] == resumed['completed_frames']
        # Refused preflight (file missing) has no native restore attempt.
        missing = session.request('load_state', {'path': str(out / 'absent.mcs')})
        rows.append(dict(method='load_state_missing', response=missing))
        assert not missing['ok'] and missing['error']['kind'] == 'io_error', missing
        assert observation('frame_boundary') == restored
        # Once native restore is attempted, even rejection invalidates old evidence.
        malformed = out / 'malformed.mcs'; malformed.write_bytes(b'invalid')
        invalid = session.request('load_state', {'path': str(malformed)})
        rows.append(dict(method='load_state_malformed', response=invalid))
        assert not invalid['ok'] and invalid['error']['kind'] == 'io_error', invalid
        rejected = observation('frame_boundary')
        assert rejected['generation'] > restored['generation']
        assert rejected['completed_frames'] == restored['completed_frames']
        call('execution_speed', {'mode': 'limited', 'percent': 1})
        call('resume')
        deadline = time.monotonic() + 8
        while True:
            pacing = call('status')['native_control']
            if pacing['context'] == 'pacing_wait': break
            assert time.monotonic() < deadline, pacing
            time.sleep(0.01)
        assert not pacing['parked'] and pacing['valid'], pacing
        call('pause'); observation('frame_boundary')
        if args.system == 'md':
            call('execution_speed', {'mode': 'unlimited'})
            call('set_breakpoint', {'kind': 'write', 'memory_type': 'vram',
                                   'start': 0, 'end': 65535, 'pause_on_hit': True})
            hit = call('step', {'frames': 30})
            assert hit['status'] == 'interrupted', hit
            observation('device_callback', parked=False)
            device_parent = dict(runtime=identity.get('launch_id') or f'unmanaged-pid:{pid}',
                                 owner_id='boundary-witness', operation_id='device-admission')
            refused = session.request('begin_temporal_operation', {'parent': device_parent})
            rows.append(dict(method='device_parent_admission', response=refused))
            assert not refused['ok'] and refused['error']['kind'] == 'unsupported', refused
            observation('device_callback', parked=False)
            call('clear_all_breakpoints')
            call('step', {'frames': 1}); observation('frame_boundary')
        (out / 'result.json').write_text(json.dumps(dict(passed=True,
            cold_callbacks=4, chained_callbacks=2, generation_invalidation=True,
            failed_restore_invalidation=True, pacing_not_parked=True), indent=2))
        (out / 'artifacts.json').write_text(json.dumps(dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), hello=identity), indent=2))
    finally:
        if session: session.close()
        listener.close()
        if pid:
            actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
            if actual.returncode == 0:
                assert actual.stdout == fingerprint if fingerprint else str(binary) in actual.stdout
                terminate_owned(pid)
            absent = subprocess.run(['ps', '-p', str(pid)], stdout=subprocess.DEVNULL).returncode != 0
            (out / 'exit.json').write_text(json.dumps(dict(pid=pid, exit_verified=absent))); assert absent


if __name__ == '__main__':
    main()
