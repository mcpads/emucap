#!/usr/bin/env python3
"""Opt-in native Mesen control service/progress witness; does not qualify MCP cancellation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess

from support import ROOT, LAUNCHER, Session, default_binary, require_ok, terminate_owned

WRAPPER = r'''
assert(type(emu.getControlState) == 'function' and emu.eventType.controlIdle ~= nil)
local function record(kind, count, unit)
  local state = emu.getControlState()
  local fields = {'"kind":"' .. kind .. '"'}
  for key, value in pairs(state) do
    fields[#fields+1] = '"' .. key .. '":' .. tostring(value)
  end
  if count then
    fields[#fields+1] = '"requested":' .. tostring(count)
    fields[#fields+1] = '"unit":' .. tostring(unit)
  end
  local f = assert(io.open(os.getenv('EMUCAP_CONTROL_TRACE'), 'ab'))
  f:write('{' .. table.concat(fields, ',') .. '}\n'); f:close()
end
local step = emu.step
emu.step = function(count, unit, cpu)
  record('before_step', count, unit)
  step(count, unit, cpu)
  record('after_step', count, unit)
end
emu.addEventCallback(function() record('control') end, emu.eventType.controlIdle)
emu.addEventCallback(function() record('pacing') end, emu.eventType.pacingIdle)
emu.addEventCallback(function() record('halt') end, emu.eventType.codeBreak)
dofile(os.getenv('EMUCAP_CONTROL_ENTRY'))
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    system = profile['launch_plan']['system']
    entry = {'snes': 'snes', 'nes': 'nes', 'gb': 'gb', 'gbc': 'gb',
             'gba': 'gba', 'sms': 'sms', 'gamegear': 'sms'}[system]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    wrapper = out / 'observe.lua'
    wrapper.write_text(WRAPPER)
    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen()
    listener.settimeout(30)
    port = listener.getsockname()[1]
    home = out / 'home'
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_EMU_HOME=str(home), MESEN_BIN=str(default_binary()),
               EMUCAP_MESEN_LUA=str(wrapper), EMUCAP_LAUNCH_WAIT='10',
               EMUCAP_POST_CONNECT_GRACE='0', EMUCAP_LOG=str(out / 'launch.log'),
               EMUCAP_CONTROL_TRACE=str(out / 'trace.jsonl'),
               EMUCAP_CONTROL_ENTRY=str(ROOT / 'adapters/mesen2' / f'emucap-{entry}.lua'))
    session, pid, fingerprint = None, None, None
    rows = []
    try:
        launch = subprocess.run(['bash', str(LAUNCHER), profile['launch_plan']['content_path'],
                                 str(port), 'native-control-progress', system],
                                env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch-output.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mesen2' / str(port) / 'mesen.pid'
        if pidfile.exists():
            pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0])
        identity = require_ok(session.request('hello'), 'hello')
        (out / 'identity.json').write_text(json.dumps(identity, indent=2))

        def call(method, params=None):
            result = require_ok(session.request(method, params), method)
            rows.append({'method': method, 'params': params, 'result': result})
            (out / 'requests.json').write_text(json.dumps(rows, indent=2))
            return result

        call('pause')
        assert call('status')['state'] == 'frozen'
        (out / 'trace.jsonl').write_text('')
        call('execution_speed', {'mode': 'unlimited'})
        for count in (1, 3):
            assert call('step_instructions', {'count': count})['status'] == 'completed'
        for count in (1, 2):
            assert call('step', {'frames': count})['status'] == 'completed'
        call('execution_speed', {'mode': 'limited', 'percent': 1})
        assert call('step', {'frames': 2})['status'] == 'completed'
        assert call('status')['state'] == 'frozen'
        trace = [json.loads(line) for line in (out / 'trace.jsonl').read_text().splitlines()]
        admitted = None
        pairs = []
        for row in trace:
            if row['kind'] == 'before_step':
                assert row['halted'], row
            if row['kind'] == 'after_step':
                assert not row['halted'] and admitted is None, row
                admitted = row
            elif row['kind'] == 'halt' and admitted:
                assert row['halted'] and row['generation'] == admitted['generation']
                if admitted['instructionRemaining'] > 0:
                    count = (row['instructionBoundaries'] - admitted['instructionBoundaries']
                             + row['haltedCpuSteps'] - admitted['haltedCpuSteps'])
                    assert count == admitted['instructionRemaining'], (admitted, row)
                else:
                    count = row['ppuCycles'] - admitted['ppuCycles']
                    assert count == admitted['ppuRemaining'], (admitted, row)
                pairs.append({'start': admitted, 'stop': row, 'native_delta': count})
                admitted = None
        assert admitted is None and len(pairs) == 6, pairs
        service = [r for r in trace if r['kind'] in ('control', 'pacing')]
        assert any(r['kind'] == 'control' for r in service)
        assert any(r['kind'] == 'pacing' for r in service)
        assert all(not r['halted'] for r in service)
        # Bound gaps within each active chunk; time between separate commands is excluded.
        max_gap = 0
        for pair in pairs:
            start, stop = pair['start']['monotonicMs'], pair['stop']['monotonicMs']
            stamps = [start] + [r['monotonicMs'] for r in service if start <= r['monotonicMs'] <= stop] + [stop]
            max_gap = max(max_gap, max(b - a for a, b in zip(stamps, stamps[1:])))
        assert max_gap <= 50, max_gap
        # Apphost bytes alone do not identify the native core in framework-dependent bundles.
        artifacts = [p for p in home.rglob('*') if p.is_file() and p.name in
                     ('Mesen', 'Mesen.dll', 'MesenCore.dylib', 'MesenCore.dll', 'MesenCore.so')]
        (out / 'artifacts.json').write_text(json.dumps({str(p.relative_to(out)):
            hashlib.sha256(p.read_bytes()).hexdigest() for p in artifacts}, indent=2))
        (out / 'result.json').write_text(json.dumps({'passed': True, 'system': system, 'launch_route': 'compatibility-shell',
            'profile_launch_overrides_applied': False,
            'paired_native_steps': pairs, 'service_samples': len(service),
            'maximum_service_gap_ms': max_gap}, indent=2))
    finally:
        if session:
            session.close()
        listener.close()
        if pid:
            actual = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True)
            if actual.returncode == 0:
                assert actual.stdout == fingerprint if fingerprint else str(home) in actual.stdout
                terminate_owned(pid)
            absent = subprocess.run(['ps', '-p', str(pid)], capture_output=True).returncode != 0
            (out / 'exit.json').write_text(json.dumps({'pid': pid, 'exit_verified': absent}))
            assert absent


if __name__ == '__main__':
    main()
