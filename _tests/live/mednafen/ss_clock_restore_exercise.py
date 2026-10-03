"""Restore an active Saturn frame across both native clock divisors."""
import hashlib
import json
from state_sections import StateSections


def exercise(call, out):
    call('execution_speed', {'mode': 'unlimited'})
    call('step', {'frames': 60})

    def snapshot(name):
        path = out / (name + '.mcs')
        call('save_state', {'path': str(path)})
        return path, StateSections(path.read_bytes())

    baseline, _ = snapshot('baseline')
    fixtures = {}
    # Use the native pending-change owner; StartFrame applies each divisor and
    # updates all device ratios. No guest code or ROM bytes are replaced.
    for divisor in (61, 65):
        fixture = StateSections(baseline.read_bytes())
        old = fixture.sections['SMPC']['PendingClockDivisor']
        fixture.sections['SMPC']['PendingClockDivisor'] = divisor.to_bytes(
            len(old), 'big' if fixture.big_endian else 'little')
        path = out / ('pending-' + str(divisor) + '.mcs')
        path.write_bytes(fixture.encode())
        fixtures[divisor] = path

    def destination(divisor, park):
        call('load_state', {'path': str(fixtures[divisor])})
        call('step', {'frames': 1})
        if park == 'cpu':
            call('step_instructions', {'count': 5000})
        _, state = snapshot('destination')
        assert state.clocks({'SMPC': ['CurrentClockDivisor']})['SMPC.CurrentClockDivisor'] == divisor

    rows = []
    for origin_divisor in (61, 65):
        destination(origin_divisor, 'cpu')
        origin, origin_state = snapshot('origin-' + str(origin_divisor))
        reference = None
        for divisor in (origin_divisor, 126 - origin_divisor):
            for park in ('frame', 'cpu'):
                destination(divisor, park)
                call('load_state', {'path': str(origin)})
                native = call('status')['native_control']
                assert native['parked'] and native['valid']
                name = str(origin_divisor) + '-from-' + str(divisor) + '-' + park
                _, restored = snapshot(name + '-immediate')
                sequence = []
                for count in (1, 7, 63):
                    call('step_instructions', {'count': count})
                    _, state = snapshot(name + '-after-' + str(count))
                    sequence.append({section: {field: hashlib.sha256(value).hexdigest()
                        for field, value in fields.items()} for section, fields in state.sections.items()})
                if reference is None:
                    reference = sequence
                differences = sorted({section + '.' + field
                    for actual, expected in zip(sequence, reference)
                    for section in actual.keys() | expected.keys()
                    for field in actual.get(section, {}).keys() | expected.get(section, {}).keys()
                    if actual.get(section, {}).get(field) != expected.get(section, {}).get(field)})
                rows.append(dict(origin_divisor=origin_divisor, destination_divisor=divisor,
                    destination_park=park, immediate_equal=restored.sections == origin_state.sections,
                    continuation_equal=not differences, changed_fields=differences,
                    native=native))
                (out / 'clock-restoration.json').write_text(json.dumps(rows, indent=2))
    assert all(row['immediate_equal'] and row['continuation_equal'] for row in rows), rows
