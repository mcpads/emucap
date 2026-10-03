"""Named native save-state sections, for local runtime fault-injection witnesses."""
import struct
import hashlib


class StateSections:
    def __init__(self, data):
        assert data[:8] == b'MDFNSVST'
        total, width, height = struct.unpack_from('<III', data, 20)
        self.big_endian = bool(total & 0x80000000)
        native_size = total & 0x7fffffff
        assert 32 <= native_size <= len(data)
        self.history = data[native_size:]
        if self.history:
            assert len(self.history) >= 104 and self.history[:8] == b'ECHSTATE'
            assert hashlib.sha256(data[:-32]).digest() == data[-32:]
            lengths = struct.unpack_from('<QQQQ', self.history, 40)
            assert 72 + sum(lengths) + 32 == len(self.history)
        data = data[:native_size]
        offset = 32 + width * height * 3
        self.prefix = data[:offset]
        self.sections = {}
        while offset < len(data):
            name = data[offset:offset+32].split(b'\0', 1)[0].decode()
            size, = struct.unpack_from('<I', data, offset+32)
            offset += 36; end = offset + size
            assert end <= len(data) and name not in self.sections
            fields = {}
            while offset < end:
                length = data[offset]; offset += 1
                key = data[offset:offset+length].decode(); offset += length
                size, = struct.unpack_from('<I', data, offset); offset += 4
                assert offset + size <= end and key not in fields
                fields[key] = data[offset:offset+size]; offset += size
            assert offset == end
            self.sections[name] = fields
        assert offset == len(data)

    def encode(self, omitted=None):
        omitted = omitted or {}
        result = bytearray(self.prefix)
        for name, fields in self.sections.items():
            block = bytearray()
            for key, value in fields.items():
                if key in omitted.get(name, set()): continue
                text = key.encode()
                block += bytes([len(text)]) + text + struct.pack('<I', len(value)) + value
            result += name.encode().ljust(32, b'\0') + struct.pack('<I', len(block)) + block
        struct.pack_into('<I', result, 20, len(result) | (0x80000000 if self.big_endian else 0))
        if self.history:
            result += self.history[:-32]
            result += hashlib.sha256(result).digest()
        return bytes(result)

    def clocks(self, selection):
        result = {}
        for section, fields in selection.items():
            assert section in self.sections, section
            for field in fields:
                value = self.sections[section][field]
                assert len(value) == 4, (section, field)
                result[section + '.' + field] = int.from_bytes(value, 'big' if self.big_endian else 'little')
        return result
