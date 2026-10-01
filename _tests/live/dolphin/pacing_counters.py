#!/usr/bin/env python3
"""Guest VI/timebase/input comparison using the typed synthetic DOL."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_tests/live'))
from observation_speed import Witness, free_port
from dolphin_frame_restore import runtime_binary


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--content', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cpu', type=int, choices=[0,4,5], default=4)
    parser.add_argument('--single-core', action='store_true')
    parser.add_argument('--display', action='store_true')
    parser.add_argument('--sound', action='store_true')
    parser.add_argument('--dsp', choices=['hle','lle'], default='hle')
    parser.add_argument('--dsp-rom-dir', type=Path)
    args = parser.parse_args()
    if args.dsp == 'lle' and args.dsp_rom_dir is None:
        parser.error('--dsp-rom-dir is required for LLE')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    content = args.content.resolve(strict=True)
    metadata = content.with_suffix('.json')
    layout = json.loads(metadata.read_text())
    port = free_port()
    home = out / 'home'
    config = f'[Core]\nCPUCore = {args.cpu}\nCPUThread = {not args.single_core}\nDSPHLE = {args.dsp == "hle"}\nGPUDeterminismMode = auto\n'
    cfg = home / 'dolphin' / str(port) / 'user/Config/Dolphin.ini'
    cfg.parent.mkdir(parents=True)
    cfg.write_text(config)
    (out / 'native-config.ini').write_text(config)
    bound = [content, metadata, Path(__file__).resolve(), ROOT/'_tests/live/observation_speed.py',
             ROOT/'_tests/live/mesen2/support.py', ROOT/'_tests/live/dolphin_frame_restore.py',
             ROOT/'target/release/emucap-mcp', ROOT/'adapters/dolphin/upstream.lock']
    if args.dsp == 'lle':
        gc = cfg.parent.parent / 'GC'
        gc.mkdir()
        for name in ['dsp_rom.bin','dsp_coef.bin']:
            source = (args.dsp_rom_dir / name).resolve(strict=True)
            shutil.copy2(source, gc/name)
            bound.append(source)
    hashes = {str(p):sha(p) for p in bound}
    w = Witness(out, dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(home), EMUCAP_PORT=str(port)))
    launch = None
    rows = []
    def record():
        raw = w.call('read_memory', {'memory_type':'main','address':layout['record'],'length':layout['record_bytes']})
        return list(struct.unpack('>6I',bytes.fromhex(raw['hex'])))
    try:
        w.process.initialize()
        plan = w.call('launch_plan', {'content_path':str(content),'system':'gamecube'})
        assert plan['ready_to_launch'], plan
        launch = w.call('launch', {**plan['preferred_launcher']['args'],'display':args.display,'sound':args.sound})
        native = Path(launch['binary'])
        assert sha(native) == sha(runtime_binary(launch)), 'runtime producer bytes'
        hashes[str(native)] = sha(native)
        (out/'producer.json').write_text(json.dumps(hashes,indent=2))
        w.call('pause')
        w.call('status')
        w.speed({'mode':'unlimited'})
        bp = w.call('set_breakpoint', {'kind':'exec','memory_type':'main','start':layout['ready'],'end':layout['ready'],'pause_on_hit':True})
        stopped = w.call('step', {'count':5000,'unit':'frames'})
        assert stopped['status']=='interrupted' and stopped['reason']=='breakpoint', stopped
        w.call('clear_breakpoint', {'id':bp['id']})
        w.call('poll_events')
        w.call('step', {'count':4,'unit':'frames'})
        w.call('step', {'count':1,'unit':'instructions'})
        origin = out/'origin.state'
        w.call('save_state', {'path':str(origin)})
        hashes[str(origin)] = sha(origin)
        (out/'producer.json').write_text(json.dumps(hashes,indent=2))
        base = record()
        assert base[0]>0, base
        for percent, held in [(100,True),(100,True),(100,False),(50,True),(400,True),(10000,True),(None,True),(1,True)]:
            w.call('load_state', {'path':str(origin)})
            w.call('status')
            w.revision=None
            w.speed({'mode':'unlimited'} if percent is None else {'mode':'limited','percent':percent})
            assert record()==base, 'origin record restoration'
            before=w.call('status')
            audio=before['audio_output']
            assert audio['initialized'] and audio['start_verified'] and not audio['failure'], audio
            assert (audio['backend'] == 'No Audio Output') == (not args.sound), audio
            components=w.call('get_state',{'groups':['cpu']})['runtime_components']
            assert components['dual_core'] == (not args.single_core), components
            assert not components['deterministic_gpu_thread'], components
            assert components['dsp_engine']==args.dsp, components
            assert components['cpu_engine']=={0:'Interpreter64',4:'JITARM64',5:'Cached Interpreter'}[args.cpu], components
            if held:
                result=w.call('tap',{'buttons':['a'],'press_frames':2,'after_frames':3})
            else:
                result=w.call('step',{'count':6,'unit':'frames'})
            after=w.call('status')
            current=record()
            cpu=w.call('get_state',{'groups':['cpu']})
            row={'percent':percent,'held':held,'base':base,'record':current,
                 'frame_delta':after['frame']-before['frame'],'audio_output':audio,'components':components,'cpu':cpu,'result':result}
            rows.append(row)
            (out/'runs.json').write_text(json.dumps(rows,indent=2))
            print(percent,held,current,row['frame_delta'],flush=True)
        reference = rows[0]
        for row in rows:
            current = row['record']
            assert current[0] - base[0] == 6, row
            assert current[1] - base[1] == (2 if row['held'] else 0), row
            # 2 fields/scan * 525 half-lines * 429 samples * 3 TB ticks/sample.
            expected = 6 * 2 * 525 * 429 * 3
            # Each true scan edge lies between the guest's previous-read lower
            # sample and current-read upper sample. Compare the resulting interval.
            lower = (current[2] - base[5]) % (1 << 32)
            upper = (current[5] - base[2]) % (1 << 32)
            assert lower <= expected <= upper, (lower, expected, upper, row)
            for sample in (base, current):
                assert (sample[5] - sample[2]) % (1 << 32) < 429 * 3, sample
            assert row['frame_delta'] == 12, row
            if row['held']:
                assert row['record'] == reference['record'] and row['cpu'] == reference['cpu'], row
        (out/'completed.json').write_text(json.dumps({'passed':True,'launch':launch,'cases':len(rows)},indent=2))
    finally:
        try:
            if launch:w.call('stop',{'launch_id':launch['launch_id']})
        finally:
            w.process.close()
    assert all(sha(p)==h for p,h in hashes.items()), 'inputs changed'
    assert subprocess.run(['ps','-p',str(launch['pid'])],stdout=subprocess.DEVNULL).returncode!=0
    (out/'audit.json').write_text(json.dumps({'inputs_unchanged':True,'owned_process_absent':True},indent=2))


if __name__ == '__main__':
    main()
