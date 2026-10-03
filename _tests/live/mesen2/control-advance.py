#!/usr/bin/env python3
"""Opt-in native advance driver witness. Does not qualify wire/parent cancellation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time

from support import ROOT, LAUNCHER, Session, default_binary, require_ok, terminate_owned

WRAPPER = r'''
dofile(os.getenv('EMUCAP_CONTROL_ENTRY'))
local Advance = require('emucap_control_advance')
local Json = require('emucap_json')
local driver, started, cancelled, terminal, stopped, case_id
local root = os.getenv('EMUCAP_CONTROL_OUTPUT')
local function encode(t)
  local fields = {}
  for k,v in pairs(t) do
    fields[#fields+1] = '"' .. k .. '":' .. (type(v) == 'string' and ('"' .. v .. '"') or tostring(v))
  end
  return '{' .. table.concat(fields, ',') .. '}'
end
local function service()
  local s = emu.getControlState()
  if terminal then
    assert(s.halted and s.generation == stopped.generation)
    assert(s.ppuCycles == stopped.ppuCycles and s.instructionBoundaries == stopped.instructionBoundaries
      and s.haltedCpuSteps == stopped.haltedCpuSteps)
    if s.monotonicMs - stopped.monotonicMs >= 100 then
      terminal.stable_stop = true
      local f = assert(io.open(root .. '/result-' .. case_id .. '.json', 'wb'))
      f:write(encode(terminal)); f:close()
      driver, terminal = nil, nil
    end
    return
  end
  if not driver then
    local f = io.open(root .. '/command.json', 'rb'); if not f then return end
    local command = Json.decode(f:read('*a')); f:close()
    if command.id == case_id then return end
    assert(s.halted)
    case_id, started, cancelled = command.id, s.monotonicMs, command.cancel_ms
    driver = Advance.new(emu, command.unit, command.count)
  end
  if cancelled and s.monotonicMs - started >= cancelled then
    assert(driver:cancel()); cancelled = nil
  end
  local result, err = driver:poll(); assert(not err, err)
  if result then
    terminal, stopped = result, emu.getControlState()
    terminal.elapsed_ms = stopped.monotonicMs - started
  end
end
for _, event in ipairs({emu.eventType.controlIdle, emu.eventType.pacingIdle,
    emu.eventType.codeBreakIdle, emu.eventType.codeBreakIdleSavestate}) do
  emu.addEventCallback(service, event)
end
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
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    wrapper = out / 'observe.lua'; wrapper.write_text(WRAPPER)
    listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(30)
    port = listener.getsockname()[1]
    home = out / 'home'
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_EMU_HOME=str(home), MESEN_BIN=str(default_binary()),
               EMUCAP_MESEN_LUA=str(wrapper), EMUCAP_LAUNCH_WAIT='10',
               EMUCAP_POST_CONNECT_GRACE='0', EMUCAP_LOG=str(out / 'launch.log'),
               EMUCAP_CONTROL_OUTPUT=str(out),
               EMUCAP_CONTROL_ENTRY=str(ROOT / 'adapters/mesen2' / f'emucap-{entry}.lua'))
    session, pid, fingerprint = None, None, None
    try:
        launch = subprocess.run(['bash', str(LAUNCHER), profile['launch_plan']['content_path'],
                                 str(port), 'native-control-advance', system],
                                env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch-output.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mesen2' / str(port) / 'mesen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0])
        (out / 'identity.json').write_text(json.dumps(require_ok(session.request('hello'), 'hello'), indent=2))
        require_ok(session.request('pause'), 'pause')
        for case in [dict(id=1, unit='instructions', count=3),
                     dict(id=2, unit='frames', count=60, cancel_ms=250)]:
            speed = {'mode': 'unlimited'} if case['id'] == 1 else {'mode': 'limited', 'percent': 1}
            require_ok(session.request('execution_speed', speed), 'speed')
            temporary = out / 'command.tmp'; temporary.write_text(json.dumps(case)); temporary.rename(out / 'command.json')
            result_path = out / f"result-{case['id']}.json"
            deadline = time.monotonic() + 15
            while not result_path.exists() and time.monotonic() < deadline: time.sleep(0.02)
            result = json.loads(result_path.read_text())
            assert result['stable_stop'] and result['requested'] == case['count'], result
            if case['id'] == 1:
                assert result['status'] == 'completed' and result['count'] == 3, result
            else:
                assert result['status'] == 'interrupted' and result['count'] < 60, result
                assert result['reason'] == 'cancelled' and result['elapsed_ms'] <= 5250, result
        artifacts = [p for p in home.rglob('*') if p.is_file() and p.name in
                     ('Mesen', 'Mesen.dll', 'MesenCore.dylib', 'MesenCore.dll', 'MesenCore.so')]
        identities = {str(p.relative_to(out)): hashlib.sha256(p.read_bytes()).hexdigest() for p in artifacts}
        identities['driver'] = hashlib.sha256((ROOT / 'adapters/mesen2/emucap_control_advance.lua').read_bytes()).hexdigest()
        (out / 'artifacts.json').write_text(json.dumps(identities, indent=2))
        (out / 'result.json').write_text(json.dumps({'passed': True, 'system': system,
            'launch_route': 'compatibility-shell', 'profile_launch_overrides_applied': False,
            'wire_cancellation_qualified': False}, indent=2))
    finally:
        if session: session.close()
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
