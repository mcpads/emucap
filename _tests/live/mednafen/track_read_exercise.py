"""Opt-in PC-FX fixture containing a track tail that is not a valid data sector."""
import json
from state_sections import StateSections


def exercise(call, out, identity):
    assert call('status')['system']=='pcfx'
    tracks=[w for w in identity['memory_batch_capability']['windows']
            if w['memory_type'].startswith('track')]
    assert tracks
    call('execution_speed',dict(mode='unlimited'))
    call('step',dict(frames=60))
    failures=[]
    for origin in ('frame','cpu'):
        if origin=='cpu': call('step_instructions',dict(count=5))
        boundary=call('status')['native_control']
        before=out/f'{origin}-before.mcs';after=out/f'{origin}-after.mcs'
        call('save_state',dict(path=str(before)))
        for window in tracks:
            first=dict(memory_type=window['memory_type'],address=window['address'],length=1)
            tail=dict(first,address=window['address']+window['length']-1)
            expected=call('read_memory',first)
            try:
                call('read_memory_batch',dict(ranges=[first,tail,first]))
            except AssertionError as error:
                response=error.args[0]
                assert response['ok'] is False and 'result' not in response,response
                assert response['error']['message']=='CD track sector read failed.',response
                failures.append(dict(origin=origin,window=window,error=response['error']))
            assert call('read_memory',first)==expected
            assert call('status')['native_control']==boundary
        call('save_state',dict(path=str(after)))
        assert StateSections(before.read_bytes()).sections==StateSections(after.read_bytes()).sections
    assert {entry['origin'] for entry in failures}=={'frame','cpu'}, failures
    assert call('step',dict(frames=3))['status']=='completed'
    (out/'result.json').write_text(json.dumps(dict(passed=True,failures=failures),indent=2))
