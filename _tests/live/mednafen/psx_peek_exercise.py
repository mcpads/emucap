"""PSX CPU aliases preserve memory reads and reject unimplemented device peeks."""
import json
from state_sections import StateSections


def exercise(call, reject, out, identity):
    assert call('status')['system']=='psx'
    windows=[w for w in identity['memory_batch_capability']['windows'] if w['memory_type']=='cpu']
    call('execution_speed',dict(mode='unlimited'));call('step',dict(frames=60))
    rows=[]
    for origin in ('frame','cpu'):
        if origin=='cpu':call('step_instructions',dict(count=5))
        boundary=call('status')['native_control']
        before=out/f'{origin}-before.mcs';after=out/f'{origin}-after.mcs'
        call('save_state',dict(path=str(before)))
        physical=call('read_memory',dict(memory_type='cpu',address=0x10000,length=16))
        for alias in (0,0x80000000,0xa0000000):
            assert call('read_memory',dict(memory_type='cpu',address=alias+0x10000,length=16))==physical
            good=dict(memory_type='cpu',address=alias+0x10000,length=1)
            for address,length in ((0x1f801023,2),(0x1f801024,1),(0x1f801810,4),(0x1f802fff,2)):
                bad=dict(memory_type='cpu',address=alias+address,length=length)
                assert not any(w['address']<=bad['address'] and bad['address']+length<=w['address']+w['length'] for w in windows)
                operations=[('read_memory',bad),('read_memory_batch',dict(ranges=[good,bad])),
                            ('find_pattern',dict(memory_type='cpu',start=bad['address'],length=length,hex='00'))]
                if origin=='frame':
                    operations.append(('probe',dict(bad,state=str(before),frame=0)))
                for method,params in operations:
                    error=reject(method,params);assert error['kind'] in ('bad_params','unsupported'),error
                    rows.append(dict(origin=origin,method=method,address=bad['address'],error=error))
            result=call('read_memory_batch',dict(ranges=[good,good]))
            assert result['reads'][0]['hex']==result['reads'][1]['hex']
        call('save_state',dict(path=str(after)))
        assert StateSections(before.read_bytes()).sections==StateSections(after.read_bytes()).sections
        assert call('status')['native_control']==boundary
    assert call('step',dict(frames=3))['status']=='completed'
    (out/'result.json').write_text(json.dumps(dict(passed=True,rejections=rows),indent=2))
