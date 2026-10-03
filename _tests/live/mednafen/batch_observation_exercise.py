"""Sample every advertised window while checking complete native-state preservation."""
import json
from state_sections import StateSections


def exercise(call, out, identity):
    capability=identity['memory_batch_capability']
    system=call('status')['system']
    call('execution_speed',dict(mode='unlimited'))
    call('step',dict(frames=60))
    rows=[]
    for origin in ('frame','cpu'):
        if origin=='cpu': call('step_instructions',dict(count=5))
        before=out/f'{origin}-before.mcs';after=out/f'{origin}-after.mcs'
        call('save_state',dict(path=str(before)))
        original=StateSections(before.read_bytes()).sections
        boundary=call('status')['native_control']
        for window in capability['windows']:
            start=window['address'];end=start+window['length']-1
            addresses=[start,end,start]
            if system=='md' and window['memory_type']=='cpu':
                # Native bus, VDP, controller and FM peeks must preserve the park.
                addresses += [0xff0000,0xc00004,0xa10003,0xa04000]
            ranges=[dict(memory_type=window['memory_type'],address=address,length=1)
                    for address in addresses]
            result=call('read_memory_batch',dict(ranges=ranges))
            assert result['total_bytes']==len(ranges) and len(result['reads'])==len(ranges),result
            for index,(request,read) in enumerate(zip(ranges,result['reads'])):
                assert read['index']==index and all(read[k]==v for k,v in request.items())
                scalar=call('read_memory',request)
                assert scalar['hex']==read['hex'] and len(bytes.fromhex(read['hex']))==1
            rows.append(dict(origin=origin,window=window,reads=result['reads']))
            (out/'window-reads.json').write_text(json.dumps(rows,indent=2))
        call('save_state',dict(path=str(after)))
        current=StateSections(after.read_bytes()).sections
        changed=[f'{section}/{field}' for section,values in original.items()
                 for field,data in values.items() if current.get(section,{}).get(field)!=data]
        (out/f'{origin}-comparison.json').write_text(json.dumps(dict(changed=changed),indent=2))
        assert current==original,changed
        assert call('status')['native_control']==boundary
    assert call('step',dict(frames=3))['status']=='completed'
    (out/'result.json').write_text(json.dumps(dict(passed=True,windows=len(capability['windows']),
        origins=['frame','cpu'],window_observations=len(rows)),indent=2))
