"""Inspect revision-5 snapshots from the unscaled Wii presentation fixture.

This is a fixture-specific witness reader, not a general Dolphin state decoder.
The native header/body admission path remains authoritative for loading states.
"""
import ctypes
import ctypes.util
import hashlib
from pathlib import Path
import struct


def read_body(path):
    data = Path(path).read_bytes()
    base = 32 + struct.unpack_from('<I', data, 28)[0]
    version, compression, offset, size = struct.unpack_from('<HHIQ', data, base)
    assert (version, offset) == (0x454d, 16), 'expected presentation-state header'
    assert data[base + 16:base + 32] == b'EMUCAPPS\x05\0\0\0\x10\0\0\0'
    payload = data[base + 32:]
    if compression == 0:
        assert len(payload) == size
        return payload
    assert compression == 1, 'unsupported fixture compression'
    name = ctypes.util.find_library('lz4')
    if not name:
        raise RuntimeError('Install the LZ4 shared library used by the native build')
    library = ctypes.CDLL(name)
    library.LZ4_decompress_safe.argtypes = [ctypes.c_char_p, ctypes.c_void_p,
                                          ctypes.c_int, ctypes.c_int]
    library.LZ4_decompress_safe.restype = ctypes.c_int
    # These small synthetic guests produce one native LZ4 block.
    length = struct.unpack_from('<i', payload)[0]
    assert 0 < length == len(payload) - 4 and 0 < size <= 0x7fffffff
    output = ctypes.create_string_buffer(size)
    assert library.LZ4_decompress_safe(payload[4:], output, length, size) == size
    return output.raw


def texture_header(height, layers, pixel_format, flags):
    return struct.pack('<7IB3xI', 640, height, 1, layers, 1, pixel_format, flags, 0,
                       640 * height * layers * 4)


def inspect_fixture_state(path, layers, samples):
    body = read_body(path)
    efb = body.index(texture_header(528, layers, 0, 0))
    assert body[efb - 5] == 1, 'required EFB missing'
    assert struct.unpack_from('<I', body, efb - 4)[0] == samples
    position = efb
    for _ in range(samples):
        for pixel_format in (0, 11):
            header = texture_header(528, layers, pixel_format, 0)
            assert body[position:position + 36] == header
            position += 36 + 640 * 528 * layers * 4
            assert position <= len(body)

    candidates = [body.find(texture_header(height, layers, 0, 1))
                  for height in (480, 240)]
    position = min(value for value in candidates if value >= 0)
    next_id, count = struct.unpack_from('<QI', body, position - 12)
    assert 0 < count <= (len(body) - position) // 105
    entries = {}
    for _ in range(count):
        config = struct.unpack_from('<7IB3xI', body, position)
        width, height, levels, entry_layers, entry_samples, fmt, _, _, length = config
        assert levels == entry_samples == 1 and fmt == 0
        assert length == width * height * entry_layers * 4
        end = position + 36 + length + 69
        assert end <= len(body)
        entry_id = struct.unpack_from('<Q', body, position + 36 + length + 44)[0]
        assert entry_id < next_id and entry_id not in entries
        entries[entry_id] = (config, body[position + 36:position + 36 + length])
        position = end
    assert struct.unpack_from('<I', body, position)[0] == 0x42
    position += 4
    for layout in ('<II', '<II', '<QI', '<II'):
        count = struct.unpack_from('<I', body, position)[0]
        position += 4 + count * struct.calcsize(layout)
        assert position <= len(body)
    assert struct.unpack_from('<I', body, position)[0] == 0x42
    presenter = position + 4
    root_id = struct.unpack_from('<Q', body, presenter + 41)[0]
    assert body[presenter + 65] == 1 and root_id in entries
    config, pixels = entries[root_id]
    assert config[3] == layers
    return {
        'image_sha256': hashlib.sha256(pixels).hexdigest(),
        'width': config[0], 'height': config[1], 'layers': layers, 'samples': samples,
        'immediate_field': body[presenter + 40],
    }
