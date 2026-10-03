"""A completed load must expose a generation-bound native frozen boundary."""
import json
import hashlib
import time
from itertools import product
from state_sections import StateSections


def exercise(call, out, continuations=False, origin_instructions=5, continuation_unit='instructions',
             input_transition=False, released_input=False, speed_percent=None,
             compare_speeds=None, boot_frames=60, frame_counts=(1, 2, 3)):
    policy = ({'mode': 'unlimited'} if speed_percent is None else
              {'mode': 'limited', 'percent': speed_percent})
    call('execution_speed', policy)
    call('step', {'frames': boot_frames})
    rows = []
    # Legacy PC-FX states retain the destination clock. New states restore
    # coordinated native origins, so compare their raw clocks without normalization.
    system = call('status')['system']
    pcfx = system == 'pcfx'
    source_button, destination_button = ('cross', 'circle') if system == 'psx' else ('a', 'b')
    destination_buttons = [] if released_input else [destination_button]

    def image_identity(label):
        import base64
        from PIL import Image
        path = out / (label + '.png')
        path.write_bytes(base64.b64decode(call('screenshot')['png_base64']))
        with Image.open(path) as image:
            rgb = image.convert('RGB')
            return dict(size=rgb.size, sha256=hashlib.sha256(rgb.tobytes()).hexdigest())

    def comparable(state, origin):
        result = dict(state)
        if (pcfx and not clock_origins) or (system == 'psx' and 'g0.TStamp' in result):
            result['g0.TStamp'] -= origin['g0.TStamp']
        return result
    def comparable_continuation(entry, origin):
        clock_origin = origin
        if system == 'psx' and entry['completed_frames']:
            clock_origin = dict(origin, **{'g0.TStamp': 0})
        result = dict(entry, state=comparable(entry['state'], clock_origin))
        # PSX saves these deadlines relative to its CPU park. At <= 0 the
        # unit has completed: every native consumer tests timestamp < deadline.
        # Keep positive deadlines and every other field exact; retain raw hashes
        # and decoded deadlines in the evidence rather than deleting them.
        deadlines = result.pop('psx_unit_deadlines', {})
        if deadlines:
            result['fields'] = dict(result['fields'])
            result['fields']['CPU'] = dict(result['fields']['CPU'])
            for field, deadline in deadlines.items():
                if deadline <= 0:
                    result['fields']['CPU'][field] = 'completed'
        next_event = result.pop('psx_next_event', None)
        if next_event is not None and 'g0.TStamp' in entry['state']:
            result['fields'] = dict(result['fields'])
            result['fields']['CPU'] = dict(result['fields']['CPU'])
            result['fields']['CPU']['next_event_ts'] = next_event - entry['state']['g0.TStamp']
        return result

    def continue_from(label):
        sequence = []
        frame_origin = call('status')['native_control']['completed_frames']
        counts = frame_counts if continuation_unit == 'frames' else (1, 7, 63)
        for count in counts:
            if continuation_unit == 'frames':
                call('step', {'frames': count})
            else:
                call('step_instructions', {'count': count})
            continued = out / (label + '-after-' + str(count) + '.mcs')
            call('save_state', {'path': str(continued)})
            parsed = StateSections(continued.read_bytes())
            sections = parsed.sections
            sequence.append(dict(**{continuation_unit: count},
                state=call('get_state')['state'],
                completed_frames=call('status')['native_control']['completed_frames'] - frame_origin,
                fields={section: {field: hashlib.sha256(value).hexdigest()
                    for field, value in fields.items()}
                    for section, fields in sections.items()}))
            if system == 'psx':
                deadlines = {}
                for field in ('gte_ts_done', 'muldiv_ts_done'):
                    data = sections['CPU'][field]
                    assert len(data) == 4
                    deadlines[field] = int.from_bytes(data,
                        'big' if parsed.big_endian else 'little', signed=True)
                sequence[-1]['psx_unit_deadlines'] = deadlines
                data = sections['CPU']['next_event_ts']
                assert len(data) == 4
                sequence[-1]['psx_next_event'] = int.from_bytes(data,
                    'big' if parsed.big_endian else 'little', signed=True)
            if continuation_unit == 'frames':
                import base64
                from PIL import Image
                path = continued.with_suffix('.png')
                try:
                    capture = call('screenshot')
                except AssertionError as error:
                    reply = error.args[0] if error.args else None
                    if not isinstance(reply, dict) or reply.get('error', {}).get('kind') != 'capture_failed':
                        raise
                    sequence[-1]['image_error'] = reply['error']
                else:
                    path.write_bytes(base64.b64decode(capture['png_base64']))
                    with Image.open(path) as image:
                        rgb = image.convert('RGB')
                        sequence[-1]['image'] = dict(size=list(rgb.size),
                            pixels_sha256=hashlib.sha256(rgb.tobytes()).hexdigest())
        return sequence

    for saved_at in ('frame', 'cpu'):
        call('execution_speed', policy)
        if input_transition:
            call('set_input', {'buttons': [source_button]})
        call('step', {'frames': 1})
        if saved_at == 'cpu':
            call('step_instructions', {'count': origin_instructions})
        snapshot = out / (saved_at + '-origin.mcs')
        call('save_state', {'path': str(snapshot)})
        saved_state = call('get_state')['state']
        saved_image = image_identity(saved_at + '-origin')
        saved_sections = StateSections(snapshot.read_bytes()).sections
        clock_origins = bool(any(saved_sections.get("MAIN", {}).get("ClockOriginsPresent", b"")))
        saved_status = call('status')
        assert saved_status['native_control']['parked'], saved_status
        if input_transition:
            call('set_input', {'buttons': destination_buttons})
            assert call('status')['input_override'] != saved_status['input_override']
        reference_continuation = None
        if continuations:
            uninterrupted = continue_from(saved_at + "-uninterrupted")
            reference_continuation = [comparable_continuation(entry, saved_state)
                                      for entry in uninterrupted]
            (out / (saved_at + "-uninterrupted.json")).write_text(json.dumps(uninterrupted, indent=2))
        for loaded_at, rate in product(('frame', 'cpu'), compare_speeds or (speed_percent,)):
            candidate_policy = ({'mode': 'unlimited'} if rate is None else
                                {'mode': 'limited', 'percent': rate})
            speed_reply = call('execution_speed', candidate_policy)
            effective = speed_reply['execution_speed']
            assert all(effective.get(k) == v for k, v in candidate_policy.items()), speed_reply
            label = saved_at + '-at-' + loaded_at
            if compare_speeds:
                label += '-speed-' + str(rate)
            if input_transition:
                call('set_input', {'buttons': [source_button]})
            call('step', {'frames': 1})
            if loaded_at == 'cpu':
                call('step_instructions', {'count': 7})
            if input_transition:
                call('set_input', {'buttons': destination_buttons})
            before = call('status')
            before_state = call('get_state')['state']
            result = call('load_state', {'path': str(snapshot)})
            after = call('status')
            time.sleep(.04)
            later = call('status')
            native = after['native_control']
            assert result['status'] == 'completed' and after['state'] == 'frozen'
            assert result['output_history'] == ('restored' if StateSections(snapshot.read_bytes()).history
                                                else 'unavailable')
            assert native['generation'] > before['native_control']['generation']
            assert later['native_control'] == native, (after, later)
            assert after['input_override'] == before['input_override']
            assert after['native_input_mask'] == before['native_input_mask']
            assert after['execution_speed'] == before['execution_speed']
            restored_state = call('get_state')['state']
            restored_image = image_identity(label + '-immediate')
            roundtrip = out / (label + '-roundtrip.mcs')
            call('save_state', {'path': str(roundtrip)})
            restored_sections = StateSections(roundtrip.read_bytes()).sections
            field_changes = []
            for section in sorted(saved_sections.keys() | restored_sections.keys()):
                expected = saved_sections.get(section, {})
                actual = restored_sections.get(section, {})
                for field in sorted(expected.keys() | actual.keys()):
                    if expected.get(field) != actual.get(field):
                        field_changes.append(section + '.' + field)
            rows.append(dict(saved_at=saved_at, loaded_at=loaded_at,
                reference_policy=policy, candidate_policy=candidate_policy,
                snapshot_sha256=hashlib.sha256(snapshot.read_bytes()).hexdigest(),
                origin_instructions=origin_instructions if saved_at == 'cpu' else 0,
                saved_state=saved_state, restored_state=restored_state,
                saved_image=saved_image, restored_image=restored_image,
                registers_preserved=(comparable(saved_state, saved_state)
                    == comparable(restored_state, restored_state)
                    and (not pcfx or clock_origins
                         or restored_state['g0.TStamp'] == before_state['g0.TStamp'])
                    and (system != 'psx' or 'g0.TStamp' not in restored_state
                         or restored_state['g0.TStamp'] == 0)),
                destination_state=before_state,
                changed_native_fields=field_changes,
                saved_status=saved_status, before=before, load=result, after=after,
                boundary_qualified=native['valid'] and native['parked']))
            # Reload while still parked in the acknowledgement itself. This
            # must reclassify the new generation without leaving the guest halt.
            call('load_state', {'path': str(snapshot)})
            repeated = call('status')
            repeated_native = repeated['native_control']
            assert repeated_native['generation'] > native['generation']
            assert repeated_native['completed_frames'] == native['completed_frames']
            assert call('get_state')['state'] == restored_state
            rows[-1]['repeated_status'] = repeated
            rows[-1]['repeated_image'] = image_identity(label + '-repeated')
            rows[-1]['image_preserved'] = (saved_image == restored_image == rows[-1]['repeated_image'])
            rows[-1]['boundary_qualified'] &= (repeated_native['valid']
                                               and repeated_native['parked'])
            if continuations:
                sequence = continue_from(label)
                rows[-1]['continuation_unit'] = continuation_unit
                rows[-1]['continuation'] = sequence
                normalized = [comparable_continuation(entry, restored_state)
                              for entry in sequence]
                rows[-1]['continuation_matches'] = normalized == reference_continuation
                if pcfx:
                    rows[-1]['clock_domain'] = ('V810 restored native origin' if clock_origins
                        else 'V810 elapsed cycles from retained destination origin')
            (out / 'restore-boundaries.json').write_text(json.dumps(rows, indent=2))
    # Reset uses the same native acknowledgement owner. Retain both origins'
    # register observations, but reset can preserve architectural registers
    # (for example M68K::Reset); raw equality is not a universal reset contract.
    if input_transition:
        call('set_input', {'buttons': []})
    call('step', {'frames': 1})
    call('reset')
    reset_reference = call('get_state')['state']
    call('step_instructions', {'count': 5})
    before_reset = call('status')
    call('reset')
    reset_status = call('status')
    reset_state = call('get_state')['state']
    reset_native = reset_status['native_control']
    reset_qualified = (reset_native['parked'] and reset_native['valid']
                       and reset_status['state'] == 'frozen')
    assert reset_native['generation'] > before_reset['native_control']['generation']
    (out / 'reset-boundaries.json').write_text(json.dumps(dict(
        reference=reset_reference, restored=reset_state, status=reset_status,
        registers_equal=reset_state == reset_reference, qualified=reset_qualified), indent=2))
    missing = [(r['saved_at'], r['loaded_at']) for r in rows if not r['boundary_qualified']]
    register_changes = [(r['saved_at'], r['loaded_at']) for r in rows
                        if not r['registers_preserved']]
    continuation_changes = [(r['saved_at'], r['loaded_at'], r['candidate_policy']) for r in rows
                            if r.get('continuation_matches') is False]
    capture_errors = [(r['saved_at'], r['loaded_at'], i)
                      for r in rows for i, entry in enumerate(r.get('continuation', []))
                      if 'image_error' in entry]
    # Native field changes are diagnostic: individual owners can canonicalize
    # valid encodings. Retain names and actual snapshots for a source audit.
    image_changes = [(r['saved_at'], r['loaded_at']) for r in rows if not r['image_preserved']]
    assert not missing and not register_changes and not continuation_changes and not capture_errors and not image_changes and reset_qualified, dict(
        changed_immediate_images=image_changes,
        missing_park_authority=missing, changed_registers=register_changes,
        changed_continuations=continuation_changes, capture_errors=capture_errors, reset_qualified=reset_qualified)
