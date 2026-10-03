#!/usr/bin/env python3
"""Opt-in native owned advance/stop witness; does not qualify public MCP cancellation."""
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
    parser.add_argument('--batch-observation', action='store_true',
                        help='Compare full native state around advertised-window reads')
    parser.add_argument('--track-read-failure', action='store_true',
                        help='Check native track acquisition errors and recovery at a frozen boundary')
    parser.add_argument('--psx-peek', action='store_true',
                        help='Verify unimplemented CPU device peeks reject before observation')
    parser.add_argument('--pcfx-peek', action='store_true',
                        help='Verify CPU reads preserve native state and reject the device aperture')
    parser.add_argument('--nothrottle-policy', action='store_true',
                        help='Check native nothrottle admission, policy preservation and progress')
    parser.add_argument('--sound', action='store_true',
                        help='Enable native host audio for profile qualification')
    parser.add_argument('--sustained-frames', type=int, default=0)
    parser.add_argument('--idle-fixture', action='store_true')
    parser.add_argument('--restore-idle', action='store_true')
    parser.add_argument('--restore-halted-md', action='store_true')
    parser.add_argument('--restore-halted-pcfx', action='store_true')
    parser.add_argument('--restore-halted-ss', action='store_true')
    parser.add_argument('--clock-restore', action='store_true')
    parser.add_argument('--restore-clock-ss', action='store_true')
    parser.add_argument('--save-publication', action='store_true')
    parser.add_argument('--restore-snapshot', type=Path,
                        help='Reload a retained restore-boundaries origin in a fresh process')
    parser.add_argument('--restore-snapshot-buttons', nargs='*', default=None,
                        help='Current frontend buttons for the separately launched restore witness')
    parser.add_argument('--restore-failure', action='store_true')
    parser.add_argument('--restore-failure-cpu', action='store_true')
    parser.add_argument('--restore-boundaries', action='store_true')
    parser.add_argument('--restore-boot-frames', type=int, default=60,
                        help='Frames before creating boundary comparison origins (1..5000)')
    parser.add_argument('--restore-speed-percent', type=int,
                        help='Limited speed for boundary continuation checks (default: unlimited)')
    parser.add_argument('--restore-compare-speeds', type=int, nargs='+',
                        help='Compare one origin at these limited rates against its reference speed')
    parser.add_argument('--restore-continuations', action='store_true')
    parser.add_argument('--restore-input-transition', action='store_true')
    parser.add_argument('--restore-input-release', action='store_true')
    parser.add_argument('--restore-frame-continuations', action='store_true')
    parser.add_argument('--restore-frame-counts', type=int, nargs='+', default=[1, 2, 3],
                        help='Successive frame advances for ordered continuation comparisons')
    parser.add_argument('--restore-origin-instructions', type=int, default=5,
                        help='CPU-origin offset for the native restoration witness (1..5000)')
    parser.add_argument('--clock-boot-frames', type=int, default=0,
                        help='For PCE CD clock checks, press RUN and advance this many frames first')
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert 0 <= args.sustained_frames <= 5000
    assert 0 <= args.clock_boot_frames <= 5000
    assert not args.clock_boot_frames or args.clock_restore
    assert sum((args.restore_halted_md, args.restore_halted_pcfx, args.restore_halted_ss)) <= 1
    assert not (args.restore_halted_md or args.restore_halted_pcfx or args.restore_halted_ss) or not (args.idle_fixture or args.restore_boundaries or args.restore_failure or args.clock_restore or args.sustained_frames)
    assert not args.restore_idle or args.idle_fixture
    assert 1 <= args.restore_boot_frames <= 5000
    assert args.restore_boot_frames == 60 or args.restore_boundaries
    assert args.restore_speed_percent is None or (args.restore_boundaries
        and 1 <= args.restore_speed_percent <= 10000)
    assert args.restore_compare_speeds is None or (args.restore_boundaries
        and (args.restore_continuations or args.restore_frame_continuations)
        and all(1 <= rate <= 10000 for rate in args.restore_compare_speeds))
    assert 1 <= args.restore_origin_instructions <= 5000
    assert args.restore_origin_instructions == 5 or args.restore_boundaries
    assert all(1 <= count <= 300 for count in args.restore_frame_counts)
    assert len(set(args.restore_frame_counts)) == len(args.restore_frame_counts), "frame counts name distinct evidence files"
    assert args.restore_frame_counts == [1, 2, 3] or args.restore_frame_continuations
    assert not (args.restore_continuations and args.restore_frame_continuations)
    assert not (args.restore_continuations or args.restore_frame_continuations) or args.restore_boundaries
    assert not args.restore_input_transition or (args.restore_boundaries
        and (args.restore_continuations or args.restore_frame_continuations))
    assert not args.restore_input_release or args.restore_input_transition
    assert not args.restore_failure_cpu or args.restore_failure
    assert not args.restore_failure or not (args.clock_restore or args.idle_fixture or args.sustained_frames or args.restore_boundaries)
    assert not args.restore_boundaries or not (args.clock_restore or args.idle_fixture or args.sustained_frames)
    assert not args.restore_clock_ss or not (args.restore_boundaries or args.restore_failure or args.clock_restore or args.idle_fixture or args.sustained_frames or args.restore_halted_md or args.restore_halted_pcfx or args.restore_halted_ss)
    assert not args.save_publication or not any((args.restore_boundaries, args.restore_failure,
        args.restore_clock_ss, args.restore_halted_md, args.restore_halted_pcfx,
        args.restore_halted_ss, args.clock_restore, args.idle_fixture, args.sustained_frames))
    assert not args.restore_snapshot or not any((args.save_publication, args.restore_boundaries,
        args.restore_failure, args.restore_clock_ss, args.restore_halted_md,
        args.restore_halted_pcfx, args.restore_halted_ss, args.clock_restore,
        args.idle_fixture, args.sustained_frames))
    assert args.restore_snapshot_buttons is None or args.restore_snapshot
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = ROOT / 'adapters/mednafen/work/mednafen/src/mednafen'
    listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(15)
    port = listener.getsockname()[1]; home = out / 'home'
    runtime = 'native-advance-' + uuid.uuid4().hex
    env = dict(os.environ, **profile.get('env', {}))
    env.update(EMUCAP_EMU_HOME=str(home), EMUCAP_START_FROZEN='1', EMUCAP_HEADLESS='1',
               MEDNAFEN_SOUND='1' if args.sound else '0',
               EMUCAP_POST_CONNECT_GRACE='0', EMUCAP_LAUNCH_ID=runtime)
    system = profile['launch_plan']['system']; module = 'ss' if system == 'saturn' else system
    session = None; pid = None; fingerprint = None; rows = []; pending = {}; serial = 0

    if args.pcfx_peek or args.psx_peek or args.batch_observation or args.track_read_failure:
        assert sum((args.pcfx_peek, args.psx_peek, args.batch_observation, args.track_read_failure)) == 1
        assert not any((args.nothrottle_policy, args.restore_boundaries, args.restore_failure,
            args.clock_restore, args.sustained_frames, args.idle_fixture, args.save_publication,
            args.restore_snapshot, args.restore_clock_ss, args.restore_halted_md,
            args.restore_halted_ss, args.restore_halted_pcfx))
    if args.nothrottle_policy:
        assert not any((args.restore_boundaries, args.restore_failure, args.clock_restore,
            args.sustained_frames, args.idle_fixture, args.save_publication,
            args.restore_snapshot, args.restore_clock_ss, args.restore_halted_md,
            args.restore_halted_ss, args.restore_halted_pcfx))
        runtime_home = home / 'mednafen' / str(port)
        runtime_home.mkdir(parents=True)
        (runtime_home / 'mednafen.cfg').write_text('nothrottle 1\n')

    def save():
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))

    def send(method, params=None):
        nonlocal serial
        serial += 1
        request = dict(v=1, id=serial, method=method, params=params or {})
        session.socket.sendall(json.dumps(request).encode() + b'\n')
        rows.append(dict(request=request)); save()
        return serial

    def receive(wanted, expect_ok=True):
        while wanted not in pending:
            line = session.file.readline()
            assert line, 'connection closed'
            response = json.loads(line); rows.append(dict(response=response)); save()
            result = response.get('result')
            if not isinstance(result, dict) or result.get('status') != 'working':
                pending[response['id']] = response
        response = pending.pop(wanted)
        assert response['ok'] == expect_ok, response
        return response['result' if expect_ok else 'error']

    def call(method, params=None):
        return receive(send(method, params))

    def reject(method, params=None):
        return receive(send(method, params), expect_ok=False)

    def reconnect():
        nonlocal session
        session.close(); session = Session(listener.accept()[0]); pending.clear()
        call('hello')

    def key(name):
        return dict(runtime=runtime, owner_id='witness', operation_id=name)

    def begin(name):
        parent = key(name); call('begin_temporal_operation', dict(parent=parent)); return parent

    def step(parent, name, count, instructions=False):
        child = key(name)
        params = dict(_temporal_owner=parent, _control=child)
        params['count' if instructions else 'frames'] = count
        return send('step_instructions' if instructions else 'step', params), child

    def stopped():
        state = call('status')
        assert state['state'] == 'frozen' and state['native_control']['parked'], state
        time.sleep(.06)
        later = call('status')
        assert later['native_control'] == state['native_control'], (state, later)
        return state

    def finish(parent, ports):
        result = call('finish_temporal_operation', dict(parent=parent))
        assert result['cleanup_verified'] and result['released_ports'] == ports, result
        return result

    try:
        launch = subprocess.run(['bash', str(ROOT / 'adapters/mednafen/launch.sh'),
            profile['launch_plan']['content_path'], str(port), 'native-advance', module],
            env=env, capture_output=True, text=True, timeout=30)
        (out / 'launch.txt').write_text(launch.stdout + launch.stderr)
        pidfile = home / 'mednafen' / str(port) / 'mednafen.pid'
        if pidfile.exists(): pid = int(pidfile.read_text())
        assert launch.returncode == 0 and pid, launch.stderr
        fingerprint = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart=,command='], text=True)
        session = Session(listener.accept()[0]); identity = call('hello')
        (out / 'artifacts.json').write_text(json.dumps(dict(
            profile=dict(sound=args.sound, headless=True),
            binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), hello=identity,
            sidecar=json.loads(Path(str(binary) + '.emucap-build.json').read_text())), indent=2))
        assert identity.get('control_session_lifecycle') is True
        assert identity['temporal_cancellation_capability'] == dict(
            methods=['step', 'step_instructions'], control_service_ms=50, stop_host_ms=5000)
        if args.psx_peek:
            from psx_peek_exercise import exercise
            exercise(call, reject, out, identity)
            return
        if args.track_read_failure:
            from track_read_exercise import exercise
            exercise(call, out, identity)
            return
        if args.batch_observation:
            from batch_observation_exercise import exercise
            exercise(call, out, identity)
            return
        if args.pcfx_peek:
            assert system == 'pcfx' and not args.nothrottle_policy
            from pcfx_peek_exercise import exercise
            exercise(call, reject, out, identity)
            return
        if args.nothrottle_policy:
            from pacing_override_exercise import exercise
            exercise(call, reject, out, sound=args.sound)
            return
        if args.restore_snapshot:
            from snapshot_relaunch_exercise import exercise
            exercise(call, out, args.restore_snapshot.resolve(), args.restore_snapshot_buttons)
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
                snapshot_relaunch=True)))
            return
        if args.save_publication:
            from save_publication_exercise import exercise
            exercise(call, reject, out)
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
                save_publication=True)))
            return
        if args.restore_clock_ss:
            from ss_clock_restore_exercise import exercise
            assert system == 'saturn'
            exercise(call, out)
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system, restore_clock_ss=True)))
            return
        if args.restore_boundaries:
            from restore_boundary_exercise import exercise
            exercise(call, out, continuations=args.restore_continuations or args.restore_frame_continuations,
                     origin_instructions=args.restore_origin_instructions,
                     input_transition=args.restore_input_transition,
                     released_input=args.restore_input_release,
                     speed_percent=args.restore_speed_percent,
                     compare_speeds=args.restore_compare_speeds,
                     boot_frames=args.restore_boot_frames,
                     frame_counts=args.restore_frame_counts,
                     continuation_unit='frames' if args.restore_frame_continuations else 'instructions')
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
                restore_boundaries=True)))
            return
        if args.restore_failure:
            from restore_failure_exercise import exercise
            exercise(call, reject, reconnect, out, system, args.restore_failure_cpu, key('quarantine-parent'))
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
                restore_failure=True, cpu_park=args.restore_failure_cpu)))
            return
        if args.restore_halted_md or args.restore_halted_pcfx or args.restore_halted_ss:
            from state_sections import StateSections
            assert system == ('md' if args.restore_halted_md else 'pcfx' if args.restore_halted_pcfx else 'saturn')
            call('execution_speed', {'mode': 'unlimited'})
            call('step', {'frames': 60})
            if system == 'saturn':
                call('step_instructions', {'count': 5})
            original = out / 'origin.mcs'
            call('save_state', {'path': str(original)})
            parsed = StateSections(original.read_bytes())
            byte_order = 'big' if parsed.big_endian else 'little'
            if system == 'saturn':
                # Native external-halt pipeline phase; architecture and ROM stay intact.
                fields = parsed.sections['SH2-M']
                pipe = int.from_bytes(fields['Pipe_ID'], byte_order)
                pending_irq = int.from_bytes(fields['EPending'], byte_order)
                fields['Pipe_ID'] = ((pipe & 0x00FFFFFF) | 0xFE000000).to_bytes(4, byte_order)
                fields['EPending'] = (pending_irq | 0xFF800000).to_bytes(4, byte_order)
                fields['ExtHalt'] = bytes([1])
                fields['ExtHaltDMA'] = bytes([fields['ExtHaltDMA'][0] | 1])
            else:
                section, field, value = ('M68K', 'XPending', 0x400) if system == 'md' else ('V810', 'Halted', 2)
                width = 4 if system == 'md' else 1
                assert len(parsed.sections[section][field]) == width
                # Native data-only fatal/error halt; idle clocks can continue.
                parsed.sections[section][field] = value.to_bytes(width, byte_order)
            halted = out / 'error-halted.mcs'; halted.write_bytes(parsed.encode())
            call('step_instructions', {'count': 1})
            callbacks = call('status')['native_control']['cpu_callbacks']
            for _ in range(2):
                call('load_state', {'path': str(halted)})
                native = call('status')['native_control']
                assert native['parked'] and native['context'] == 'restored_continuation'
                assert native['cpu_callbacks'] == callbacks
            before = call('get_state')['state']
            cpu_fields = ('P_REG', 'S_REG', 'PC', 'Halted', 'lastop', 'src_cache',
                          'dst_cache', 'have_src_cache', 'have_dst_cache', 'in_bstr', 'in_bstr_to')
            cpu_section = 'V810' if system == 'pcfx' else 'SH2-M'
            if system == 'saturn':
                cpu_fields = ('R', 'PC', 'CtrlRegs', 'SysRegs', 'Pipe_ID', 'ExtHalt')
            if system in ('pcfx', 'saturn'):
                cpu_before_path = out / 'halted-before.mcs'
                call('save_state', {'path': str(cpu_before_path)})
                cpu_before = StateSections(cpu_before_path.read_bytes()).sections[cpu_section]
            call('execution_speed', {'mode': 'limited', 'percent': 1})
            parent = begin(system + '-halted-parent')
            op, child = step(parent, system + '-halted-child', 3, True)
            time.sleep(.08)
            cancel = call('cancel_operation', child); result = receive(op)
            finish(parent, [])
            after = call('get_state')['state']; native = call('status')['native_control']
            (out / 'halted-restoration.json').write_text(json.dumps(dict(
                before=before, after=after, cancel=cancel, terminal=result, native=native), indent=2))
            assert result['status'] == 'interrupted' and result['count'] == 0
            assert native['cpu_callbacks'] == callbacks
            if system in ('pcfx', 'saturn'):
                # The debugger g0 group also contains live IRQ, timer and pad
                # devices. Compare the native CPU architecture/transfer fields.
                cpu_after_path = out / 'halted-after.mcs'
                call('save_state', {'path': str(cpu_after_path)})
                cpu_after = StateSections(cpu_after_path.read_bytes()).sections[cpu_section]
                assert all(cpu_before[field] == cpu_after[field] for field in cpu_fields)
            else:
                assert before == after
            call('load_state', {'path': str(original)})
            call('step_instructions', {'count': 1})
            assert call('status')['native_control']['parked']
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
                restored_halt=True)))
            return
        if args.idle_fixture:
            assert system == 'wswan'
            call('step', dict(frames=1))
            assert call('read_memory', dict(memory_type='ram', address=256, length=2))['hex'].lower() == '3412'
            before = call('get_state')['state']
            if args.restore_idle:
                path = out / 'halted-origin.mcs'
                call('save_state', {'path': str(path)})
                call('reset')
                call('step_instructions', {'count': 1})
                origin = call('status')['native_control']
                restored = []
                for _ in range(2):
                    call('load_state', {'path': str(path)})
                    observed = call('status')['native_control']
                    assert observed['context'] == 'restored_continuation' and observed['parked']
                    assert observed['cpu_callbacks'] == origin['cpu_callbacks']
                    assert call('get_state')['state'] == before
                    restored.append(observed)
                (out / 'idle-restoration.json').write_text(json.dumps(restored, indent=2))
            call('execution_speed', dict(mode='limited', percent=1))
            parent = begin('idle-parent'); op, child = step(parent, 'idle-child', 3, True)
            time.sleep(.08)
            cancel = call('cancel_operation', child); result = receive(op)
            finish(parent, [])
            after = call('get_state')['state']
            marker = call('read_memory', dict(memory_type='ram', address=256, length=2))['hex'].lower()
            (out / 'idle.json').write_text(json.dumps(dict(before=before, after=after, marker=marker,
                cancel=cancel, terminal=result), indent=2))
            assert marker == '3412', marker
            assert result['status'] == 'interrupted' and result['count'] == 0, result
            (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system, idle=True)))
            return
        origin = stopped()['native_control']; parent = begin('instruction-parent')
        op, _ = step(parent, 'cold', 3, True); result = receive(op)
        assert result['count'] == 3 and result['status'] == 'completed', result
        cold = stopped()['native_control']
        assert cold['cpu_callbacks'] - origin['cpu_callbacks'] == 4, (origin, cold)
        op, _ = step(parent, 'chained', 7, True); result = receive(op)
        chained = stopped()['native_control']
        assert result['count'] == 7 and chained['cpu_callbacks'] - cold['cpu_callbacks'] == 7, (result, cold, chained)
        finish(parent, [])
        call('execution_speed', dict(mode='unlimited'))
        parent = begin('frame-parent'); before = stopped()['native_control']
        op, _ = step(parent, 'frames', 3); result = receive(op)
        after = stopped()['native_control']
        assert result['status'] == 'completed' and result['count'] == 3, result
        assert after['completed_frames'] - before['completed_frames'] == 3, (before, after)
        finish(parent, [])
        if args.clock_restore:
            from clock_restore_exercise import exercise as check_clocks
            check_clocks(call, out, system, args.clock_boot_frames)
        if args.sustained_frames:
            from owned_advance_exercise import exercise
            exercise(call, begin, step, receive, finish, stopped, out, args.sustained_frames, system)
        call('execution_speed', dict(mode='limited', percent=1))
        parent = begin('cancel-parent')
        call('set_input', dict(buttons=['cross' if system == 'psx' else 'a'], _temporal_owner=parent))
        op, child = step(parent, 'cancel-child', 5000)
        time.sleep(.12); start = time.monotonic()
        assert call('cancel_operation', key('stale-child'))['status'] == 'not_active'
        assert call('cancel_operation', child)['status'] == 'requested'
        result = receive(op)
        assert result['status'] == 'interrupted' and result['count'] < 5000 and result['reason'] == 'cancelled', result
        finish(parent, [0]); elapsed = time.monotonic() - start
        assert elapsed < 5, elapsed
        assert stopped()['native_input_mask'] == 0
        # Parent admission may precede the first native park.
        call('resume'); parent = begin('running-parent')
        call('pause', dict(_temporal_owner=parent)); stopped()
        op, _ = step(parent, 'finish-child', 5000); time.sleep(.12)
        finish_id = send('finish_temporal_operation', dict(parent=parent))
        result = receive(op); terminal = receive(finish_id)
        assert result['status'] == 'interrupted' and result['count'] < 5000, result
        assert terminal['cleanup_verified'] and terminal['released_ports'] == [], terminal
        stopped()
        # A disconnect during native execution must finish cleanup before a replacement is admitted.
        parent = begin('eof-parent')
        call('set_input', dict(buttons=['cross' if system == 'psx' else 'a'], _temporal_owner=parent))
        step(parent, 'eof-child', 5000); time.sleep(.12)
        session.close(); session = Session(listener.accept()[0]); pending.clear()
        call('hello'); assert stopped()['native_input_mask'] == 0
        parent = begin('replacement'); finish(parent, [])
        (out / 'result.json').write_text(json.dumps(dict(passed=True, system=system,
            cancel_cleanup_seconds=elapsed, sustained_frames=args.sustained_frames, clock_restore=args.clock_restore), indent=2))
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
