"""Real native nothrottle config: policy admission, load retention and advancement."""
import json
import time


def exercise(call, reject, out, *, sound):
    initial = call('execution_speed')
    assert initial['mode'] == ('limited' if sound else 'custom'), initial
    assert initial['diagnostics']['nothrottle'] == (not sound), initial
    initial_status = call('status')
    assert initial_status['state'] == 'frozen'
    if sound:
        changed = call('execution_speed', dict(mode='limited', percent=50))
        assert changed['execution_speed']['percent'] == 50, changed
    else:
        for percent in (1, 50, 100, 10000):
            failure = reject('execution_speed', dict(mode='limited', percent=percent))
            assert 'failed_restored' in json.dumps(failure), failure
            assert call('execution_speed') == initial
            state = call('status')
            assert state['state'] == 'frozen'
            assert state['native_control'] == initial_status['native_control']
    changed = call('execution_speed', dict(mode='unlimited'))
    policy = changed['execution_speed']
    assert policy['mode'] == 'unlimited' and policy['percent'] is None
    assert call('execution_speed') == policy
    before = call('status')
    result = call('step', dict(frames=6))
    assert result['status'] == 'completed', result
    after = call('status')
    assert after['frame'] - before['frame'] == 6, (before, after)
    checkpoint = out / 'override.mcs'
    call('save_state', dict(path=str(checkpoint)))
    call('step', dict(frames=2))
    call('load_state', dict(path=str(checkpoint)))
    assert call('execution_speed') == policy
    call('step', dict(frames=3))
    parked = call('status')
    time.sleep(.08)
    assert call('status')['native_control'] == parked['native_control']
    if not sound:
        # A rejected limited request also preserves an already accepted unlimited policy.
        reject('execution_speed', dict(mode='limited', percent=50))
        assert call('execution_speed') == policy
    (out / 'result.json').write_text(json.dumps(dict(passed=True, sound=sound,
        initial=initial, accepted=policy, continuation=parked), indent=2))
