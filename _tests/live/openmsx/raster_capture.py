#!/usr/bin/env python3
"""Opt-in native raster qualification. Outputs remain in a caller-selected private directory.

Default: generated Z80 border-toggle cartridge, no game data. Requires Pillow and
an installed pinned openMSX host + release Control/bridge. --content/--stop-pc runs
an independent consumer route; it does not substitute for the border witness.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys

from PIL import Image

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live/mesen2'))
from support import McpProcess


def fixture(path):
    # Disable display and IRQ. Poll the VBlank latch once per VDP field, toggling
    # border register 7 between black (1) and white (15). RAM C100 is the exact
    # last written colour, so the preceding field's center has the other colour.
    code = bytes.fromhex('f33100f33e00d3993e81d3993e013200c1')
    loop = 0x4010 + len(code)
    code += bytes.fromhex('db99e68028fa3a00c1ee0e3200c1d3993e87d399')
    stop = 0x4010 + len(code)  # after the VDP register write, in VBlank
    code += bytes([0xc3, loop & 255, loop >> 8])
    data = bytearray([255] * 16384)
    data[:16] = b'AB\x10\x40' + bytes(12)
    data[16:16 + len(code)] = code
    path.write_bytes(data)
    return stop


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--content', type=Path)
    parser.add_argument('--system', default='msx')
    parser.add_argument('--stop-pc', type=lambda s: int(s, 0))
    parser.add_argument('--snapshot', action='append', default=[])
    args = parser.parse_args()
    if args.content and args.stop_pc is None:
        parser.error("--content requires --stop-pc")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    content = args.content.resolve() if args.content else out / 'border-toggle.rom'
    stop_pc = args.stop_pc if args.content else fixture(content)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(out/'home'), EMUCAP_PORT=str(port))
    process = McpProcess(ROOT/'target/release/emucap-mcp', env)
    launch = None
    rows = []

    def call(name, arguments=None, error=False):
        result = process.request('tools/call', {'name': name, 'arguments': arguments or {}}, timeout=300)
        for block in result.get('result', {}).get('content', []):
            if block.get('type') == 'image':
                block['data'] = '<PNG saved separately>'
        rows.append({'tool': name, 'arguments': arguments, 'response': result})
        (out/'requests.json').write_text(json.dumps(rows, indent=2))
        failed = bool(result.get('error') or result.get('result', {}).get('isError'))
        assert failed == error, result
        return result.get('result', {}).get('structuredContent', result)

    def step(n):
        return call('step', {'unit': 'frames', 'count': n})

    def capture(name, unavailable=False):
        before = call('get_state', {'groups': ['cpu']})
        result = call('screenshot', {'save_path': str(out/(name+'.png'))}, error=unavailable)
        assert before == call('get_state', {'groups': ['cpu']}), 'capture advanced CPU state'
        if unavailable:
            assert result["error"]["code"] == "bad_state", result
            assert not (out/(name+'.png')).exists()
            return None
        p = result['provenance']
        assert p['freshness'] == 'latest_completed_frame', p
        assert p['frame_before'] == p['frame_after'] == p['frame']
        assert p['raster_boundary']['frame'] + 1 == p['capture_boundary']['frame'], p
        assert p['raster_boundary']['launch_id'] == launch['launch_id']
        assert p['raster_boundary']['image_sha256'] == p['sha256']
        assert hashlib.sha256((out/(name+'.png')).read_bytes()).hexdigest() == p['sha256']
        return {**p, "_file":name+".png"}

    def same_raster(left, right):
        # Native PNGs contain a wall-clock Creation Time chunk. Compare decoded
        # pixels and raster provenance; each PNG digest still binds its own bytes.
        a, b = dict(left['raster_boundary']), dict(right['raster_boundary'])
        a.pop('image_sha256'); b.pop('image_sha256')
        assert a == b
        assert left['capture_boundary'] == right['capture_boundary']
        assert Image.open(out/left['_file']).tobytes() == Image.open(out/right['_file']).tobytes()

    def colour():
        return int(call('read_memory', {'memory_type':'memory', 'address':'0xc100', 'length':1})['hex'], 16)

    def check_center(name, expected_white):
        rgb = Image.open(out/(name+'.png')).convert('RGB').getpixel((160,120))
        assert (min(rgb) > 240 if expected_white else max(rgb) < 16), (name, rgb, expected_white)

    try:
        process.initialize()
        bootstrap = call('bootstrap')
        plan = call('launch_plan', {'content_path':str(content), 'system':args.system})
        assert plan['ready_to_launch'], plan
        launch = call('launch', {**plan['preferred_launcher']['args'], 'display':True, 'sound':False})
        status = call('status')
        assert status['connected'] and status['contracts']['state'] == 'validated'
        (out/'identity.json').write_text(json.dumps({'bootstrap':bootstrap,'launch':launch,'status':status,
            'content_sha256':hashlib.sha256(content.read_bytes()).hexdigest()}, indent=2))
        if args.content:
            assert stop_pc is not None, '--content requires --stop-pc'
            bp = call('set_breakpoint', {'kind':'exec','memory_type':'memory','start':hex(stop_pc),'end':hex(stop_pc),
                'pause_on_hit':True,'snapshot':args.snapshot})
            hit = step(2500)
            assert hit['status'] == 'interrupted', hit
            call('poll_events')
            first = capture('exec-stop')
            assert first['capture_boundary']['pc'] == stop_pc
            same_raster(capture('exec-repeat'), first)
            call('clear_breakpoint', {'id':bp['id']})
            call('step', {'unit':'instructions','count':1})
            step(2)
            capture('two-frames')
            step(10)
            capture('ten-frames')
        else:
            # An explicit reset invalidates the previously retained raster.
            call('reset')
            capture('reset-unavailable', unavailable=True)
            step(2000)
            first = capture('long-step')
            check_center('long-step', colour() == 1)
            same_raster(capture('long-repeat'), first)
            for i in range(4):
                previous = colour()
                step(1)
                capture(f'short-{i}')
                check_center(f'short-{i}', previous == 15)
            bp = call('set_breakpoint', {'kind':'exec','memory_type':'memory','start':hex(stop_pc),'end':hex(stop_pc),'pause_on_hit':True,
                'snapshot':['memory:0xc100:1']})
            hit = step(5000)
            assert hit['status'] == 'interrupted', hit
            call('poll_events')
            halted = capture('exec-stop')
            assert halted['capture_boundary']['pc'] == stop_pc
            same_raster(capture('exec-repeat'), halted)
            call('clear_breakpoint', {'id':bp['id']})
            call('step', {'unit':'instructions','count':1})
            advanced = capture('instruction-step')
            assert advanced['raster_boundary']['id'] == halted['raster_boundary']['id']
            assert Image.open(out/'instruction-step.png').tobytes() == Image.open(out/'exec-stop.png').tobytes()
            step(5000)
            capture('long-step-again')
            check_center('long-step-again', colour() == 1)
        saved = out/'checkpoint.state'
        before = capture('before-save')
        call('save_state', {'path':str(saved)})
        step(3)
        call('load_state', {'path':str(saved)})
        capture('restore-unavailable', unavailable=True)
        step(2)  # explicitly request enough progress for one complete new raster
        after = capture('after-restore')
        assert after['raster_boundary']['epoch'] != before['raster_boundary']['epoch']
        same_raster(capture('restore-repeat'), after)
        if not args.content:
            check_center('after-restore', colour() == 1)
        print('qualified', out, flush=True)
    finally:
        if launch and launch.get('launch_id'):
            stopped = call('stop', {'launch_id':launch['launch_id']})
            assert stopped['stopped'] and stopped['processes']['completed'], stopped
        process.close()


if __name__ == '__main__':
    main()
