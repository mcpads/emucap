#!/usr/bin/env python3
"""Opt-in Apple GL diagnostic with actual native upload wrapper and real pixel readback.

Some unguarded trials may crash. Each child is isolated, with PID/status/logs preserved.
Requires a materialized xemu source, clang and pkg-config glib-2.0 on macOS.
"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--iterations', type=int, default=512)
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    assert sys.platform == 'darwin' and args.iterations > 0 and args.repeats > 0
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for name in ('macos_gl_upload.c', 'macos_gl_upload.py'):
        (out / name).write_bytes(Path(__file__).with_name(name).read_bytes())
    native = (ROOT / 'adapters/xemu/work/xemu/ui/xemu-gl-upload.c').read_text()
    (out / 'native-original.c').write_text(native)
    # Replace integration includes only; retain the production GLib lock and wrapper bodies.
    standalone = '#include <glib.h>\n#include <OpenGL/gl3.h>\n' + '\n'.join(
        line for line in native.splitlines() if not line.startswith('#include'))
    (out / 'native-upload.c').write_text(standalone)
    flags = shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','glib-2.0'],text=True))
    command = ['clang','-O2','-Wall','-Wextra','-Werror','-Wno-deprecated-declarations',
               '-framework','OpenGL','-pthread',str(out/'macos_gl_upload.c'),
               str(out/'native-upload.c'),'-o',str(out/'probe'),*flags]
    subprocess.run(command,check=True)
    (out / 'identity.json').write_text(json.dumps({'platform':platform.platform(),
        'revision':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        'compile':command,'binary_sha256':hashlib.sha256((out/'probe').read_bytes()).hexdigest(),
        'native_source_sha256':hashlib.sha256(native.encode()).hexdigest()},indent=2))
    (out/'source.diff').write_bytes(subprocess.check_output(['git','diff','HEAD'],cwd=ROOT))
    rows=[]
    for repeat in range(args.repeats):
        for shared, serialized in ((0,0),(1,1),(1,0),(1,1)):
            label=f'run-{len(rows)}-shared{shared}-serial{serialized}'
            with (out/f'{label}.log').open('w') as log:
                process=subprocess.Popen([str(out/'probe'),str(serialized),str(shared),str(args.iterations)],stdout=log,stderr=subprocess.STDOUT)
                timed_out = False
                try:
                    code=process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    process.terminate()
                    try:
                        code=process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        code=process.wait()
            row={'label':label,'shared':bool(shared),'serialized':bool(serialized),
                 'iterations_per_thread':args.iterations,'pid':process.pid,'returncode':code,
                 'timed_out':timed_out}
            rows.append(row)
            (out/'results.json').write_text(json.dumps(rows,indent=2))
            print(json.dumps(row),flush=True)
    assert all(r['returncode']==0 for r in rows if r['serialized']), 'native wrapper trial failed'


if __name__ == '__main__':
    main()
