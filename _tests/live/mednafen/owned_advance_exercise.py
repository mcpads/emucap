"""Optional sustained native workload and breakpoint checks for owned-advance.py."""
import hashlib
import json
import time


def exercise(call, begin, step, receive, finish, stopped, out, frames, system):
    call('execution_speed', {'mode': 'unlimited'})
    call('step', {'frames': 1})
    trace_supported = 'set_trace' in call('hello')['methods']
    if trace_supported: call('set_trace', {'enabled': True})
    bp = call('set_breakpoint', {'kind': 'exec', 'memory_type': 'cpu',
        'start': 0, 'end': {'pce': 0xffff, 'wswan': 0xffff, 'pcfx': 0xfffffffe, 'ngp': 0xffffff}.get(system, 0xffffffff), 'pause_on_hit': True})['id']
    parent = begin('breakpoint-parent')
    op, _ = step(parent, 'breakpoint-child', 5000)
    result = receive(op)
    assert result['status'] == 'interrupted' and result['reason'] == 'breakpoint', result
    finish(parent, [])
    stopped(); call('clear_breakpoint', {'id': bp})
    trace = call('get_trace', {'count': 32})['trace'] if trace_supported else []
    parent = begin('trace-parent'); op, _ = step(parent, 'trace-child', 10, True)
    continued = receive(op); assert continued['status'] == 'completed' and continued['count'] == 10
    finish(parent, [])
    if trace_supported:
        assert len(call('get_trace', {'count': 32})['trace']) > len(trace)
        call('set_trace', {'enabled': False})
    call('step', {'frames': 1})
    snapshot = out / 'sustained-origin.mcs'
    call('save_state', {'path': str(snapshot)})
    runs = []
    # Same native origin; AB/BA order includes one warmup pair, then two measured pairs.
    for index, owned in enumerate([False, True, True, False, False, True]):
        call('load_state', {'path': str(snapshot)})
        before = stopped()['native_control']
        parent = begin('sustained-parent-' + str(index)) if owned else None
        start = time.monotonic()
        if owned:
            op, _ = step(parent, 'sustained-child-' + str(index), frames)
            result = receive(op)
        else:
            result = call('step', {'frames': frames})
        elapsed = time.monotonic() - start
        assert result['status'] == 'completed', result
        if owned:
            assert result['count'] == frames, result
            finish(parent, [])
        after = stopped()['native_control']
        assert after['completed_frames'] - before['completed_frames'] == frames, (before, after)
        state = call('get_state')['state']
        png = call('screenshot')['png_base64']
        run = dict(owned=owned, warmup=index < 2, frames=frames, host_seconds=elapsed,
            frames_per_second=frames/elapsed, state=state,
            png_base64_sha256=hashlib.sha256(png.encode()).hexdigest(), terminal=result)
        runs.append(run)
        (out / 'sustained.json').write_text(json.dumps(runs, indent=2))
    for index, run in enumerate(runs):
        assert run['state'] == runs[0]['state'], ('native state diverged', index)
        assert run['png_base64_sha256'] == runs[0]['png_base64_sha256'], ('native image diverged', index)
    return runs
