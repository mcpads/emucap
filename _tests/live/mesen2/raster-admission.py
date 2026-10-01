#!/usr/bin/env python3
"""Live NES/SMS/GG/GB/GBC/GBA raster snapshot preflight: malformed/legacy payloads cannot mutate devices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from input_pacing import Origin, Session, window_batches
from observation_speed import ROOT, Witness, free_port, warmup


def decode(path):
    data = path.read_bytes()
    assert data[:3] == b'MSS' and struct.unpack_from('<I', data, 7)[0] == 4
    compressed_preview = struct.unpack_from('<I', data, 31)[0]
    pos = 35 + compressed_preview
    name_length = struct.unpack_from('<I', data, pos)[0]
    pos += 4 + name_length
    assert data[pos] == 1
    length, packed_length = struct.unpack_from('<II', data, pos + 1)
    raw = zlib.decompress(data[pos+9:pos+9+packed_length])
    assert len(raw) == length
    entries = {}
    offset = 0
    while offset < len(raw):
        end = raw.index(0, offset)
        name = raw[offset:end].decode('ascii')
        size = struct.unpack_from('<I', raw, end+1)[0]
        entries[name] = raw[end+5:end+5+size]
        offset = end+5+size
    assert offset == len(raw)
    return data[:pos], entries


def encode(path, header, entries):
    raw = b''.join(k.encode()+b'\0'+struct.pack('<I', len(v))+v for k,v in entries.items())
    packed = zlib.compress(raw)
    path.write_bytes(header+b'\1'+struct.pack('<II',len(raw),len(packed))+packed)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--profile', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    profile = json.loads(args.profile.read_text())
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()), EMUCAP_EMU_HOME=str(out/'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env); session = None

    def observe():
        state = w.call('get_state')
        memory = hashlib.sha256()
        for ranges in batches:
            result = w.call('read_memory_batch', {'ranges':ranges})
            for read in result['reads']:
                memory.update(bytes.fromhex(read['hex']))
        return {'state':state,'memory':memory.hexdigest(),'policy':w.speed({})}

    try:
        w.process.initialize()
        plan = w.call('launch_plan', profile['launch_plan']); assert plan['ready_to_launch']
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch',{})})
        status = session.start()
        (out/'identity.json').write_text(json.dumps({'launch':session.launch,'status':status},indent=2))
        capability = status['memory_batch_capability']
        is_nes = any(window['memory_type'] == 'nesInternalRam' for window in capability['windows'])
        is_gba = any(window['memory_type'] == 'gbaVideoRam' for window in capability['windows'])
        is_gb = any(window['memory_type'] == 'gbWorkRam' for window in capability['windows'])
        regions = (['nesInternalRam','nesChrRam','nesNametableRam','nesPaletteRam','nesSpriteRam','nesSecondarySpriteRam']
                   if is_nes else ['gbaIntWorkRam','gbaExtWorkRam','gbaVideoRam','gbaPaletteRam','gbaSpriteRam']
                   if is_gba else ['gbWorkRam','gbVideoRam','gbSpriteRam','gbHighRam'] if is_gb
                   else ['smsWorkRam','smsVideoRam','smsPaletteRam'])
        batches = [batch for window in capability['windows'] if window['memory_type'] in regions
                   for batch in window_batches(capability, window)]
        assert batches
        w.speed({'mode':'unlimited'}); warmup(w,status,profile.get('warmup_frames',60))
        origin = Origin(w,out,status,60,session); assert origin.path, origin.save_error
        header, entries = decode(Path(origin.path))
        prefix = 'ppu.' if is_nes or is_gba or is_gb else 'vdp.'
        names = [prefix+name for name in ['rasterVersion','outputBufferIndex','outputBuffer0',
                                        'outputBuffer1','completedFrame','previousCompletedFrame']]
        if is_nes:
            names = names[:4]
        if is_gb:
            names.append(prefix+'completedPixelCount')
        assert all(n in entries for n in names), list(entries)
        cases = [('legacy', {k:v for k,v in entries.items() if k not in names})]
        for name in names:
            cases.append(('missing-'+name, {k:v for k,v in entries.items() if k != name}))
        invalid = [('unknown-version', names[0], struct.pack('<I',0xffffffff)),
                   ('legacy-version', names[0], struct.pack('<I',0 if is_nes or is_gba or is_gb else 1)),
                   ('invalid-index', names[1], b'\2')]
        if is_gb:
            invalid.append(('invalid-pixel-count', prefix+'completedPixelCount', struct.pack('<I',1)))
        for name in names[2:]:
            invalid.extend([('short-'+name,name,entries[name][:-2]),
                            ('long-'+name,name,entries[name]+b'\0\0')])
        for label, name, value in invalid:
            cases.append((label, {**entries,name:value}))
        original = observe(); rows = []
        for name, payload in cases:
            path = out/(name+'.state'); encode(path,header,payload)
            w.call('step', {'unit':'instructions','count':1})
            before = observe()
            response = w.call('load_state', {'path':str(path)}, error=True)
            assert 'unsafe_halt' not in json.dumps(response), response
            assert observe() == before, name
            rows.append({'case':name,'rejected':True,'unchanged':True,'response':response})
        w.call('step', {'unit':'instructions','count':1})
        w.call('load_state', {'path':origin.path})
        assert observe() == original, 'valid load must still work after rejected attempts'
        (out/'result.json').write_text(json.dumps({'passed':True,'cases':rows,'valid_retry':True},indent=2))
    finally:
        try:
            if session: (out/'stop.json').write_text(json.dumps(session.stop(),indent=2))
        finally: w.process.close()


if __name__ == '__main__': main()
