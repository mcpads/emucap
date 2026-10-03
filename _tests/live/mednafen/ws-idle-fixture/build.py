#!/usr/bin/env python3
"""Build the self-authored idle fixture; requires NASM, output is user-selected."""
import argparse
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
args = parser.parse_args()
args.output.parent.mkdir(parents=True, exist_ok=True)
subprocess.run(['nasm', '-f', 'bin', str(Path(__file__).with_name('idle.asm')),
                '-o', str(args.output)], check=True)
rom = bytearray(args.output.read_bytes())
assert len(rom) == 128 * 1024
rom[-2:] = (sum(rom[:-2]) & 65535).to_bytes(2, 'little')
args.output.write_bytes(rom)
