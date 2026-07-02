#!/usr/bin/env python3
"""Create a PC-98 state bundle that proves whether load_state is instruction-exact.

The generated bundle redirects the i386 real-mode CPU to 0000:8000 and places:

    inc byte ptr [0x9000]
    jmp $

If load_state is atomic, an immediate read of RAM 0x9000 returns 00 and EIP is
0x8000.  If execution leaks between register restore and debugger stop, RAM
0x9000 returns a non-zero counter and EIP is usually 0x8004.
"""

from __future__ import annotations

import argparse
import json
import sys
import zipfile
from pathlib import Path

I386_REGS = [
    "eax",
    "ecx",
    "edx",
    "ebx",
    "esp",
    "ebp",
    "esi",
    "edi",
    "eip",
    "eflags",
    "cs",
    "ss",
    "ds",
    "es",
    "fs",
    "gs",
]


def parse_int(raw: str) -> int:
    raw = raw.strip()
    if raw.startswith("$"):
        return int(raw[1:], 16)
    return int(raw, 0)


def patch_reg(regs: bytearray, name: str, value: int) -> None:
    index = I386_REGS.index(name) * 4
    regs[index : index + 4] = int(value).to_bytes(4, "little")


def require_16bit_address(name: str, value: int) -> None:
    if not 0 <= value <= 0xFFFF:
        raise SystemExit(f"{name} must fit in a 16-bit real-mode displacement: {value:#x}")


def make_sled(base: Path, output: Path, code_address: int, counter_address: int) -> dict[str, object]:
    require_16bit_address("code_address", code_address)
    require_16bit_address("counter_address", counter_address)
    if code_address <= counter_address < code_address + 6:
        raise SystemExit("counter_address overlaps the injected sled code")

    with zipfile.ZipFile(base, "r") as zin:
        manifest = json.loads(zin.read("state.json").decode("utf-8"))
        if manifest.get("format") not in (
            "emucap-mame-pc98-state-v1",
            "emucap-mame-pc98-state-v2",
        ):
            raise SystemExit(f"unsupported state format: {manifest.get('format')!r}")

        regs = bytearray.fromhex(str(manifest.get("registers_hex", "")))
        min_len = len(I386_REGS) * 4
        if len(regs) < min_len:
            raise SystemExit(f"state register packet is too short for i386: {len(regs)} bytes")

        for name in ("eax", "ecx", "edx", "ebx", "esi", "edi"):
            patch_reg(regs, name, 0)
        patch_reg(regs, "esp", 0x7000)
        patch_reg(regs, "ebp", 0x7000)
        patch_reg(regs, "eip", code_address)
        patch_reg(regs, "eflags", 0x202)
        for name in ("cs", "ss", "ds", "es", "fs", "gs"):
            patch_reg(regs, name, 0)

        code = bytes([0xFE, 0x06, counter_address & 0xFF, counter_address >> 8, 0xEB, 0xFE])
        manifest["registers_hex"] = regs.hex()
        manifest.pop("save_items", None)
        manifest["emucap_atomic_restore_probe"] = {
            "code_address": f"0000:{code_address:04x}",
            "counter_memory_type": "ram",
            "counter_address": f"0x{counter_address:04x}",
            "code_hex": code.hex(),
            "expected_exact_counter_hex": "00",
            "leaked_execution_counter_hex": "nonzero",
            "discarded_save_items": True,
            "mcp_check": [
                f"load_state({output})",
                f'read_memory(memory_type="ram", address="0x{counter_address:04x}", length=1)',
                "get_state(groups=[\"cpu\"])",
            ],
        }

        output.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as zout:
            for info in zin.infolist():
                if info.filename.startswith("saveitems/"):
                    continue
                data = zin.read(info.filename)
                if info.filename == "ram.bin":
                    ram = bytearray(data)
                    end = max(code_address + len(code), counter_address + 1)
                    if end > len(ram):
                        raise SystemExit(f"ram.bin too small for sled end {end:#x}")
                    ram[code_address : code_address + len(code)] = code
                    ram[counter_address] = 0
                    data = bytes(ram)
                elif info.filename == "state.json":
                    data = json.dumps(manifest, separators=(",", ":")).encode("utf-8")
                zout.writestr(info, data)

    return manifest["emucap_atomic_restore_probe"]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base_state", type=Path, help="existing emucap PC-98 state zip")
    parser.add_argument("output_state", type=Path, help="where to write the sled zip")
    parser.add_argument("--code-address", default="0x8000", type=parse_int)
    parser.add_argument("--counter-address", default="0x9000", type=parse_int)
    args = parser.parse_args(argv)

    probe = make_sled(args.base_state, args.output_state, args.code_address, args.counter_address)
    print(json.dumps({"output_state": str(args.output_state), "probe": probe}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
