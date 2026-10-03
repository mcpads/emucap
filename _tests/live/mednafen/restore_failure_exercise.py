"""Native partial-load failure must retire guest access across reconnects."""
import json
from state_sections import StateSections


def exercise(call, reject, reconnect, out, system, cpu_park, parent):
    assert system == 'pce'
    call('execution_speed', {'mode': 'unlimited'})
    call('step', {'frames': 120})
    snapshot = out / 'origin.mcs'
    call('save_state', {'path': str(snapshot)})
    original = snapshot.read_bytes()
    broken = StateSections(original)
    assert 'PSG' in broken.sections
    del broken.sections['PSG']
    damaged = out / 'missing-psg.mcs'
    damaged.write_bytes(broken.encode())
    call('step', {'frames': 7})
    if cpu_park:
        call('step_instructions', {'count': 37})
    before = call('status')
    state = call('get_state')
    # Failure to open the file precedes the native loader and keeps the halt valid.
    error = reject('load_state', {'path': str(out / 'absent.mcs')})
    assert error['kind'] == 'io_error', error
    assert call('get_state') == state
    assert call('status')['native_control'] == before['native_control']
    # Complete file framing is checked before native generation/state mutation.
    import struct
    import hashlib
    malformed = [('truncated', original[:-1])]
    overrun = bytearray(original)
    struct.pack_into('<II', overrun, 24, 0xffffffff, 0xffffffff)
    malformed.append(('preview-overflow', bytes(overrun)))
    native_size = struct.unpack_from('<I', original, 20)[0] & 0x7fffffff
    if native_size < len(original):
        def signed(data):
            data[-32:] = hashlib.sha256(data[:-32]).digest()
            return bytes(data)
        corrupt = bytearray(original); corrupt[-1] ^= 1
        malformed.append(('history-digest', bytes(corrupt)))
        wrong_content = bytearray(original); wrong_content[native_size + 24] ^= 1
        malformed.append(('history-content', signed(wrong_content)))
        lengths = struct.unpack_from('<QQQQ', original, native_size + 40)
        offset = native_size + 72
        for label, length in zip(('raster', 'filters', 'completed'), lengths):
            corrupt = bytearray(original); corrupt[offset] ^= 1
            malformed.append(('history-' + label, signed(corrupt)))
            offset += length
        # A digest-valid but undecodable output block must also precede guest load.
        corrupt = bytearray(original[:offset]) + b'?' + bytearray(original[offset:])
        struct.pack_into('<Q', corrupt, native_size + 64, lengths[3] + 1)
        malformed.append(('history-output', signed(corrupt)))
    preflight = []
    for label, data in malformed:
        path = out / (label + '.mcs')
        path.write_bytes(data)
        error = reject('load_state', {'path': str(path)})
        assert error['kind'] == 'io_error', error
        after = call('status')
        assert after['native_control'] == before['native_control']
        assert after['execution_speed'] == before['execution_speed']
        assert not after.get('adapter_failure_active', False)
        assert call('get_state') == state
        preflight.append(dict(case=label, error=error, after=after))
    (out / 'preflight-rejection.json').write_text(json.dumps(preflight, indent=2))
    if native_size < len(original):
        legacy = out / 'native-only.mcs'
        legacy.write_bytes(original[:native_size])
        result = call('load_state', {'path': str(legacy)})
        assert result['status'] == 'completed' and result['output_history'] == 'unavailable'
    error = reject('load_state', {'path': str(damaged)})
    assert error['kind'] == 'restore_unverified', error
    assert 'PSG' in error['message'], error
    rows = []
    for attachment in range(2):
        if attachment:
            reconnect()
        statuses = [call('status'), call('status')]
        for status in statuses:
            assert status['state'] == 'frozen', status
            assert status['adapter_failure_active'], status
            assert status['adapter_failure_operation'] == 'load_state', status
            assert not status['native_control']['valid'] and not status['native_control']['parked'], status
            assert status['execution_speed'] == before['execution_speed']
        rejected = []
        for method, params in [
                ('begin_temporal_operation', {'parent': parent}),
                ('step', {'frames': 1}), ('step_instructions', {'count': 1}),
                ('resume', {}), ('reset', {}), ('get_state', {}), ('screenshot', {}),
                ('read_memory', {'memory_type': 'cpu', 'address': 0, 'length': 1}),
                ('execution_speed', {'mode': 'limited', 'percent': 100}),
                ('set_input', {'buttons': ['a']}),
                ('load_state', {'path': str(snapshot)}), ('save_state', {'path': str(snapshot)})]:
            error = reject(method, params)
            assert error['kind'] == 'restore_unverified', (method, error)
            rejected.append(dict(method=method, error=error))
        after = call('status')
        assert after['native_control'] == statuses[0]['native_control']
        assert after['adapter_failure_active']
        assert snapshot.read_bytes() == original
        rows.append(dict(attachment=attachment, statuses=statuses, rejected=rejected, after=after))
        (out / 'restore-failure.json').write_text(json.dumps(rows, indent=2))
