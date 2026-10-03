"""Clock-origin restoration and destination-independence native witness."""
import hashlib
import json
from state_sections import StateSections


def exercise(call, out, system, boot_frames=0):
    assert system == 'pce'
    fields = dict(PSG={'lastts'}, CPU={'timestamp', 'timer_lastts'}, VCE={'last_ts', 'cd_event'})
    fields.update({'SCH' + str(i): {'lastts'} for i in range(6)})
    call('execution_speed', {'mode': 'unlimited'})
    call('step', {'frames': 120})
    if boot_frames:
        call('set_input', {'buttons': ['run']})
        call('step', {'frames': 6})
        call('set_input', {'buttons': []})
        call('step', {'frames': boot_frames})
    rows = []
    divergences = []
    for save_offset in [0, 37, 5000]:
        call('step', {'frames': 1})
        if save_offset: call('step_instructions', {'count': save_offset})
        snapshot = out / ('clock-origin-' + str(save_offset) + '.mcs')
        call('save_state', {'path': str(snapshot)})
        data = snapshot.read_bytes(); parsed = StateSections(data)
        assert parsed.encode() == data
        selected = dict(fields)
        if 'PECD' in parsed.sections: selected.update(PECD={'lastts'}, CDRM={'lastts'})
        saved_clocks = parsed.clocks(selected)
        legacy = out / ('clock-legacy-' + str(save_offset) + '.mcs')
        legacy.write_bytes(parsed.encode(selected))
        for kind, path in [('current', snapshot), ('legacy', legacy)]:
            expected = saved_clocks if kind == 'current' else {key: int(key == 'VCE.cd_event') for key in saved_clocks}
            baseline = None
            for destination, policy in zip([0, 1, 71, 5000],
                    [dict(mode='unlimited'), dict(mode='limited', percent=1),
                     dict(mode='limited', percent=10000), dict(mode='limited', percent=100)]):
                call('step', {'frames': 1})
                if destination: call('step_instructions', {'count': destination})
                call('execution_speed', policy)
                policy_before = call('execution_speed')
                call('load_state', {'path': str(path)})
                assert call('execution_speed') == policy_before
                restored = out / ('clock-restored-' + str(len(rows)) + '.mcs')
                call('save_state', {'path': str(restored)})
                actual = StateSections(restored.read_bytes()).clocks(selected)
                assert actual == expected, (kind, save_offset, destination, expected, actual)
                sequence = []
                for frame in range(4):
                    if frame: call('step', {'frames': 1})
                    state = call('get_state')['state']
                    png = call('screenshot')['png_base64']
                    observed_path = out / ('clock-observed-' + str(len(rows)) + '-' + str(frame) + '.mcs')
                    call('save_state', {'path': str(observed_path)})
                    observed = StateSections(observed_path.read_bytes())
                    byte_order = 'big' if observed.big_endian else 'little'
                    master_clock = int.from_bytes(observed.sections['MAIN']['PCE_TimestampBase'], byte_order)
                    master_clock += int.from_bytes(observed.sections['CPU']['timestamp'], byte_order)
                    sequence.append(dict(state=state, master_clock=master_clock,
                        png_sha256=hashlib.sha256(png.encode()).hexdigest()))
                row = dict(kind=kind, save_offset=save_offset, destination=destination,
                    clocks=actual, policy=policy, sequence=sequence)
                if baseline is None: baseline = sequence
                differences = []
                for frame, (observed, reference) in enumerate(zip(sequence, baseline)):
                    if observed['master_clock'] != reference['master_clock']:
                        differences.append(dict(frame=frame, component='guest_master_clock'))
                    if observed['state'] != reference['state']:
                        differences.append(dict(frame=frame, component='registers'))
                    if observed['png_sha256'] != reference['png_sha256']:
                        differences.append(dict(frame=frame, component='image'))
                row['differences'] = differences
                if differences:
                    divergences.append(dict(kind=kind, save_offset=save_offset,
                                            destination=destination, differences=differences))
                rows.append(row); (out / 'clock-restore.json').write_text(json.dumps(rows, indent=2))
        assert snapshot.read_bytes() == data
    call('step', {'frames': 1})
    assert not divergences, ('restored continuation diverged', divergences)
    return rows
