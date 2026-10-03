"""Checkpoint replacement and rejected destinations preserve owned native state."""
import hashlib
import json
from state_sections import StateSections


def exercise(call, reject, out):
    call('execution_speed', {'mode': 'unlimited'})
    call('step', {'frames': 60})
    rows = []
    for boundary in ('frame', 'cpu'):
        call('step', {'frames': 1})
        if boundary == 'cpu':
            call('step_instructions', {'count': 5000})
        path = out / (boundary + '.mcs')
        path.write_bytes(b'old checkpoint placeholder')
        before = call('status')
        state = call('get_state')
        result = call('save_state', {'path': str(path)})
        assert result['status'] == 'completed'
        saved = path.read_bytes()
        StateSections(saved)
        assert call('status')['native_control'] == before['native_control']
        assert call('get_state') == state
        link = out / (boundary + '-link.mcs')
        link.symlink_to(path)
        directory = out / (boundary + '-directory')
        directory.mkdir()
        marker = directory / 'keep'
        marker.write_bytes(b'preserve')
        errors = []
        for destination in (link, directory, out / 'missing-parent' / (boundary + '.mcs')):
            error = reject('save_state', {'path': str(destination)})
            assert error['kind'] == 'io_error', error
            assert path.read_bytes() == saved
            assert link.is_symlink() and marker.read_bytes() == b'preserve'
            assert call('get_state') == state
            after = call('status')
            assert after['native_control'] == before['native_control']
            assert after['execution_speed'] == before['execution_speed']
            assert not after.get('adapter_failure_active', False)
            errors.append(dict(path=str(destination), error=error))
        assert not list(out.glob('.emucap-state-*.tmp'))
        # An overwritten artifact remains accepted by the real staged loader.
        call('step', {'frames': 1})
        loaded = call('load_state', {'path': str(path)})
        assert loaded['status'] == 'completed' and call('status')['state'] == 'frozen'
        call('step', {'frames': 1})
        rows.append(dict(boundary=boundary, before=before, saved_state=state,
                         snapshot_sha256=hashlib.sha256(saved).hexdigest(),
                         rejected=errors, load=loaded, after=call('status')))
        (out / 'save-publication.json').write_text(json.dumps(rows, indent=2))
