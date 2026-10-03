#!/usr/bin/env python3
"""Opt-in MD native request-routing and malformed-frame no-effect witness."""
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

    def raw(text):
        session.socket.sendall(text.encode() + b'\n')
        data = session.file.readline()
        response = json.loads(data) if data else None
        rows.append(dict(wire=text, response=response))
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        return response

    def call(method, params=None):
        result = require_ok(session.request(method, params), method)
        rows.append(dict(method=method, params=params, result=result))
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        return result

    try:
        launch = subprocess.run(['bash', str(ROOT / 'adapters/mednafen/launch.sh'), str(args.rom.resolve()),
                                 str(port), 'strict-control-wire', 'md'], env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mednafen' / str(port) / 'mednafen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0]); identity = call('hello')
        call('set_input', {'buttons': ['b']})
        before = call('status'); assert before['state'] == 'frozen' and before['native_input_mask'] != 0
        memory = {'ranges': [{'memory_type': 'ram', 'address': 0, 'length': 16}]}
        baseline_memory = call('read_memory_batch', memory)
        invalid_inputs = [
            {'buttons': [1]}, {'buttons': [None]}, {'buttons': [False]},
            {'buttons': [[]]}, {'buttons': [{}]}, {'buttons': ['a', 1]},
            {'buttons': None}, {'buttons': 'a'}, {'buttons': {'other': []}},
        ] + [{'port': port, 'buttons': ['a']} for port in (False, True, '0', None, [], {}, 0.5, 1)]
        for method in ('set_input', 'press_buttons'):
            for params in invalid_inputs:
                response = raw(json.dumps(dict(v=1, id=68, method=method, params=params)))
                assert response['id'] == 68 and not response['ok'] and response['error']['kind'] == 'bad_params', response
                assert call('read_memory_batch', memory) == baseline_memory
                after = call('status')
                assert after['state'] == 'frozen' and after['frame'] == before['frame'], after
                assert after['input_override'] == before['input_override'] and after['native_input_mask'] == before['native_input_mask'], after
        for frames in (0, -1, 1.5, True, '2', None, [], {}, 5001):
            response = raw(json.dumps(dict(v=1, id=69, method='press_buttons', params={'buttons': ['a'], 'frames': frames})))
            assert response['id'] == 69 and not response['ok'] and response['error']['kind'] == 'bad_params', response
            assert call('read_memory_batch', memory) == baseline_memory
            after = call('status')
            assert after['state'] == 'frozen' and after['frame'] == before['frame'], after
            assert after['input_override'] == before['input_override'] and after['native_input_mask'] == before['native_input_mask'], after
        # Field order and a method-looking parameter cannot select a different handler.
        response = raw('{"params":{"method":"set_input","buttons":["a"],"id":99},"method":"status","id":70,"v":1}')
        assert response['id'] == 70 and response['ok'] and response['result']['state'] == 'frozen', response
        assert response['result']['native_input_mask'] == before['native_input_mask']
        for field in ('_control', '_temporal_owner', '_control_session', '_control_attachment'):
            response = raw(json.dumps(dict(v=1, id=71, method='set_input', params={'buttons': ['a'], field: {}})))
            assert response['id'] == 71 and not response['ok'] and response['error']['kind'] in ('bad_params', 'bad_state'), response
            assert call('status')['native_input_mask'] == before['native_input_mask']
        response = raw('{"v":1,"id":18446744073709551615,"method":"status","params":null}')
        assert response['id'] == 18446744073709551615 and not response['ok']
        malformed = [
            '{"v":1,"id":72,"method":"status","method":"reset","params":{}}',
            '{"v":1,"id":72,"method":"set_input","params":{"buttons":[],"\\u0062uttons":["a"]}}',
            '{"v":1,"id":72.0,"method":"reset","params":{}}',
            '{"v":1,"id":72,"method":"reset","params":{}} trailing',
            '{"v":1,"id":72,"method":"reset","params":{"x":' + '[' * 33 + '0' + ']' * 33 + '}}',
        ]
        for wire in malformed:
            assert raw(wire) is None
            session.close(); session = Session(listener.accept()[0]); call('hello')
            assert call('read_memory_batch', memory) == baseline_memory
            after = call('status')
            assert after['state'] == 'frozen' and after['frame'] == before['frame'], after
            assert after['native_input_mask'] == before['native_input_mask'], after
            assert after['input_override'] == before['input_override'], after
        call('execution_speed', {'mode': 'limited', 'percent': 1})
        call('resume')
        initial_frame = call('status')['frame']
        deadline = time.monotonic() + 8
        while True:
            running_frame = call('status')['frame']
            if running_frame > initial_frame: break
            assert time.monotonic() < deadline, 'no completed frame at 1% pacing'
            time.sleep(0.02)
        started = time.monotonic()
        for method, params in (
                ('set_input', {'buttons': [1]}),
                ('set_input', {'port': False, 'buttons': ['a']}),
                ('press_buttons', {'buttons': ['a'], 'frames': 1.5}),
                ('press_buttons', {'buttons': [None]})):
            response = raw(json.dumps(dict(v=1, id=73, method=method, params=params)))
            assert not response['ok'] and response['error']['kind'] == 'bad_params', response
        response = raw('{"v":1,"id":18446744073709551615,"method":"step","params":{"frames":1}}')
        assert response['id'] == 18446744073709551615 and not response['ok']
        after = call('status'); elapsed = time.monotonic() - started
        pacing = dict(frame_before=running_frame, frame_after=after['frame'], elapsed_seconds=elapsed)
        (out / 'pacing-rejections.json').write_text(json.dumps(pacing, indent=2))
        assert elapsed < 0.5, 'observation exceeded the qualified portion of the 1% frame period'
        assert after['state'] == 'running' and after['frame'] == running_frame, pacing
        assert after['native_input_mask'] == before['native_input_mask'], after
        call('pause'); assert call('status')['state'] == 'frozen'
        (out / 'result.json').write_text(json.dumps(dict(passed=True, malformed_frames=len(malformed),
            frozen_input_rejections=2*len(invalid_inputs)+9, running_rejections=5, pacing=pacing), indent=2))
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
