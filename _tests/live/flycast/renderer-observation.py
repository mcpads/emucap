#!/usr/bin/env python3
"""Managed Flycast frame/CPU observations and restore under a supplied render profile.

Use an isolated native config in profile.env; preserve effective config and independent
writer-path evidence separately. Passing this witness does not prove that an RTT or
framebuffer writer was exercised. All generated media/state/evidence remains local.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observation_speed import ROOT, Witness, free_port
from input_pacing import Session


def advance(w, count):
    before = w.call('status')['frame']
    result = w.call('step', {'unit': 'frames', 'count': count})
    assert result['status'] == 'completed' and result['state'] == 'frozen', result
    assert result['frame'] == before + count, (before, count, result)
    return result


def check_partial_load_failure(w, out, checkpoint, ranges):
    """Distinguish harmless envelope rejection from a partially applied native load."""
    original = Path(checkpoint).read_bytes()
    assert original[:8] == b'EMUCAPFC' and len(original) > 48
    payload = bytearray(original[:len(original)//2])
    damaged = out/'truncated.state'
    damaged.write_bytes(payload)
    before = w.call('get_state')['state']
    rejected = w.call('load_state', {'path': str(damaged)}, error=True)
    assert rejected['error']['code'] == 'bad_params'
    assert w.call('status')['state'] == 'frozen'
    assert w.call('get_state')['state'] == before
    # Preserve the valid envelope length; fail inside the native deserializer.
    payload[16:24] = (len(payload)-24).to_bytes(8, 'little')
    damaged.write_bytes(payload)
    failure = w.call('load_state', {'path': str(damaged)}, error=True)
    assert failure['error']['code'] == 'io_error', failure
    quarantined = w.call('status')
    assert quarantined['state'] == 'unknown', quarantined
    denied = []
    for method, arguments in (
        ('read_memory_batch', {'ranges': ranges}), ('get_state', {}),
        ('read_memory', {'memory_type': 'ram', 'address': 0, 'length': 16}),
        ('save_state', {'path': str(out/'must-not-exist.state')}),
        ('load_state', {'path': checkpoint}), ('resume', {}),
        ('step', {'unit': 'frames', 'count': 1}),
    ):
        denied.append({'method': method, 'response': w.call(method, arguments, error=True)})
    for _ in range(2):
        current = w.call('status')
        assert current['state'] == 'unknown' and current['frame'] == quarantined['frame']
    assert not (out/'must-not-exist.state').exists()
    return {'preflight_rejected_without_cpu_change': rejected, 'native_load_failure': failure,
            'denied': denied, 'status_did_not_recover': True}


def check_renderer_fault(w, out, ranges, kind):
    """Delay only the real renderer thread; leave the socket owner runnable."""
    before = w.call('status')
    pid = int(before['runtime_instance']['emulator_pid'])
    gate = out/'renderer-wait-started'
    commands = out/'renderer-wait.lldb'
    commands.write_text("\n".join([
        'settings set show-statusline false',
        f'process attach --pid {pid}',
        "script import pathlib; renderer = next(t for t in lldb.process if t.GetName() == 'Flycast-rend'); "
        "assert lldb.process.SetSelectedThread(renderer); print('render_thread', renderer.GetThreadID(), renderer.GetName()); "
        + (f"pathlib.Path({str(gate)!r}).touch()" if kind == 'timeout' else ''),
        # The debugger's initial single-thread evaluation can interrupt sleep.
        # Finish the remaining wait when it retries with all threads runnable.
        'expression --all-threads true --timeout 10000000 -- '
        + ('{ unsigned int remaining = 4; while (remaining) remaining = (unsigned int)sleep(remaining); }'
         if kind == 'timeout' else '(void)emucap_renderer_failed("controlled renderer failure")'),
        'process detach', f"script pathlib.Path({str(gate)!r}).touch()", 'quit', '',
    ]))
    log = out/'renderer-wait-lldb.log'
    with log.open('w') as output:
        debugger = subprocess.Popen(['lldb', '--no-lldbinit', '--batch', '-s', str(commands)],
                                    stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic()+30
            while not gate.exists():
                assert debugger.poll() is None and time.monotonic() < deadline, log.read_text()
                time.sleep(.02)
            started = time.monotonic()
            failure = None
            if kind == 'timeout':
                failure = w.call('status', error=True)
                assert failure['error']['code'] == 'emulator_error', failure
                assert 'renderer completion unverified' in failure['error']['message']
            seconds = time.monotonic()-started
            if kind == 'timeout':
                assert 1.5 < seconds < 5, seconds
            unknown = w.call('status')
            assert unknown['state'] == 'unknown' and unknown['frame'] == before['frame']
            rejected = w.call('read_memory_batch', {'ranges': ranges}, error=True)
            assert debugger.wait(timeout=15) == 0, log.read_text()
            assert f'Process {pid} detached' in log.read_text() and 'error:' not in log.read_text(), log.read_text()
            after = w.call('status')
            assert after['state'] == 'unknown' and after['frame'] == before['frame']
            w.call('resume', error=True)
            files = list((out/'home/sessions').glob('*/generations/*/adapter-failure.json'))
            assert len(files) == 1, files
            artifact = json.loads(files[0].read_text())
            assert artifact['active'] and artifact['execution_state'] == 'unknown', artifact
            if kind == 'failure':
                assert artifact['operation'] == 'renderer' and artifact['reason'] == 'controlled renderer failure', artifact
            return {'injection': kind, 'artifact': artifact, 'failure': failure, 'seconds': seconds, 'batch_rejected': rejected,
                    'renderer_wait_returned_and_debugger_detached': True,
                    'uncertainty_retained_after_wait': True}
        finally:
            if debugger.poll() is None:
                debugger.terminate()
                try:
                    debugger.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    debugger.kill()
                    debugger.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--warmup-frames', type=int, default=900)
    faults = parser.add_mutually_exclusive_group()
    faults.add_argument('--check-partial-load-failure', action='store_true',
                        help='End the owned session by testing retained native-load quarantine')
    faults.add_argument('--check-renderer-timeout', action='store_true',
                        help='Use LLDB on the owned renderer thread to test its completion deadline')
    faults.add_argument('--check-renderer-failure', action='store_true',
                        help='Invoke the native failure publisher on the owned renderer thread')
    args = parser.parse_args()
    assert 1 <= args.warmup_frames <= 5000
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    profile = json.loads(args.profile.read_text())
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out/'home'))
    w = Witness(out, env)
    session = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile['launch']})
        session.start()
        identity = w.call('status')
        (out/'identity.json').write_text(json.dumps(identity, indent=2))
        assert 'native_renderer_fence' in identity['emulator_identity'].get('host_features', []), identity
        w.speed({'mode': 'limited', 'percent': 10000})
        warmed = advance(w, args.warmup_frames)
        ranges = [{'memory_type': m, 'address': 0, 'length': 16384}
                  for m in ('ram', 'vram', 'aica')]
        def observe():
            return (w.call('get_state')['state'], w.call('read_memory_batch', {'ranges': ranges})['reads'])
        origin = observe()
        checkpoint = str(out/'frame.state')
        w.call('save_state', {'path': checkpoint})
        continuations = []
        for percent in (50, 10000):
            w.speed({'mode': 'limited', 'percent': percent})
            advance(w, 3)
            w.call('load_state', {'path': checkpoint})
            assert observe() == origin
            continuations.append(advance(w, 3))
            w.call('load_state', {'path': checkpoint})
        w.call('resume')
        w.call('pause')
        assert w.call('status')['state'] == 'frozen'
        pc = w.call('get_state')['state']['cpu.pc']
        bp = w.call('set_breakpoint', {'kind': 'exec', 'memory_type': 'ram',
                                     'start': hex(pc), 'end': hex(pc), 'pause_on_hit': True})
        stopped = w.call('step', {'unit': 'frames', 'count': 3})
        assert stopped['status'] == 'interrupted' and stopped['state'] == 'frozen', stopped
        hit = w.call('poll_events')
        assert hit['events'] and w.call('get_state')['state']['cpu.pc'] == pc
        w.call('clear_breakpoint', {'id': bp['id']})
        cpu_origin = observe()
        checkpoint = str(out/'cpu.state')
        w.call('save_state', {'path': checkpoint})
        advance(w, 3)
        w.call('load_state', {'path': checkpoint})
        assert observe() == cpu_origin
        advance(w, 3)
        result = {'passed': True, 'warmup': warmed, 'warmup_count': args.warmup_frames,
                  'restore_memory_bytes': 49152, 'registers_restored': True,
                  'continuations': continuations, 'breakpoint_terminal': stopped,
                  'cpu_restore': True, 'profile_sha256': hashlib.sha256(args.profile.read_bytes()).hexdigest()}
        if args.check_partial_load_failure:
            result['partial_load_failure'] = check_partial_load_failure(w, out, checkpoint, ranges)
        if args.check_renderer_timeout:
            result['renderer_timeout'] = check_renderer_fault(w, out, ranges, 'timeout')
        if args.check_renderer_failure:
            result['renderer_failure'] = check_renderer_fault(w, out, ranges, 'failure')
        (out/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    finally:
        try:
            if session:
                (out/'stop.json').write_text(json.dumps(session.stop(), indent=2)+'\n')
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
