#!/usr/bin/env python3
"""Opt-in filesystem regression using the built xemu generator and cipher objects.

Requirement: a generated EEPROM is absent or complete, including at first creation.
The fopen hook supplies the failing interleaving for the old implementation; the
reader checks published bytes through the real filesystem. Entropy is a fixture.
Requires a native xemu build and pkg-config glib-2.0. --source permits a baseline.
"""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--build', type=Path, default=ROOT / 'adapters/xemu/work/xemu/build')
p.add_argument('--source', type=Path)
a = p.parse_args()
build = a.build.resolve()
row = next(r for r in json.loads((build / 'compile_commands.json').read_text())
           if r['file'].endswith('/eeprom_generation.c'))
with tempfile.TemporaryDirectory(prefix='xemu-eeprom-') as tmp:
    tmp = Path(tmp)
    cmd = shlex.split(row['command'])
    for flag in ('-o', '-MQ', '-MF'):
        i = cmd.index(flag)
        cmd[i+1] = str(tmp / {'-o': 'generator.o', '-MQ': 'generator.o', '-MF': 'generator.d'}[flag])
    cmd.extend(['-I', str((build / row['file']).resolve().parent)])
    if a.source:
        cmd[cmd.index('-c')+1] = str(a.source.resolve())
    subprocess.run(cmd, cwd=build, check=True)
    flags = shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'glib-2.0'], text=True))
    subprocess.run(['cc', str(Path(__file__).with_suffix('.c')), str(tmp / 'generator.o'),
                    str(build / 'libqemuutil.a.p/util_sha1.c.o'),
                    str(build / 'libqemuutil.a.p/util_rc4.c.o'), *flags, '-o', str(tmp / 'test')], check=True)
    (tmp / 'files').mkdir()
    subprocess.run([str(tmp / 'test'), str(tmp / 'files')], check=True, timeout=30)
