#!/usr/bin/env python3
"""Native drive-A swap witness. Optional --content opens an interactive JSON tool loop.

All media, captures, state and protocol logs go to the caller's private output directory.
The synthetic route writes through the guest disk BIOS, swaps, restores and reads back.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import McpProcess
from disk_state import fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--content', type=Path)
    parser.add_argument('--firmware', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    disk = args.content.resolve() if args.content else out / 'boot.dsk'
    stops = None if args.content else fixture(disk)
    source_hash = hashlib.sha256(disk.read_bytes()).hexdigest()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(port),
               EMUCAP_EMU_HOME=str(out / 'home'), EMUCAP_OPENMSX_FIRMWARE=str(args.firmware.resolve()))
    process = McpProcess(ROOT / 'target/release/emucap-mcp', env)
    launch = None
    rows = []

    def call(name, arguments=None, error=False):
        response = process.request('tools/call', {'name': name, 'arguments': arguments or {}}, timeout=300)
        for block in response.get('result', {}).get('content', []):
            if block.get('type') == 'image':
                image = out / f'capture-{len(rows):04}.png'
                image.write_bytes(base64.b64decode(block['data']))
                block['data'] = str(image)
        rows.append({'tool': name, 'arguments': arguments, 'response': response})
        (out / 'requests.json').write_text(json.dumps(rows, indent=2))
        failed = bool(response.get('error') or response.get('result', {}).get('isError'))
        assert failed == error, response
        return response.get('result', {}).get('structuredContent', response)

    def seek(pc):
        bp = call('set_breakpoint', {'kind': 'exec', 'memory_type': 'memory', 'start': hex(pc), 'end': hex(pc), 'pause_on_hit': True})
        assert call('step', {'unit': 'frames', 'count': 2500})['status'] == 'interrupted'
        call('poll_events')
        call('clear_breakpoint', {'id': bp['id']})

    def boundary():
        return call('get_state', {'groups': ['cpu']})

    def change(path=None, eject=False, error=False, **extra):
        params = {'device': 'diska', 'eject': eject, **extra}
        if path is not None:
            params['path'] = str(path)
        return call('change_media', params, error=error)

    try:
        process.initialize()
        call('bootstrap')
        plan = call('launch_plan', {'content_path': str(disk), 'system': 'msx2'})
        assert plan['ready_to_launch'], plan
        launch = call('launch', {**plan['preferred_launcher']['args'], 'display': True, 'sound': False})
        status = call('status')
        assert status['connected'] and 'change_media' in status['methods'], status
        assert status['contracts']['state'] == 'validated', status
        (out / 'identity.json').write_text(json.dumps({'launch': launch, 'status': status, 'rom': call('get_rom_info')}, indent=2))
        if args.content:
            print(json.dumps({'ready': launch, 'output': str(out)}), flush=True)
            for line in sys.stdin:
                request = json.loads(line)
                if request['tool'] == 'quit':
                    break
                print(json.dumps(call(request['tool'], request.get('arguments'), request.get('error', False))), flush=True)
        else:
            seek(stops[0])
            state = out / 'saved.state'
            call('save_state', {'path': str(state)})
            before = boundary()
            second = out / 'second.dsk'
            second.write_bytes(bytes([0x22]) * 737280)
            change(second, expected_sha1='0' * 40, error=True)
            change(out / 'missing.dsk', error=True)
            assert boundary() == before
            swapped = change(second, expected_sha1=hashlib.sha1(second.read_bytes()).hexdigest())
            assert boundary() == before
            written = Path(swapped['previous']['path'])
            assert written.read_bytes()[-512:] == bytes([0x5a]) * 512
            # Reinsert exported bytes, not the unchanged input source.
            change(written)
            seek(stops[1])
            after_write = boundary()
            ejected = change(eject=True)
            assert boundary() == after_write
            assert ejected['current']['mounted'] is False
            call('save_state', {'path': str(out / 'empty.state')}, error=True)
            assert not (out / 'empty.state').exists()
            changed = Path(ejected['previous']['path'])
            assert changed.read_bytes()[-512:] == bytes([0xa5]) * 512
            call('load_state', {'path': str(state)})
            restored = change(second)
            assert Path(restored['previous']['path']).read_bytes()[-512:] == bytes([0x5a]) * 512
            change(changed)
            seek(stops[2])
            observed = call('read_memory', {'memory_type': 'memory', 'address': '0xd000', 'length': 512})['hex']
            assert observed == 'a5' * 512, observed
            assert hashlib.sha256(disk.read_bytes()).hexdigest() == source_hash
            assert second.read_bytes() == bytes([0x22]) * 737280
            (out / 'result.json').write_text(json.dumps({'passed': True, 'source_sha256': source_hash}, indent=2))
            print('Native disk swap/write/eject/restore/readback passed', flush=True)
    finally:
        if launch:
            call('stop', {'launch_id': launch['launch_id']})
        process.close()


if __name__ == '__main__':
    main()
