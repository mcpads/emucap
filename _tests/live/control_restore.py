#!/usr/bin/env python3
"""Qualify an advertised execution breakpoint and repeated restore on an owned guest.

Profiles may specify debugger.pc_field (a dotted state key), memory_type and
breakpoint_frames, entry_instructions and state_path (empty for top-level CPU groups). Missing or ambiguous CPU observations require an explicit
profile; they never become a passing debugger witness. A producer-specific completion
receipt uses explicit debugger.load_status_path and load_status_values.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path

from observation_speed import ROOT, Witness, free_port, state_call, warmup
from input_pacing import Session


def flatten(value, prefix=''):
    result = {}
    for key, item in value.items():
        name = prefix + key
        if isinstance(item, dict):
            result.update(flatten(item, name + '.'))
        else:
            result[name] = item
    return result


def field_at_path(value, path):
    for key in path.split('.'):
        if key:
            value = value[key]
    return value


def cpu_state(w, status, selection):
    groups = selection.get('groups')
    if groups is None:
        groups = [g for g in ('cpu', 'g0', 'main', 'arm9', 'ee')
                  if g in status.get('state_groups', [])][:1]
    args = {'groups': groups} if groups else {}
    state = field_at_path(w.call('get_state', args), selection.get('state_path', 'state'))
    assert isinstance(state, dict), state
    return flatten(state)


def program_counter(state, selection):
    field = selection.get('pc_field')
    if field:
        value = state[field]
    else:
        candidates = {k: v for k, v in state.items()
                      if k.lower().split('.')[-1] == 'pc'}
        if not candidates:
            candidates = {k: v for k, v in state.items()
                          if k.lower().split('.')[-1] == 'eip'}
        assert len(candidates) == 1, {'need_pc_field': candidates, 'state_keys': list(state)}
        field, value = next(iter(candidates.items()))
    assert isinstance(value, int), (field, value)
    # The 65816 exposes PC and program bank separately.
    bank_field = selection.get('bank_field')
    if bank_field:
        value |= state[bank_field] << 16
    elif field == 'cpu.pc' and 'cpu.k' in state:
        value |= state['cpu.k'] << 16
    return value


def require_hit(w, breakpoint_id, frames):
    result = w.call('step', {'unit': 'frames', 'count': frames})
    stopped = w.call('status')
    events = w.call('poll_events')
    assert result['status'] == 'interrupted', result
    assert stopped['state'] == 'frozen', stopped
    assert any(event.get('breakpoint_id') == breakpoint_id
               for event in events.get('events', [])), events
    return result


def exercise(w, out, status, profile):
    selection = profile.get('debugger', {})
    capture_mode = profile.get('capture_mode', 'frozen_stable')
    capture_resume_frames = profile.get('capture_resume_frames', 1)
    assert isinstance(capture_resume_frames, int) and capture_resume_frames > 0
    assert capture_mode in ('frozen_stable', 'frozen_advancing', 'running'), capture_mode
    kinds = [k for k in status.get('breakpoint_kinds', []) if k.get('kind') == 'exec']
    assert kinds and 'set_breakpoint' in status['methods'], 'execution breakpoint not advertised'
    if selection.get('entry_instructions'):
        entered = w.call('step', {'unit': 'instructions', 'count': selection['entry_instructions']})
        assert entered['status'] == 'completed', entered
    state = cpu_state(w, status, selection)
    pc = program_counter(state, selection)
    # The public tool requires memory_type even when this native breakpoint kind ignores it.
    arguments = dict(kind='exec', start=pc, end=pc, pause_on_hit=True,
                     memory_type=selection.get('memory_type', status['memory_types'][0]))
    armed = w.call('set_breakpoint', arguments)
    breakpoint_id = armed['id']
    w.call('poll_events')
    require_hit(w, breakpoint_id, selection.get('breakpoint_frames', 60))
    halt_pc = program_counter(cpu_state(w, status, selection), selection)
    w.call('clear_breakpoint', {'id': breakpoint_id})
    # Some producers serialize only after the CPU/debugger callback has unwound.
    # The profile explicitly selects that advertised boundary; the save still owns admission.
    save_boundary = selection.get('save_boundary')
    if save_boundary:
        assert set(save_boundary) == {'unit', 'count'}
        boundary = w.call('step', save_boundary)
        assert boundary['status'] == 'completed' and boundary['count'] == save_boundary['count'], boundary
    snapshot = out / 'debugger.state'
    save_args = {'path': str(snapshot)}
    if 'instruction_snapshot_capture' in json.dumps(status):
        save_args['snapshot_key'] = 'windows-debugger-restore'
    saved, error = state_call(w, status, 'save_state', save_args)
    assert saved is not None, error
    assert saved.get('status', 'completed') in ('completed', 'saved'), saved
    assert snapshot.is_file() and snapshot.stat().st_size > 0
    snapshot_hash = hashlib.sha256(snapshot.read_bytes()).hexdigest()
    saved_pc = program_counter(cpu_state(w, status, selection), selection)
    restores = []
    for count in (1, 3):
        warmup(w, status, count)
        loaded, error = state_call(w, status, 'load_state', {'path': str(snapshot)})
        assert loaded is not None, error
        load_status = field_at_path(loaded, selection.get('load_status_path', 'status'))
        assert load_status in selection.get('load_status_values', ['completed', 'loaded']), loaded
        after = w.call('status')
        assert after['state'] == 'frozen' and after['connected'], after
        restored_pc = program_counter(cpu_state(w, status, selection), selection)
        if loaded.get('frame_counter_continuous') is not False:
            assert restored_pc == saved_pc, (restored_pc, saved_pc)
        elif restores:
            # Mupen64Plus services native loads by advancing to a frame boundary.
            assert restored_pc == restores[0]['restored_pc']
        image = out / f'restore-{count}.png'
        # Frame-serviced loads may return inside a native frame callback. Enter
        # guest execution before choosing the next executable breakpoint address,
        # just as at the initial halt; preserve the restored-PC assertion above.
        breakpoint_pc = restored_pc
        if selection.get('entry_instructions'):
            entered = w.call('step', {'unit': 'instructions', 'count': selection['entry_instructions']})
            assert entered['status'] == 'completed', entered
            breakpoint_pc = program_counter(cpu_state(w, status, selection), selection)
        armed = w.call('set_breakpoint', {**arguments, 'start': breakpoint_pc, 'end': breakpoint_pc})
        listed = w.call('list_breakpoints')
        assert any(bp['id'] == armed['id'] for bp in listed['breakpoints']), listed
        interruption = require_hit(w, armed['id'], selection.get('breakpoint_frames', 60))
        w.call('clear_breakpoint', {'id': armed['id']})
        warmup(w, status, capture_resume_frames)
        continued = w.call('status')
        assert continued['connected'] and continued['state'] == 'frozen', continued
        # Capture follows the PC/breakpoint proof and a completed frame. Some
        # producers populate their framebuffer only when execution resumes.
        if capture_mode == 'running':
            w.call('resume')
            try:
                w.call('screenshot', {'save_path': str(image)})
            finally:
                w.call('pause')
        else:
            w.call('screenshot', {'save_path': str(image)})
        captured = w.call('status')
        assert captured['connected'] and captured['state'] == 'frozen', captured
        assert image.is_file() and image.stat().st_size > 0
        restores.append({'capture_mode': capture_mode, 'load': loaded, 'restored_pc': restored_pc, 'breakpoint_address': breakpoint_pc, 'breakpoint_step': interruption, 'image_sha256': hashlib.sha256(image.read_bytes()).hexdigest(),
                         'continued_frame': continued['frame']})
    assert hashlib.sha256(snapshot.read_bytes()).hexdigest() == snapshot_hash
    return {'passed': True, 'halt_pc': halt_pc, 'saved_pc': saved_pc, 'save_boundary': save_boundary, 'breakpoint_address': pc, 'breakpoint': armed,
            'snapshot_sha256': snapshot_hash, 'save': saved, 'restores': restores}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    session = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        assert status['contracts']['state'] == 'validated', status
        (out / 'identity.json').write_text(json.dumps({'launch': session.launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 60))
        result = exercise(w, out, status, profile)
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    finally:
        try:
            if session and session.launch:
                (out / 'stop.json').write_text(json.dumps(session.stop(), indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
