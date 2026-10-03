"""Load retained on-disk output history into a separately launched producer."""
import base64
import hashlib
import json
from PIL import Image


def exercise(call, out, snapshot, buttons=None):
    origin = snapshot.stem.removesuffix('-origin')
    assert origin in ('frame', 'cpu')
    call('execution_speed', {'mode': 'unlimited'})
    if buttons is not None:
        call('set_input', {'buttons': buttons})
    before = call('status')
    result = call('load_state', {'path': str(snapshot)})
    assert result['status'] == 'completed' and result['output_history'] == 'restored'
    after = call('status')
    assert after['execution_speed'] == before['execution_speed']
    assert after['input_override'] == before['input_override']
    assert after['native_input_mask'] == before['native_input_mask']

    def pixels(path):
        with Image.open(path) as image:
            rgb = image.convert('RGB')
            return dict(size=rgb.size, sha256=hashlib.sha256(rgb.tobytes()).hexdigest())

    rows = []
    for frames in (0, 1, 2, 3):
        if frames:
            call('step', {'frames': frames})
        expected = (snapshot.with_suffix('.png') if not frames else
                    snapshot.parent / f'{origin}-uninterrupted-after-{frames}.png')
        actual = out / f'after-{frames}.png'
        actual.write_bytes(base64.b64decode(call('screenshot')['png_base64']))
        rows.append(dict(frames=frames, expected=pixels(expected), actual=pixels(actual)))
        (out / 'snapshot-relaunch.json').write_text(json.dumps(dict(
            snapshot_sha256=hashlib.sha256(snapshot.read_bytes()).hexdigest(),
            load=result, images=rows), indent=2))
        assert rows[-1]['expected'] == rows[-1]['actual'], rows[-1]
