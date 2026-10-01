#!/usr/bin/env python3
"""Owned bridge cancellation witness; not an MCP cancellation qualification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import uuid


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('bridge', 'native', 'session', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument("--stall-native", action="store_true", help="Inject native process suspension during the final advance (Unix)")
    parser.add_argument("--stall-phase", choices=("advance", "setup"), default="advance")
    parser.add_argument("--parent-input", action="store_true", help="Verify parent-owned input cleanup on idle frontend EOF")
    parser.add_argument("--broker", type=Path, help="Run through an isolated broker instead of a direct socket")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    session = json.loads(args.session.read_text())
    shutil.copytree(session['user_data'], out / 'share')
    session['user_data'] = str(out / 'share')
    shutil.copy2(session['media']['mounted_path'], out / 'content.rom')
    session['media']['mounted_path'] = str(out / 'content.rom')
    session['media']['source_path'] = str(out / 'content.rom')
    (out / 'session.json').write_text(json.dumps(session, indent=2))
    inputs = [args.bridge.resolve(), args.native.resolve(), Path(__file__).resolve(),
              out / 'session.json', out / 'content.rom']
    inputs += sorted((out / 'share/systemroms').glob('*'))
    if args.broker:
        inputs.append(args.broker.resolve())
    hashes = {str(p): digest(p) for p in inputs if p.is_file()}
    (out / 'producer.json').write_text(json.dumps(hashes, indent=2))
    runtime = 'direct-cancel-' + uuid.uuid4().hex
    env = dict(os.environ, EMUCAP_LAUNCH_ID=runtime)
    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    listener.settimeout(1)
    producer_port = listener.getsockname()[1]
    broker_process = None
    broker_log = None
    broker_port = None
    registration = None
    if args.broker:
        with socket.socket() as reserved:
            reserved.bind(('127.0.0.1', 0))
            broker_port = reserved.getsockname()[1]
        listener.close()
    process = None
    child_pid = None
    records = []
    audit = {}
    try:
        if args.broker:
            broker_log = (out / 'broker.log').open('w')
            broker_process = subprocess.Popen([str(args.broker.resolve())],
                env=dict(os.environ, EMUCAP_PORT=str(producer_port), EMUCAP_BROKER_SESSION_PORT=str(broker_port)),
                stdout=broker_log, stderr=broker_log)
            audit['broker_pid'] = broker_process.pid
        with (out / 'bridge.log').open('w') as log:
            process = subprocess.Popen([str(args.bridge.resolve()), str(producer_port),
                str(args.native.resolve()), str(out / 'session.json'), str(out / 'runtime'),
                '0', str(out / 'native.pid'), '0'], env=env, stdout=log, stderr=log)
            def connect_front(replace=False):
                nonlocal registration
                deadline = time.monotonic() + 30
                if not args.broker:
                    while True:
                        try:
                            connection, _ = listener.accept()
                            connection.settimeout(12)
                            return connection, connection.makefile('rb')
                        except socket.timeout:
                            assert process.poll() is None, "bridge exited during startup; see bridge.log"
                            assert time.monotonic() < deadline, "bridge startup deadline exceeded"
                while time.monotonic() < deadline:
                    assert broker_process.poll() is None, "owned broker exited; see broker.log"
                    assert process.poll() is None, "bridge exited; see bridge.log"
                    try:
                        connection = socket.create_connection(('127.0.0.1', broker_port), timeout=1)
                    except ConnectionRefusedError:
                        time.sleep(0.025)
                        continue
                    connection.settimeout(12)
                    front = connection.makefile('rb')
                    params = {'expected_launch_id':runtime}
                    if replace:
                        params['expected_registration_id'] = registration
                    connection.sendall((json.dumps({'v':1,'id':0,'method':'attach','params':params})+'\n').encode())
                    response = json.loads(front.readline())
                    records.append({'attach':response,'host':time.monotonic()})
                    if response['ok']:
                        registration = response['result']['broker_registration_id']
                        return connection, front
                    front.close()
                    connection.close()
                    assert response['error']['kind'] in ('not_connected','busy'), response
                    time.sleep(0.025)
                raise AssertionError('broker attachment deadline exceeded')
            conn, reader = connect_front()
            child_pid = int((out / 'native.pid').read_text())
            audit.update(bridge_pid=process.pid, native_pid=child_pid)
            counter = 0

            def send(method, params):
                nonlocal counter
                counter += 1
                request = {'v': 1, 'id': counter, 'method': method, 'params': params}
                records.append({'sent': request, 'host': time.monotonic()})
                conn.sendall((json.dumps(request) + '\n').encode())
                return counter

            def receive(expected):
                while True:
                    response = json.loads(reader.readline())
                    records.append({'received': response, 'host': time.monotonic()})
                    if response.get('result', {}).get('status') == 'working':
                        assert 0 < response['id'] <= counter, response
                        continue
                    assert response['id'] == expected, response
                    assert response['ok'], response
                    return response['result']

            def call(method, params=None):
                return receive(send(method, params or {}))

            hello = call('hello')
            assert hello.get('control_session_lifecycle') is True, hello
            call('pause')
            call('execution_speed', {'mode': 'unlimited'})
            call('step', {'frames': 400})
            outcomes = []
            for delay in (0.15, 2.2, 0.01):
                call('execution_speed', {'mode': 'limited', 'percent': 1})
                key = {'runtime': runtime, 'owner_id': uuid.uuid4().hex,
                       'operation_id': uuid.uuid4().hex}
                step_id = send('step', {'frames': 60, '_control': key})
                time.sleep(delay)
                started = time.monotonic()
                abort_id = send('cancel_operation', key)
                # The abort ack precedes native cleanup; the original terminal proves stopping.
                assert receive(abort_id)['status'] == 'requested'
                stopped = receive(step_id)
                latency = time.monotonic() - started
                assert stopped['status'] == 'interrupted' and stopped['reason'] == 'cancelled', stopped
                assert stopped['state'] == 'frozen' and 0 <= stopped['count'] < 60, stopped
                assert latency < 1.0, latency
                status = call('status')
                assert status['state'] == 'frozen', status
                before = status['frame']
                time.sleep(0.1)
                assert call('status')['frame'] == before
                policy = call('execution_speed')
                assert policy['mode'] == 'limited' and policy['percent'] == 1, policy
                call('execution_speed', {'mode': 'unlimited'})
                resumed = call('step', {'frames': 2})
                assert resumed['status'] == 'completed' and resumed['count'] == 2, resumed
                outcomes.append({'latency': latency, 'interrupted': stopped, 'next_step': resumed})
            assert any(case['interrupted']['count'] > 0 for case in outcomes), outcomes
            call('execution_speed', {'mode': 'limited', 'percent': 1})
            key = {'runtime': runtime, 'owner_id': uuid.uuid4().hex,
                   'operation_id': uuid.uuid4().hex}
            send('step', {'frames': 60, '_control': key})
            time.sleep(0.15)
            reader.close()
            conn.close()
            started = time.monotonic()
            conn, reader = connect_front()
            status = call('status')
            assert status['state'] == 'frozen', status
            reconnect_latency = time.monotonic() - started
            assert reconnect_latency < 1.0, reconnect_latency
            assert call('cancel_operation', key)['status'] == 'not_active'
            call('execution_speed', {'mode': 'unlimited'})
            assert call('step', {'frames': 2})['status'] == 'completed'
            parent_cleanup = None
            if args.parent_input:
                call('set_input', {'port':1, 'buttons':['left']})
                parent = {'runtime':runtime, 'owner_id':uuid.uuid4().hex, 'operation_id':uuid.uuid4().hex}
                admitted = call('begin_temporal_operation', {'parent':parent})
                assert admitted['parent'] == parent and admitted['status'] == 'admitted', admitted
                call('set_input', {'port':0, 'buttons':['space'], '_temporal_owner':parent})
                call('set_input', {'port':2, 'buttons':['a'], '_temporal_owner':parent})
                pressed = call('status', {'_temporal_owner':parent})
                assert pressed['input_matrix']['8'] & 1 == 0, pressed
                assert pressed['joystick_ports'][1]['engaged'], pressed
                reader.close()
                conn.close()
                started = time.monotonic()
                conn, reader = connect_front()
                released = call('status')
                latency = time.monotonic() - started
                assert released['state'] == 'frozen', released
                assert released['input_matrix']['8'] & 1 == 1, released
                assert not released['joystick_ports'][1]['engaged'], released
                assert released['joystick_ports'][0]['buttons'] == ['left'], released
                assert latency < 1.0, latency
                next_parent = {'runtime':runtime, 'owner_id':uuid.uuid4().hex, 'operation_id':uuid.uuid4().hex}
                call('begin_temporal_operation', {'parent':next_parent})
                finished = call('finish_temporal_operation', {'parent':next_parent})
                assert finished['cleanup_verified'] and finished['released_ports'] == [], finished
                call('set_input', {'port':1, 'buttons':[]})
                assert call('step', {'frames':2})['status'] == 'completed'
                parent_cleanup = {'latency':latency, 'pressed':pressed, 'released':released, 'new_parent_terminal':finished}
            replacement = None
            if args.broker and args.parent_input:
                call('execution_speed', {'mode':'limited','percent':1})
                parent = {'runtime':runtime,'owner_id':uuid.uuid4().hex,'operation_id':uuid.uuid4().hex}
                call('begin_temporal_operation', {'parent':parent})
                call('set_input', {'port':0,'buttons':['space'],'_temporal_owner':parent})
                child = dict(parent, operation_id=uuid.uuid4().hex)
                send('step', {'frames':60,'_control':child,'_temporal_owner':parent})
                time.sleep(0.15)
                old_conn, old_reader = conn, reader
                started = time.monotonic()
                conn, reader = connect_front(replace=True)
                old_reader.close()
                old_conn.close()
                # This read is queued behind detach/attach and must see completed cleanup.
                restored = call('status')
                elapsed = time.monotonic()-started
                assert restored['state']=='frozen' and restored['input_matrix']['8'] & 1 == 1, restored
                assert elapsed < 1.0, elapsed
                new_parent = dict(parent, owner_id=uuid.uuid4().hex, operation_id=uuid.uuid4().hex)
                call('begin_temporal_operation', {'parent':new_parent})
                terminal = call('finish_temporal_operation', {'parent':new_parent})
                assert terminal['cleanup_verified'], terminal
                call('execution_speed', {'mode':'unlimited'})
                assert call('step', {'frames':2})['status']=='completed'
                replacement = {'latency':elapsed,'status':restored,'new_parent_terminal':terminal}
            broker_restart = None
            if args.broker and args.parent_input:
                parent = {'runtime':runtime,'owner_id':uuid.uuid4().hex,'operation_id':uuid.uuid4().hex}
                call('begin_temporal_operation', {'parent':parent})
                call('set_input', {'port':2,'buttons':['a'],'_temporal_owner':parent})
                assert call('status', {'_temporal_owner':parent})['joystick_ports'][1]['engaged']
                started = time.monotonic()
                old_broker_pid = broker_process.pid
                broker_process.kill()
                broker_process.wait(timeout=5)
                audit['replaced_broker'] = {'pid':old_broker_pid,'returncode':broker_process.returncode}
                reader.close()
                conn.close()
                broker_process = subprocess.Popen([str(args.broker.resolve())],
                    env=dict(os.environ, EMUCAP_PORT=str(producer_port), EMUCAP_BROKER_SESSION_PORT=str(broker_port)),
                    stdout=broker_log, stderr=broker_log)
                audit['broker_pid'] = broker_process.pid
                conn, reader = connect_front()
                restored = call('status')
                elapsed = time.monotonic()-started
                assert restored['state']=='frozen' and not restored['joystick_ports'][1]['engaged'], restored
                assert elapsed < 1.0, elapsed
                next_parent = dict(parent, operation_id=uuid.uuid4().hex)
                call('begin_temporal_operation', {'parent':next_parent})
                assert call('finish_temporal_operation', {'parent':next_parent})['cleanup_verified']
                assert call('step', {'frames':2})['status']=='completed'
                broker_restart = {'latency':elapsed,'status':restored}
            stalled = None
            if args.stall_native:
                call('execution_speed', {'mode': 'limited', 'percent': 1})
                key = {'runtime': runtime, 'owner_id': uuid.uuid4().hex,
                       'operation_id': uuid.uuid4().hex}
                if args.stall_phase == 'setup':
                    os.kill(child_pid, signal.SIGSTOP)
                step_id = send('step', {'frames': 60, '_control': key})
                time.sleep(0.15)
                if args.stall_phase == 'advance':
                    os.kill(child_pid, signal.SIGSTOP)
                started = time.monotonic()
                abort_id = send('cancel_operation', key)
                terminals = {}
                while len(terminals) < 2:
                    line = reader.readline()
                    assert line, 'bridge closed before reporting the native failure'
                    response = json.loads(line)
                    records.append({'received': response, 'host': time.monotonic()})
                    if response.get('result', {}).get('status') == 'working':
                        continue
                    assert response['id'] in (step_id, abort_id), response
                    assert response['id'] not in terminals, response
                    terminals[response['id']] = response
                elapsed = time.monotonic() - started
                assert terminals[abort_id]['ok'], terminals
                assert terminals[abort_id]['result']['status'] == 'requested', terminals
                assert not terminals[step_id]['ok'], terminals
                assert elapsed < 1.0, elapsed
                process.wait(timeout=2)
                native_absent = subprocess.run(['ps', '-p', str(child_pid)],
                    stdout=subprocess.DEVNULL).returncode != 0
                assert native_absent, 'unverified native owner survived deadline retirement'
                stalled = {'phase': args.stall_phase, 'latency': elapsed, 'responses': terminals, 'native_absent':native_absent,
                           'bridge_returncode':process.returncode}
            (out / 'result.json').write_text(json.dumps({'passed': True, 'cases': outcomes,
                'disconnect': {'reconnect_latency': reconnect_latency, 'status': status},
                'stalled_native': stalled, 'parent_cleanup':parent_cleanup, 'replacement':replacement, 'broker_restart':broker_restart}, indent=2))
            reader.close()
            conn.close()
    finally:
        listener.close()
        if broker_process is not None:
            broker_process.terminate()
            try:
                broker_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                broker_process.kill()
                broker_process.wait(timeout=5)
            audit['broker_returncode'] = broker_process.returncode
        if broker_log is not None:
            broker_log.close()
        if child_pid is not None:
            try:
                os.kill(child_pid, signal.SIGCONT)
                os.kill(child_pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if process is not None:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            audit['bridge_returncode'] = process.returncode
        if child_pid is not None:
            audit['native_absent'] = subprocess.run(['ps', '-p', str(child_pid)],
                stdout=subprocess.DEVNULL).returncode != 0
        audit['inputs_unchanged'] = all(digest(p) == value for p, value in hashes.items())
        (out / 'audit.json').write_text(json.dumps(audit, indent=2))
        (out / 'requests.json').write_text(json.dumps(records, indent=2))
    assert audit.get('native_absent') and audit['inputs_unchanged'], audit
    print((out / 'result.json').read_text())


if __name__ == '__main__':
    main()
