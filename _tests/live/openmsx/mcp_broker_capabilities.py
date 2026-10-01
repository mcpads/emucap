#!/usr/bin/env python3
"""Managed MCP witness for producer capability preservation through an isolated broker."""
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
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port
from support import terminate_owned
from response_gate import ResponseGate


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def send(process, value):
    process.process.stdin.write(json.dumps(value) + '\n')
    process.process.stdin.flush()


def begin_tap(witness, out, port, button, label, percent):
    witness.speed({'mode':'limited','percent':percent})
    process = witness.process
    request_id = process.next_id
    process.next_id += 1
    send(process, {'jsonrpc':'2.0','id':request_id,'method':'tools/call', 'params':{
        'name':'tap','arguments':{'port':port,'buttons':[button],
                                 'press_frames':120,'after_frames':120}}})
    records = list((out / 'home').glob('sessions/*/generations/*/link.json'))
    assert len(records) == 1, records
    deadline = time.monotonic() + 3
    while True:
        record = json.loads(records[0].read_text())
        if record.get('temporal_operation') and record.get('last_method') == 'set_input':
            break
        assert time.monotonic() < deadline, record
        time.sleep(.005)
    # At 1%, 120 frames cannot finish during this observation interval.
    time.sleep(.25)
    active = json.loads(records[0].read_text())
    assert active.get('temporal_operation') == record['temporal_operation'], active
    assert active.get('last_method') == 'set_input', active
    (out / (label + '-active.json')).write_text(json.dumps(active, indent=2))
    return request_id, records[0], active


def assert_released(status):
    assert status['state'] == 'frozen' and not status['input_override'], status
    assert status['input_matrix']['8'] & 1, status
    assert all(p['active_low_mask'] is None for p in status['joystick_ports']), status


def cancel_tap(witness, out, port, button, percent):
    """Cancel a real MCP call after input dispatch and durable parent admission."""
    request_id, record_path, _ = begin_tap(witness, out, port, button, 'cancel-' + str(port), percent)
    started = time.monotonic()
    send(witness.process, {'jsonrpc':'2.0','method':'notifications/cancelled',
                          'params':{'requestId':request_id}})
    deadline = started + 1
    while True:
        status, failed = witness.call_result('status')
        if not failed:
            break
        assert status.get('error',{}).get('code') == 'busy', status
        assert time.monotonic() < deadline, status
        time.sleep(.005)
    elapsed = time.monotonic() - started
    assert elapsed < 1, elapsed
    assert_released(status)
    record = json.loads(record_path.read_text())
    assert not record.get('temporal_operation'), record
    witness.speed({'mode':'unlimited'})
    assert witness.call('step', {'count':2,'unit':'frames'})['status'] == 'completed'
    return {'port':port,'cancel_to_observed_cleanup_seconds':elapsed,'status':status,
            'durable_record':record}


def lose_mcp(witness, out, env, binary, gate=None, boundary=None, percent=1):
    before = None
    if gate is None:
        _, record_path, active = begin_tap(witness, out, 2, 'a', 'process-loss', percent)
    else:
        witness.speed({'mode':'limited','percent':1})
        before = witness.call('status')
        gate.boundary = boundary
        process = witness.process
        request_id = process.next_id
        process.next_id += 1
        send(process, {'jsonrpc':'2.0','id':request_id,'method':'tools/call','params':{
            'name':'tap','arguments':{'port':2,'buttons':['a'],'press_frames':1,'after_frames':120}}})
        assert gate.reached.wait(5), {'error':gate.error,'wire':gate.rows}
        records = list((out / 'home').glob('sessions/*/generations/*/link.json'))
        assert len(records) == 1, records
        record_path = records[0]
        active = json.loads(record_path.read_text())
        assert active.get('temporal_operation'), active
        (out / 'process-loss-active.json').write_text(json.dumps(active, indent=2))
    (out / 'before-process-loss-requests.json').write_text(json.dumps(witness.rows, indent=2))
    process = witness.process
    started = time.monotonic()
    process.process.kill()
    process.process.wait(timeout=5)
    process.reader.join(timeout=2)
    for stream in (process.process.stdin, process.process.stdout, process.process.stderr):
        stream.close()
    if gate is not None:
        assert gate.stopped.wait(1), 'MCP EOF did not close relay backend'
        assert gate.error is None, gate.error
        (out / 'gated-wire.json').write_text(json.dumps(gate.rows, indent=2))
        withheld = next(i for i, row in enumerate(gate.rows) if row['event'] == 'withheld')
        assert not any(row['event'] == 'request' for row in gate.rows[withheld+1:]), gate.rows
    replacement = Witness(out, env, binary)
    try:
        replacement.process.initialize()
        deadline = started + 1
        while True:
            status, failed = replacement.call_result('status')
            if not failed and status.get('connected') and status.get('state') == 'frozen':
                break
            assert time.monotonic() < deadline, status
            time.sleep(.005)
        elapsed = time.monotonic() - started
        assert elapsed < 1, elapsed
        assert_released(status)
        if before is not None:
            assert status['frame'] == before['frame'] + (boundary != 'input_pressed'), status
        record = json.loads(record_path.read_text())
        assert record['temporal_operation'] == active['temporal_operation'], record
        blocked = replacement.call('step', {'count':1,'unit':'frames'}, error=True)
        assert blocked['error']['code'] == 'temporal_quarantined', blocked
        after = replacement.call('status')
        assert after['frame'] == status['frame'] and after['state'] == 'frozen', after
        return replacement, {'kill_to_observed_cleanup_seconds':elapsed,
            'killed_mcp_pid':process.process.pid,'returncode':process.process.returncode,
            'boundary':boundary,'withheld':gate.held if gate else None,'before':before,
            'status':status,'durable_record':record,'mutation_rejection':blocked}
    except BaseException:
        replacement.process.close()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('mcp', 'broker', 'bridge', 'native', 'content', 'firmware', 'output'):
        parser.add_argument('--' + key, type=Path, required=True)
    parser.add_argument('--system', choices=['msx','msx1','msx2','msx2p'], default='msx2')
    parser.add_argument('--cancel', action='store_true')
    parser.add_argument('--cancel-percent', type=float, default=1)
    parser.add_argument('--process-loss', action='store_true')
    parser.add_argument('--loss-boundary', choices=['input_pressed','press_completed','input_released'])
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_BROKER='1',
               EMUCAP_PORT=str(free_port()), EMUCAP_BROKER_SESSION_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'), EMUCAP_OPENMSX_BIN=str(args.native.resolve()),
               EMUCAP_OPENMSX_BRIDGE_BIN=str(args.bridge.resolve()),
               EMUCAP_OPENMSX_FIRMWARE=str(args.firmware.resolve()))
    bound = [args.mcp, args.broker, args.bridge, args.native, args.content, Path(__file__),
             ROOT / '_tests/live/observation_speed.py', ROOT / '_tests/live/mesen2/support.py',
             Path(__file__).with_name('response_gate.py')]
    bound += [p for p in args.firmware.rglob('*') if p.is_file()]
    hashes = {str(p.resolve()):digest(p) for p in bound}
    (out / 'producer.json').write_text(json.dumps(hashes, indent=2))
    witness = None
    launch = None
    audit = {}
    broker = None
    gate = None
    log = (out / 'broker.log').open('w')
    try:
        # Managed launch belongs to the direct controller. Broker mode attaches an
        # existing producer and does not manufacture a direct listening port.
        launcher = Witness(out, dict(env, EMUCAP_BROKER='0'), args.mcp.resolve())
        witness = launcher
        launcher.process.initialize()
        launcher.call('bootstrap')
        plan = launcher.call('launch_plan', {'content_path':str(args.content.resolve()),'system':args.system})
        assert plan['ready_to_launch'], plan
        launch = launcher.call('launch', {**plan['preferred_launcher']['args'],
                                         'display':False,'sound':False})
        launcher.call('pause')
        (out / 'launch.json').write_text(json.dumps(launch,indent=2))
        (out / 'launch-requests.json').write_text(json.dumps(launcher.rows,indent=2))
        launcher.process.close()
        witness = None
        broker = subprocess.Popen([str(args.broker.resolve())], env=env, stdout=log, stderr=log)
        audit['broker_pid'] = broker.pid
        deadline = time.monotonic() + 10
        while True:
            assert broker.poll() is None, 'owned broker exited'
            try:
                with socket.create_connection(('127.0.0.1', int(env['EMUCAP_BROKER_SESSION_PORT'])), timeout=.1):
                    break
            except OSError:
                assert time.monotonic() < deadline, 'broker startup deadline'
                time.sleep(.025)
        witness_env = env
        if args.loss_boundary:
            gate = ResponseGate(int(env['EMUCAP_BROKER_SESSION_PORT']))
            witness_env = dict(env, EMUCAP_BROKER_SESSION_PORT=str(gate.port))
        witness = Witness(out, witness_env, args.mcp.resolve())
        witness.process.initialize()
        deadline = time.monotonic() + 10
        while True:
            status, failed = witness.call_result('status')
            if not failed and status.get('connected'):
                break
            assert time.monotonic() < deadline, status
            time.sleep(.05)
        assert status['state'] == 'frozen', status
        assert status['execution_speed_capability'] and status['memory_batch_capability'], status
        witness.speed({'mode':'limited','percent':1})
        before = witness.call('status')
        batch = witness.call('read_memory_batch', {'ranges':[
            {'memory_type':'memory','address':0,'length':4},
            {'memory_type':'ram','address':0,'length':4}]})
        after = witness.call('status')
        assert before['frame'] == after['frame'] and after['state'] == 'frozen', after
        assert len(batch['reads']) == 2, batch
        witness.speed({'mode':'unlimited'})
        assert witness.call('step', {'count':2,'unit':'frames'})['status'] == 'completed'
        cancellation = [cancel_tap(witness, out, port, button, args.cancel_percent)
                        for port, button in [(0,'space'),(1,'a'),(2,'b')]] if args.cancel else None
        process_loss = None
        if args.process_loss or args.loss_boundary:
            witness, process_loss = lose_mcp(witness, out, env, args.mcp.resolve(), gate, args.loss_boundary, args.cancel_percent)
        (out / 'result.json').write_text(json.dumps({'passed':True,'launch':launch,
            'cancellation':cancellation,'process_loss':process_loss,'cancel_percent':args.cancel_percent,
            'status':status,'batch':batch},indent=2))
    finally:
        if witness is not None and witness.process.process.poll() is None:
            witness.process.close()
        if gate is not None:
            gate.close()
            (out / 'gated-wire.json').write_text(json.dumps(gate.rows, indent=2))
            audit['relay_stopped'] = gate.stopped.is_set()
        if broker is not None:
            broker.terminate()
            try:
                broker.wait(timeout=5)
            except subprocess.TimeoutExpired:
                broker.kill()
                broker.wait(timeout=5)
            audit['broker_returncode'] = broker.returncode
        log.close()
        if launch is not None:
            # These exact PIDs were returned by this harness's managed launch.
            pids = [launch.get('openmsx_pid', launch.get('pid')), launch.get('bridge_pid')]
            for pid in pids:
                if pid:
                    terminate_owned(pid)
            audit['owned_processes_absent'] = all(subprocess.run(['ps','-p',str(pid)],
                stdout=subprocess.DEVNULL).returncode != 0 for pid in pids if pid)
        audit['inputs_unchanged'] = all(digest(path)==value for path,value in hashes.items())
        (out / 'audit.json').write_text(json.dumps(audit,indent=2))
    assert audit['inputs_unchanged'] and audit.get('owned_processes_absent'), audit
    print(json.dumps({'passed':True,'output':str(out)}))


if __name__ == '__main__':
    main()
