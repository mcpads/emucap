#!/usr/bin/env python3
"""Opt-in disk state closure/transaction witness; all generated media stays private.

Without --content, generate a boot disk whose Z80 program writes 5A then A5 to
sector 1439 and reads it back. With --content, stop at a caller-specified game PC.
Requires pinned host, release binaries and admitted MSX2 firmware.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import McpProcess


def fixture(path):
    disk = bytearray(737280)
    disk[:11] = b'\xeb\xfe\x90EMUCAP  '
    struct.pack_into('<HBHBHHBHHH', disk, 11, 512, 2, 1, 2, 112, 1440, 0xf9, 3, 9, 2)
    disk[512:515] = disk[2048:2051] = b'\xf9\xff\xff'
    code = bytearray.fromhex('3100f0')  # SP=F000, boot enters at C01E with disk ROM mapped
    def fill(value):
        code.extend(bytes.fromhex('2100d01101d001ff01') + bytes([0x36, value]) + bytes.fromhex('edb0'))
    # Save AF after disk BIOS. Carry clear is required at every checkpoint.
        code.extend(bytes.fromhex('f5e52204c1e1f1'))  # overwritten below with correct push/pop
    # Save AF via PUSH AF; POP HL; LD (C104),HL (buffer no longer needed by BIOS).
    def operation(write):
        code.extend(bytes.fromhex('af06010ef9119f052100d0') + (b'\x37' if write else b'\xb7') + bytes.fromhex('cd1040f5e12204c1'))
    fill(0x5a); operation(True)
    saved = 0xc01e + len(code)
    fill(0xa5); operation(True)
    changed = 0xc01e + len(code)
    fill(0); operation(False)
    read = 0xc01e + len(code)
    code.extend(bytes([0xc3, read & 255, read >> 8]))
    disk[30:30+len(code)] = code
    path.write_bytes(disk)
    return saved, changed, read


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--content', type=Path)
    parser.add_argument('--stop-pc', type=lambda x: int(x, 0))
    parser.add_argument('--legacy-state', type=Path)
    args = parser.parse_args()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    generated = args.content is None
    content = args.content.resolve() if args.content else out/'disk-write.dsk'
    stops = fixture(content) if generated else (args.stop_pc,)
    assert stops[0] is not None
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(out/'home'), EMUCAP_PORT=str(port))
    process = McpProcess(ROOT/'target/release/emucap-mcp', env)
    launch = None; rows = []; identities = []

    def call(name, arguments=None, error=False):
        response = process.request('tools/call', {'name':name,'arguments':arguments or {}}, timeout=300)
        for block in response.get('result', {}).get('content', []):
            if block.get('type') == 'image': block['data'] = '<PNG saved separately>'
        rows.append({'tool':name,'arguments':arguments,'response':response})
        (out/'requests.json').write_text(json.dumps(rows, indent=2))
        failed = bool(response.get('error') or response.get('result', {}).get('isError'))
        assert failed == error, response
        return response.get('result', {}).get('structuredContent', response)

    def start(path):
        nonlocal launch
        plan = call('launch_plan', {'content_path':str(path),'system':'msx2'})
        assert plan['ready_to_launch'], plan
        launch = call('launch', {**plan['preferred_launcher']['args'],'display':True,'sound':False})
        status = call('status')
        assert status['connected'] and status['contracts']['state'] == 'validated'
        identities.append({'launch':launch,'status':status,'rom':call('get_rom_info')})
        (out/'identity.json').write_text(json.dumps(identities, indent=2))

    def stop():
        nonlocal launch
        if launch:
            call('stop', {'launch_id':launch['launch_id']}); launch = None

    def set_input(buttons):
        revision = call('debug', {'operation':'describe'})['capability_revision']
        return call('debug', {'operation':'set_input','known_capability_revision':revision,'arguments':{'port':1,'buttons':buttons}})

    def step(n): return call('step', {'unit':'frames','count':n})
    def read(kind, addr, n): return call('read_memory', {'memory_type':kind,'address':hex(addr),'length':n})['hex']
    def boundary(): return {'cpu':call('get_state', {'groups':['cpu']})['state'], 'ram':read('ram',0,256), 'vram':read('vram',0,256)}
    def seek(pc):
        bp = call('set_breakpoint', {'kind':'exec','memory_type':'memory','start':hex(pc),'end':hex(pc),'pause_on_hit':True})
        result = step(2500)
        assert result['status'] == 'interrupted', result
        events = call('poll_events')
        call('clear_breakpoint', {'id':bp['id']})
        return events
    def rejected(path):
        before = boundary(); info = call('get_rom_info')
        call('load_state', {'path':str(path)}, error=True)
        assert boundary() == before
        assert call('get_rom_info') == info
        assert call('status')['connected']

    try:
        process.initialize(); call('bootstrap'); start(content)
        seek(stops[0])
        if generated:
            assert int(read('memory',0xc104,1),16) & 1 == 0, 'guest BIOS write failed'
        saved = boundary(); state = out/'snapshot.state'
        call('save_state', {'path':str(state)})
        with zipfile.ZipFile(state) as archive:
            members = {name:archive.read(name) for name in archive.namelist()}
        if generated: assert members['disk.dsk'][-512:] == bytes([0x5a])*512
        original_disk = Path(call('get_rom_info')['mounted_path'])
        if generated:
            seek(stops[1])
            changed_state = out/'changed.state'
            call('save_state', {'path':str(changed_state)})
            with zipfile.ZipFile(changed_state) as changed_archive:
                assert changed_archive.read('disk.dsk')[-512:] == bytes([0xa5])*512
        else: step(30)
        # Live watchpoint and persistent input must survive replacement.
        bp = call('set_breakpoint', {'kind':'write','memory_type':'memory','start':'0xd000','end':'0xd000','pause_on_hit':True})
        set_input(['right'])
        call('load_state', {'path':str(state)})
        assert boundary() == saved
        assert call('list_breakpoints')['breakpoints'][0]['id'] == bp['id']
        assert call('status')['joystick_ports'][0]['buttons'] == ['right']
        if generated:
            assert step(120)['status'] == 'interrupted'
            hits = call('poll_events')['events']
            assert any(e['breakpoint_id'] == bp['id'] for e in hits)
        call('clear_breakpoint', {'id':bp['id']})
        set_input([])
        if generated:
            assert Path(call('get_rom_info')['mounted_path']).read_bytes()[-512:] == bytes([0x5a])*512
            # Freeze a real WD2793 write after its command is issued, before sector completion.
            command_bp = call('set_breakpoint', {'kind':'write','memory_type':'memory','start':'0x7ff8','end':'0x7ff8','pause_on_hit':True})
            for attempt in range(10):
                assert step(120)['status'] == 'interrupted'
                hits = call('poll_events')['events']
                if any((e.get('value', 0) & 0xe0) == 0xa0 for e in hits): break
            else: raise AssertionError('no guest write-sector command observed')
            call('clear_breakpoint', {'id':command_bp['id']})
            call('step', {'unit':'instructions','count':1})
            assert int(read('memory',0x7ff8,1),16) & 1, 'FDC is not busy'
            pending = boundary(); pending_status = read('memory',0x7ff8,8)
            pending_state = out/'pending-write.state'
            call('save_state', {'path':str(pending_state)})
            call('load_state', {'path':str(pending_state)})
            assert boundary() == pending and read('memory',0x7ff8,8) == pending_status
            seek(stops[1])
            completed_state = out/'completed-write.state'
            call('save_state', {'path':str(completed_state)})
            with zipfile.ZipFile(completed_state) as completed:
                assert completed.read('disk.dsk')[-512:] == bytes([0xa5])*512
        stop()
        old_generation = original_disk.parent.parent
        assert old_generation.is_relative_to(out/'home')
        old_generation.rename(out/'retired-generation')
        assert not old_generation.exists()
        start(content)
        call('load_state', {'path':str(state)})
        assert boundary() == saved
        mounted = Path(call('get_rom_info')['mounted_path'])
        assert mounted != original_disk and mounted.is_relative_to(out/'home')
        # Legacy/malformed, source mismatch, and native checksum mismatch all retain original.
        legacy = out/'legacy.state'; legacy.write_bytes(members['machine.state'])
        if generated:
            # Guest-written 5A is absent from admitted source: legacy restoration must fail.
            rejected(legacy)
        else:
            call('load_state', {'path':str(legacy)})
            assert boundary() == saved
        for case in ['source', 'native-checksum', 'machine', 'candidate-identity']:
            mutated = dict(members); manifest = json.loads(mutated['manifest.json'])
            if case == 'source': manifest['source_sha1'] = '0'*40
            elif case == 'native-checksum':
                disk = bytearray(mutated['disk.dsk']); disk[-1] ^= 1; mutated['disk.dsk'] = bytes(disk)
                manifest['disk_sha256'] = hashlib.sha256(disk).hexdigest()
            elif case == 'candidate-identity':
                xml = gzip.decompress(mutated['machine.state'])
                assert b'<name>Philips_NMS_8250</name>' in xml
                xml = xml.replace(b'<name>Philips_NMS_8250</name>', b'<name>Foreign_MSX</name>', 1)
                mutated['machine.state'] = gzip.compress(xml)
                manifest['machine_sha256'] = hashlib.sha256(mutated['machine.state']).hexdigest()
            else:
                # Keep wrapper hashes consistent; invalid native payload must fail transactionally.
                mutated['machine.state'] = b'invalid native state'
                manifest['machine_sha256'] = hashlib.sha256(mutated['machine.state']).hexdigest()
            mutated['manifest.json'] = json.dumps(manifest).encode()
            bad = out/(case+'.state')
            with zipfile.ZipFile(bad,'w') as archive:
                for name, data in mutated.items(): archive.writestr(name,data)
            rejected(bad)
        if generated:
            # Replace the future write with NOPs so the BIOS read must observe saved 5A.
            # This intervention is explicit; it does not count as natural game evidence.
            second_io = stops[1] - 20
            call('write_memory', {'memory_type':'memory','address':hex(second_io),'hex':'00'*20})
            seek(stops[2])
            assert int(read('memory',0xc104,1),16) & 1 == 0
            assert read('memory',0xd000,512) == '5a'*512
        if args.legacy_state:
            call('load_state', {'path':str(args.legacy_state.resolve())})
            call('get_state', {'groups':['cpu']})
        call('tap', {'buttons':['space'],'press_frames':2,'after_frames':30})
        step(2)
        call('screenshot', {'save_path':str(out/'restored.png')})
        (out/'result.json').write_text(json.dumps({'passed':True,'snapshot_sha256':hashlib.sha256(state.read_bytes()).hexdigest(),'saved':saved},indent=2))
    finally:
        stop(); process.close()

if __name__ == '__main__': main()
