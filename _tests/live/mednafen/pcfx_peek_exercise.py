"""Check live PC-FX peeks against complete native-state fields."""
import json
from state_sections import StateSections


def exercise(call, reject, out, identity):
    windows=identity['memory_batch_capability']['windows']
    cpu=[w for w in windows if w['memory_type']=='cpu']
    assert cpu==[dict(memory_type='cpu',address=0,length=0x80000000),
                 dict(memory_type='cpu',address=0x80780000,length=0x7f880000)],cpu
    call('execution_speed',dict(mode='unlimited'))
    call('step',dict(frames=60))
    before=out/'before-peek.mcs';after=out/'after-peek.mcs'
    call('save_state',dict(path=str(before)))
    original=StateSections(before.read_bytes()).sections
    boundary=call('status')['native_control']
    rows=[]
    for address in (0,0x12000,0x1fffff,0xe0000006,0xe8000008,0xfff00000):
        scalar=call('read_memory',dict(memory_type='cpu',address=address,length=1))
        batch=call('read_memory_batch',dict(ranges=[dict(memory_type='cpu',address=address,length=1)]))
        assert batch['reads'][0]['hex']==scalar['hex']
        rows.append(dict(address=address,hex=scalar['hex']))
    for address,length in ((0x80000600,1),(0x7fffffff,2),(0x8077ffff,2)):
        reject('read_memory',dict(memory_type='cpu',address=address,length=length))
        reject('find_pattern',dict(memory_type='cpu',start=address,length=length,hex='00'))
        reject('read_memory_batch',dict(ranges=[
            dict(memory_type='cpu',address=0x13000,length=1),
            dict(memory_type='cpu',address=address,length=length)]))
    call('find_pattern',dict(memory_type='cpu',start=0x12000,length=16,hex='00'))
    call('save_state',dict(path=str(after)))
    current=StateSections(after.read_bytes()).sections
    changed=[f'{section}/{field}' for section,values in original.items()
             for field,data in values.items() if current.get(section,{}).get(field)!=data]
    (out/'peek-comparison.json').write_text(json.dumps(dict(changed=changed,reads=rows),indent=2))
    assert current==original,changed
    assert call('status')['native_control']==boundary
    assert call('step',dict(frames=3))['status']=='completed'
    (out/'result.json').write_text(json.dumps(dict(passed=True,native_fields_equal=True),indent=2))
