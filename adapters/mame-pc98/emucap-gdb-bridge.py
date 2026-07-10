#!/usr/bin/env python3
"""emucap <-> MAME PC-98 GDB-stub bridge.

This is a proof-of-concept.  It translates the common emucap NDJSON protocol to the
repo-local MAME Lua GDB stub or the built-in MAME C++ gdbstub.

Scope:
- status/read_memory/write_memory/get_state/get_rom_info/find_pattern/dump_memory/screenshot
- save_state/load_state/reset/break_on_reset
- pause/resume/step/run_frames/input/breakpoints

Not in this PoC:
- full device-state snapshots

Usage:
  emucap-gdb-bridge.py <EMUCAP_PORT> [GDB_HOST:PORT]
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import posixpath
import re
import socket
import sys
import tempfile
import time
import zipfile
from typing import Any


FRONT_WRITE_TIMEOUT = 5.0


class BridgeError(Exception):
    pass


class GdbRsp:
    """Tiny GDB RSP client for MAME gdbstub.  Ack mode, packet escaping."""

    def __init__(self, host: str, port: int, timeout: float = 5.0, connect_wait: float = 30.0):
        # MAME opens its gdbstub only after the machine boots, so the bridge may start first. Retry the
        # connect until the stub is up (up to connect_wait) instead of dying on the first refusal — this
        # removes the launch race regardless of how MAME and the bridge were started.
        deadline = time.monotonic() + connect_wait
        while True:
            try:
                self.sock = socket.create_connection((host, port), timeout=timeout)
                break
            except (ConnectionRefusedError, OSError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.3)
        self.sock.settimeout(timeout)
        self.buf = b""

    def get_timeout(self) -> float:
        return self.sock.gettimeout() or 5.0

    def set_timeout(self, t: float) -> None:
        self.sock.settimeout(t)

    @staticmethod
    def _checksum(payload: bytes) -> int:
        return sum(payload) & 0xFF

    def _read_byte(self) -> bytes:
        if not self.buf:
            self.buf = self.sock.recv(4096)
            if not self.buf:
                raise BridgeError("GDB connection closed")
        b, self.buf = self.buf[:1], self.buf[1:]
        return b

    def send(self, payload: str, expect_reply: bool = True) -> str | None:
        data = payload.encode("ascii")
        frame = b"$" + data + b"#" + f"{self._checksum(data):02x}".encode("ascii")
        self.sock.sendall(frame)
        for _ in range(8):
            ack = self._read_byte()
            if ack == b"+":
                break
            if ack == b"-":
                self.sock.sendall(frame)
            elif ack == b"$":
                self.buf = ack + self.buf
                break
        return self.recv() if expect_reply else None

    def send_extension(self, payload: str) -> str:
        data = payload.encode("ascii")
        frame = b"$" + data + b"#" + f"{self._checksum(data):02x}".encode("ascii")
        self.sock.sendall(frame)
        return self.recv()

    def interrupt(self) -> str:
        self.sock.sendall(b"\x03")
        time.sleep(0.01)
        return self.send("?") or ""

    def recv(self) -> str:
        while True:
            b = self._read_byte()
            if b == b"$":
                break
        raw = bytearray()
        while True:
            b = self._read_byte()
            if b == b"#":
                break
            raw += b
        self._read_byte()
        self._read_byte()
        self.sock.sendall(b"+")

        out = bytearray()
        i = 0
        while i < len(raw):
            if raw[i] == 0x7D and i + 1 < len(raw):
                out.append(raw[i + 1] ^ 0x20)
                i += 2
            else:
                out.append(raw[i])
                i += 1
        return out.decode("latin-1")

    def recv_nonblocking(self) -> str | None:
        self.sock.setblocking(False)
        try:
            chunk = self.sock.recv(4096)
        except (BlockingIOError, OSError):
            chunk = b""
        finally:
            self.sock.settimeout(5.0)
        if not chunk and not self.buf:
            return None
        self.buf += chunk
        if b"$" not in self.buf:
            return None
        return self.recv()


MEM_BASE = {
    "physical": 0x00000,
    "cpu": 0x00000,
    "ram": 0x00000,
    "tvram": 0xA0000,
    "gvram_b": 0xA8000,
    "gvram_r": 0xB0000,
    "gvram_g": 0xB8000,
    "gvram_i": 0xE0000,
}

REGION_SIZE = {
    "physical": 0x100000,
    "cpu": 0x100000,
    "ram": 0x100000,
    "tvram": 0x4000,
    "gvram_b": 0x8000,
    "gvram_r": 0x8000,
    "gvram_g": 0x8000,
    "gvram_i": 0x8000,
}

DUMP_REGIONS = [
    {"name": "ram", "memory_type": "ram", "base_address": 0x00000, "size": 0x100000},
    {"name": "tvram", "memory_type": "tvram", "base_address": 0xA0000, "size": 0x4000},
    {"name": "gvram_b", "memory_type": "gvram_b", "base_address": 0xA8000, "size": 0x8000},
    {"name": "gvram_r", "memory_type": "gvram_r", "base_address": 0xB0000, "size": 0x8000},
    {"name": "gvram_g", "memory_type": "gvram_g", "base_address": 0xB8000, "size": 0x8000},
    {"name": "gvram_i", "memory_type": "gvram_i", "base_address": 0xE0000, "size": 0x8000},
]

MAX_FIND_LEN = 128 * 1024
MAX_READ_CHUNK = 0x4000
TRACE_CAP = 4096
LEGACY_STATE_FORMAT = "emucap-mame-pc98-state-v1"
STATE_FORMAT = "emucap-mame-pc98-state-v2"
SAVE_ITEMS_DIR = "saveitems"
SAVE_ITEMS_MANIFEST = f"{SAVE_ITEMS_DIR}/manifest.txt"
STATE_RESTORE_INFO = {
    "format": STATE_FORMAT,
    "scope": "cpu-register-packet-plus-ram-tvram-gvram-plus-mame-save-items",
    "deterministic_replay": True,
    "hidden_device_state": True,
    "save_manager_items": True,
    "save_manager_restore": "best_effort_lua_item_write",
    "post_restore_instruction_exact": True,
    "native_atomic_machine_state_load": False,
    "freeze_strategy": "lua_frozen_socket_service",
    "notes": (
        "PC-98 state bundles restore RAM/TVRAM/GVRAM, MAME save-manager items exposed "
        "through Lua, and the i386 register packet. After load_state, the Lua plugin "
        "keeps servicing the GDB socket while the debugger is stopped so MCP reads and "
        "get_state observe the restored instruction slot before it executes. This passes "
        "the PC-98 atomic-restore sled gate for post-load observation, but the "
        "implementation is still a Lua/GDB bridge hold rather than a native C++ MAME "
        "machine-state load."
    ),
}
PC98_INPUT_BUTTONS = [
    "enter",
    "esc",
    "space",
    "up",
    "down",
    "left",
    "right",
    "backspace",
    "tab",
    "del",
    "ins",
    "home",
    "help",
    "stop",
    "copy",
    "shift",
    "ctrl",
    *[f"f{i}" for i in range(1, 11)],
    *[f"vf{i}" for i in range(1, 6)],
    *[chr(code) for code in range(ord("a"), ord("z") + 1)],
    *[str(i) for i in range(10)],
]
PC98_INPUT_ALIASES = {
    "return": "enter",
    "return_key": "enter",
    "start": "enter",
    "escape": "esc",
    "select": "space",
    "delete": "del",
    "insert": "ins",
    "bksp": "backspace",
    "bs": "backspace",
}

LUA_BACKEND = "lua-gdbstub"
METHODS = [
    "hello",
    "status",
    "read_memory",
    "find_pattern",
    "dump_memory",
    "get_rom_info",
    "write_memory",
    "get_state",
    "save_state",
    "load_state",
    "probe",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "poll_events",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "screenshot",
    "set_input",
    "press_buttons",
    "reset",
    "break_on_reset",
    "run_frames",
    "disassemble",
    "watch_register",
    "set_trace",
    "get_trace",
    "call_stack",
]

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

I86_REGS = [
    "ax",
    "cx",
    "dx",
    "bx",
    "sp",
    "bp",
    "si",
    "di",
    "ip",
    "flags",
    "cs",
    "ss",
    "ds",
    "es",
]

PC98_DEBUG_REGS = {
    "eax": ("eax", "cpu.eax"),
    "ax": ("eax", "cpu.eax"),
    "ecx": ("ecx", "cpu.ecx"),
    "cx": ("ecx", "cpu.ecx"),
    "edx": ("edx", "cpu.edx"),
    "dx": ("edx", "cpu.edx"),
    "ebx": ("ebx", "cpu.ebx"),
    "bx": ("ebx", "cpu.ebx"),
    "esp": ("esp", "cpu.esp"),
    "sp": ("esp", "cpu.esp"),
    "ebp": ("ebp", "cpu.ebp"),
    "bp": ("ebp", "cpu.ebp"),
    "esi": ("esi", "cpu.esi"),
    "si": ("esi", "cpu.esi"),
    "edi": ("edi", "cpu.edi"),
    "di": ("edi", "cpu.edi"),
    "eip": ("eip", "cpu.eip"),
    "ip": ("eip", "cpu.eip"),
    "offset_pc": ("eip", "cpu.offset_pc"),
    "pc": ("pc", "cpu.pc"),
    "eflags": ("eflags", "cpu.eflags"),
    "flags": ("eflags", "cpu.eflags"),
    "cs": ("cs", "cpu.cs"),
    "ss": ("ss", "cpu.ss"),
    "ds": ("ds", "cpu.ds"),
    "es": ("es", "cpu.es"),
    "fs": ("fs", "cpu.fs"),
    "gs": ("gs", "cpu.gs"),
}


def _need(params: dict[str, Any], key: str) -> Any:
    if key not in params or params[key] is None:
        raise BridgeError(f"missing required param: {key}")
    return params[key]


def _num(value: Any) -> int:
    if isinstance(value, str):
        raw = value.strip()
        if raw.startswith("$"):
            return int(raw[1:], 16)
        return int(raw, 0)
    return int(value)


def _addr(params: dict[str, Any]) -> int:
    mt = params.get("memory_type", "physical")
    base = MEM_BASE.get(mt)
    if base is None:
        raise BridgeError(f"unsupported memory_type: {mt}")
    return base + _num(_need(params, "address"))


class Bridge:
    def __init__(self, gdb: GdbRsp):
        self.gdb = gdb
        self.backend = LUA_BACKEND
        self.frozen = True
        self.events: list[dict[str, Any]] = []
        self.bps: dict[int, dict[str, Any]] = {}
        self.next_bp = 1
        self.enriching_stop = False
        self.tracing = False
        self.trace_path: str | None = None
        try:
            stop = self.gdb.send("?")
            if stop:
                self._note_stop(stop)
        except BridgeError:
            self.frozen = False

    def hello(self, _params: dict[str, Any]) -> dict[str, Any]:
        result = {
            "protocol_version": 1,
            "system": "pc98",
            "adapter": "mame-pc98-gdb",
            "backend": self.backend,
            "debugger": True,
            "methods": list(METHODS),
            "memory_types": sorted(MEM_BASE.keys()),
            "region_sizes": REGION_SIZE,
            "state_restore": self._state_restore_info(),
            "capability_notes": self._capability_notes(),
            "input_buttons": {
                "system": "pc98",
                "buttons": PC98_INPUT_BUTTONS,
                "aliases": PC98_INPUT_ALIASES,
                "notes": "PC-98 uses keyboard inputs. Prefer enter/esc/space/up/down/left/right plus letter, digit, f1-f10, and vf1-vf5 keys.",
            },
        }
        if name := os.environ.get("EMUCAP_NAME"):
            result["name"] = name
        if token := os.environ.get("EMUCAP_SESSION_TOKEN"):
            result["session_token"] = token
        if content := os.environ.get("EMUCAP_CONTENT"):
            result["content"] = content
        if launch_id := os.environ.get("EMUCAP_LAUNCH_ID"):
            result["launch_id"] = launch_id
        # launch가 넘긴 emucap git hash(status.emulator_build) — 사용자가 git HEAD와 대조해 최신 여부 확인.
        result["build"] = os.environ.get("EMUCAP_BUILD_HASH", "unknown")
        return result

    def status(self, _params: dict[str, Any]) -> dict[str, Any]:
        self._drain_stop()
        return {
            "connected": True,
            "system": "pc98",
            "adapter": "mame-pc98-gdb",
            "backend": self.backend,
            "debugger": True,
            "frame": self._current_frame(),
            "state": "frozen" if self.frozen else "running",
            "memory_types": sorted(MEM_BASE.keys()),
            "state_restore": self._state_restore_info(),
            "capability_notes": self._capability_notes(),
            "input_buttons": {
                "system": "pc98",
                "buttons": PC98_INPUT_BUTTONS,
                "aliases": PC98_INPUT_ALIASES,
                "notes": "PC-98 keyboard inputs via MAME ioport field overrides.",
            },
        }

    def get_rom_info(self, _params: dict[str, Any]) -> dict[str, Any]:
        content = os.environ.get("EMUCAP_CONTENT") or ""
        if not content:
            raise BridgeError("EMUCAP_CONTENT is not set")
        path = os.path.abspath(content)
        if not os.path.isfile(path):
            raise BridgeError(f"content image not found: {content}")
        return {
            "system": "pc98",
            "adapter": "mame-pc98-gdb",
            "name": os.path.basename(path),
            "path": path,
            "sha1": self._sha1_file(path),
            "size": os.path.getsize(path),
            "media_type": os.path.splitext(path)[1].lstrip(".").lower(),
        }

    def read_memory(self, params: dict[str, Any]) -> dict[str, Any]:
        addr = _addr(params)
        length = int(_need(params, "length"))
        return {"hex": self._read_abs_hex(addr, length)}

    def write_memory(self, params: dict[str, Any]) -> dict[str, Any]:
        addr = _addr(params)
        hexstr = str(_need(params, "hex"))
        if len(hexstr) % 2:
            raise BridgeError("hex must have even length")
        size = len(hexstr) // 2
        resp = self._send_command(f"M{addr:x},{size:x}:{hexstr}")
        if resp != "OK":
            raise BridgeError(f"GDB memory write failed: {resp}")
        return {"written": size}

    def _capability_notes(self) -> dict[str, Any]:
        return {
            "backend": self.backend,
            "frame_step": True,
            "screenshot": True,
            "input": True,
            "step_units": ["frames", "instructions"],
            "breakpoint_conditions": True,
            "trace": True,
        }

    def _state_restore_info(self) -> dict[str, Any]:
        return STATE_RESTORE_INFO

    def _require_lua_backend(self, feature: str) -> None:
        if self.backend != LUA_BACKEND:
            raise BridgeError(f"{feature} requires the PC-98 Lua bridge")

    def set_input(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("set_input")
        buttons = self._normalize_buttons(params.get("buttons") or [])
        self._lua_cmd("setinput", ",".join(buttons))
        return {"buttons": buttons}

    def press_buttons(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("press_buttons")
        buttons = self._normalize_buttons(params.get("buttons") or [])
        frames = max(_num(params.get("frames", 1)), 1)
        stop = self._deferred_lua_op("press", f"{frames}:{','.join(buttons)}", frames)
        if stop:
            self.frozen = True
            return {
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": stop,
                "buttons": buttons,
                "frames": frames,
                "frame": self._current_frame(),
            }
        self.frozen = False
        return {
            "status": "completed",
            "buttons": buttons,
            "frames": frames,
            "frame": self._current_frame(),
            "state": "running",
        }

    def find_pattern(self, params: dict[str, Any]) -> dict[str, Any]:
        memory_type = str(params.get("memory_type", "physical"))
        if memory_type not in MEM_BASE:
            raise BridgeError(f"unsupported memory_type: {memory_type}")
        raw_hex = str(_need(params, "hex"))
        try:
            pattern = bytes.fromhex(raw_hex)
        except ValueError as err:
            raise BridgeError("hex decode failed") from err
        if not pattern:
            raise BridgeError("hex must contain at least one byte")

        start = _num(params.get("start", 0))
        if start < 0:
            start = 0
        region_size = REGION_SIZE.get(memory_type)
        length_param = params.get("length")
        if length_param is None:
            if region_size is None:
                raise BridgeError("length is required for this memory_type")
            length = max(region_size - start, 0)
        else:
            length = _num(length_param)
            if length < 0:
                raise BridgeError("length must be non-negative")
        if region_size is not None:
            if start >= region_size:
                length = 0
            else:
                length = min(length, region_size - start)

        truncated_scan = length > MAX_FIND_LEN
        scan_len = min(length, MAX_FIND_LEN)
        max_matches = _num(params.get("max_matches", 256))
        if max_matches < 1:
            max_matches = 1
        max_matches = min(max_matches, 4096)
        align = _num(params.get("align", 1))
        if align < 1:
            align = 1

        buf = self._read_region_bytes(memory_type, start, scan_len)
        matches: list[int] = []
        truncated_matches = False
        pos = 0
        while True:
            idx = buf.find(pattern, pos)
            if idx < 0:
                break
            off = start + idx
            if (off - start) % align == 0:
                if len(matches) >= max_matches:
                    truncated_matches = True
                    break
                matches.append(off)
            pos = idx + 1
        return {
            "matches": matches,
            "count": len(matches),
            "truncated": truncated_scan or truncated_matches,
            "truncated_scan": truncated_scan,
            "truncated_matches": truncated_matches,
            "scanned": scan_len,
            "start": start,
        }

    def dump_memory(self, params: dict[str, Any]) -> dict[str, Any]:
        path = str(_need(params, "path"))
        os.makedirs(path, exist_ok=True)
        metas = []
        for region in DUMP_REGIONS:
            name = region["name"]
            memory_type = region["memory_type"]
            size = int(region["size"])
            out_path = os.path.join(path, f"{name}.bin")
            with open(out_path, "wb") as f:
                offset = 0
                while offset < size:
                    chunk = min(MAX_READ_CHUNK, size - offset)
                    f.write(self._read_region_bytes(str(memory_type), offset, chunk))
                    offset += chunk
            metas.append(region)
        with open(os.path.join(path, "regions.json"), "w", encoding="utf-8") as f:
            json.dump(metas, f, separators=(",", ":"))
        return {"path": path, "regions": len(metas)}

    def screenshot(self, _params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("screenshot")
        fd, path = tempfile.mkstemp(prefix="emucap_pc98_", suffix=".png")
        os.close(fd)
        try:
            self._lua_cmd("snapshot", path)
            with open(path, "rb") as f:
                data = f.read()
            if not data.startswith(b"\x89PNG\r\n\x1a\n"):
                raise BridgeError("MAME snapshot did not produce a PNG")
            return {"png_base64": base64.b64encode(data).decode("ascii")}
        finally:
            try:
                os.unlink(path)
            except OSError:
                pass

    def save_state(self, params: dict[str, Any]) -> dict[str, Any]:
        path = str(_need(params, "path"))
        out_path = os.path.abspath(path)
        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        self._stop_for_state_restore()
        regs_hex = self._read_regs_hex()
        regions = []
        with tempfile.TemporaryDirectory(prefix="emucap_pc98_saveitems_") as save_items_dir:
            save_items = self._save_lua_save_items(save_items_dir)
            save_items["dir"] = SAVE_ITEMS_DIR
            save_items_members = self._save_item_members(save_items_dir)
            with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
                for region in DUMP_REGIONS:
                    name = str(region["name"])
                    memory_type = str(region["memory_type"])
                    size = int(region["size"])
                    member = f"{name}.bin"
                    zf.writestr(member, self._read_region_bytes(memory_type, 0, size))
                    regions.append({**region, "file": member})
                for src_path, member in save_items_members:
                    zf.write(src_path, member)
                manifest = {
                    "format": STATE_FORMAT,
                    "system": "pc98",
                    "adapter": "mame-pc98-gdb",
                    "registers_hex": regs_hex,
                    "regions": regions,
                    "save_items": save_items,
                    "state_restore": self._state_restore_info(),
                }
                zf.writestr("state.json", json.dumps(manifest, separators=(",", ":")))
        return {
            "path": path,
            "format": STATE_FORMAT,
            "regions": len(regions),
            "save_items": save_items,
            "bytes": os.path.getsize(out_path),
            "state_restore": self._state_restore_info(),
        }

    def load_state(self, params: dict[str, Any]) -> dict[str, Any]:
        path = str(_need(params, "path"))
        in_path = os.path.abspath(path)
        if not os.path.isfile(in_path):
            raise BridgeError(f"save state not found: {path}")
        self._stop_for_state_restore()
        with tempfile.TemporaryDirectory(prefix="emucap_pc98_loaditems_") as td:
            with zipfile.ZipFile(in_path, "r") as zf:
                manifest = json.loads(zf.read("state.json").decode("utf-8"))
                state_format = manifest.get("format")
                if state_format not in (STATE_FORMAT, LEGACY_STATE_FORMAT):
                    raise BridgeError(f"unsupported PC-98 state format: {manifest.get('format')}")
                save_items_dir = self._extract_save_items(zf, manifest, td)
                regions = [
                    (str(region["memory_type"]), zf.read(str(region["file"])))
                    for region in manifest.get("regions", [])
                ]
                regs_hex = str(manifest.get("registers_hex", ""))
            save_items_result = self._load_lua_save_items(save_items_dir) if save_items_dir else {}
        restore_result: dict[str, Any] = {
            "restore_strategy": "memory_only",
            "post_restore_instruction_exact": True,
        }
        self._write_state_regions(regions)
        if regs_hex:
            restore_result = self._restore_regs_after_state_load(regs_hex, regions)
        self.frozen = True
        return {
            "path": path,
            "format": state_format,
            "regions": len(manifest.get("regions", [])),
            "state_restore": self._state_restore_info(),
        } | save_items_result | restore_result

    def probe(self, params: dict[str, Any]) -> dict[str, Any]:
        path = str(_need(params, "state"))
        frame = max(_num(params.get("frame", params.get("frames", 0))), 0)
        memory_type = str(params.get("memory_type", "physical"))
        base = MEM_BASE.get(memory_type)
        if base is None:
            raise BridgeError(f"unsupported memory_type: {memory_type}")
        address = base + _num(_need(params, "address"))
        length = _num(_need(params, "length"))
        if length < 0:
            raise BridgeError("length must be non-negative")
        in_path = os.path.abspath(path)
        if not os.path.isfile(in_path):
            raise BridgeError(f"save state not found: {path}")
        self._stop_for_state_restore()
        with tempfile.TemporaryDirectory(prefix="emucap_pc98_probeitems_") as td:
            with zipfile.ZipFile(in_path, "r") as zf:
                manifest = json.loads(zf.read("state.json").decode("utf-8"))
                if manifest.get("format") not in (STATE_FORMAT, LEGACY_STATE_FORMAT):
                    raise BridgeError(
                        f"unsupported PC-98 state format for probe: {manifest.get('format')}"
                    )
                save_items_dir = self._extract_save_items(zf, manifest, td)
                regions = [
                    (str(region["memory_type"]), zf.read(str(region["file"])))
                    for region in manifest.get("regions", [])
                ]
                regs_hex = str(manifest.get("registers_hex", ""))
                if not regs_hex:
                    raise BridgeError("PC-98 probe state is missing registers_hex")
            save_items_result = self._load_lua_save_items(save_items_dir) if save_items_dir else {}
            self._write_state_regions(regions)
            result = self._register_probe(regs_hex, frame, address, length)
        result.update(save_items_result)
        self.frozen = True
        return result

    def _write_state_regions(self, regions: list[tuple[str, bytes]]) -> None:
        for memory_type, data in regions:
            self._write_region_bytes(memory_type, 0, data)

    @staticmethod
    def _save_item_members(save_items_dir: str) -> list[tuple[str, str]]:
        members = []
        for root, _dirs, files in os.walk(save_items_dir):
            for name in sorted(files):
                src_path = os.path.join(root, name)
                rel = os.path.relpath(src_path, save_items_dir)
                member = posixpath.join(SAVE_ITEMS_DIR, *rel.split(os.sep))
                members.append((src_path, member))
        return members

    def _save_lua_save_items(self, path: str) -> dict[str, int]:
        os.makedirs(path, exist_ok=True)
        resp = self._lua_cmd_reply("saveitems", path)
        return self._parse_save_items_response(resp, "saveitems")

    def _load_lua_save_items(self, path: str) -> dict[str, int]:
        resp = self._lua_cmd_reply("loaditems", path)
        parsed = self._parse_save_items_response(resp, "loaditems")
        return {
            "save_items_restored": parsed["items"],
            "save_items_skipped": parsed["skipped"],
        }

    @staticmethod
    def _parse_save_items_response(resp: str, command: str) -> dict[str, int]:
        match = re.fullmatch(r"OK\|(\d+)\|(\d+)", resp)
        if not match:
            raise BridgeError(f"MAME Lua command {command} failed: {resp}")
        return {"items": int(match.group(1)), "skipped": int(match.group(2))}

    @staticmethod
    def _extract_save_items(
        zf: zipfile.ZipFile, manifest: dict[str, Any], target_root: str
    ) -> str | None:
        save_items = manifest.get("save_items")
        if not isinstance(save_items, dict):
            return None
        directory = str(save_items.get("dir", SAVE_ITEMS_DIR)).strip("/")
        if directory != SAVE_ITEMS_DIR:
            raise BridgeError(f"unsupported PC-98 save item directory: {directory}")
        names = [name for name in zf.namelist() if name.startswith(f"{SAVE_ITEMS_DIR}/")]
        if SAVE_ITEMS_MANIFEST not in names:
            raise BridgeError("PC-98 save item manifest is missing")
        out_dir = os.path.join(target_root, SAVE_ITEMS_DIR)
        for name in names:
            rel = name[len(SAVE_ITEMS_DIR) + 1 :]
            if not rel:
                continue
            parts = rel.split("/")
            if any(part in ("", ".", "..") for part in parts):
                raise BridgeError(f"unsafe PC-98 save item member: {name}")
            dest = os.path.join(out_dir, *parts)
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as f:
                f.write(zf.read(name))
        return out_dir

    def _restore_regs_after_state_load(
        self, regs_hex: str, regions: list[tuple[str, bytes]]
    ) -> dict[str, Any]:
        current = self._load_regs_via_lua(regs_hex)
        self.frozen = True
        target = self._state_from_regs_hex(regs_hex)
        exact = self._state_matches_real_mode_pc(
            current, target.get("cpu.cs", -1), target.get("cpu.eip", -1)
        )
        return {
            "restore_strategy": "lua_register_load_hold",
            "post_restore_instruction_exact": exact,
            "observed_register_packet_matches_target": exact,
            "observed_pc": current.get("cpu.pc"),
            "observed_eip": current.get("cpu.eip"),
            "observed_cs": current.get("cpu.cs"),
        }

    @staticmethod
    def _state_matches_real_mode_pc(state: dict[str, int], cs: int, eip: int) -> bool:
        return (
            (state.get("cpu.cs", -1) & 0xFFFF) == cs
            and (state.get("cpu.eip", -1) & 0xFFFF) == eip
        )

    def _stop_for_state_restore(self) -> None:
        self._lua_cmd("stop")
        self.frozen = True

    def reset(self, _params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("reset")
        self._lua_cmd("reset")
        return {"reset": "scheduled"}

    def break_on_reset(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("break_on_reset")
        enabled = bool(params.get("enabled"))
        resp = self._lua_cmd_reply("breakonreset", "1" if enabled else "0")
        if resp != "OK":
            raise BridgeError(f"MAME break_on_reset failed: {resp}")
        return {"enabled": enabled, "system": "pc98", "mode": "machine_reset_notifier"}

    def get_state(self, _params: dict[str, Any]) -> dict[str, Any]:
        resp = self._read_regs_hex()
        return {"state": self._state_from_regs_hex(resp)}

    def pause(self, _params: dict[str, Any]) -> dict[str, Any]:
        if not self.frozen:
            stop = self.gdb.interrupt()
            self._note_stop(stop)
            self.frozen = True
        return {"state": "frozen"}

    def resume(self, _params: dict[str, Any]) -> dict[str, Any]:
        if self.frozen:
            self.gdb.send("c", expect_reply=False)
            self.frozen = False
        return {"state": "running"}

    def step(self, params: dict[str, Any]) -> dict[str, Any]:
        frames = max(_num(params.get("frames", 1)), 1)
        unit = str(params.get("unit", "frames"))
        if unit == "instructions":
            for _ in range(frames):
                self._send_command("s", allow_stop=True)
            self.frozen = True
            return {"status": "completed", "unit": "instructions", "count": frames}
        if unit != "frames":
            raise BridgeError(f"unsupported PC-98 step unit: {unit}")
        self._require_lua_backend("frame step")
        stop = self._frames_op("framestep", frames)
        self.frozen = True
        if stop:
            return {
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": stop,
                "frame": self._current_frame(),
            }
        return {
            "status": "completed",
            "unit": "frames",
            "frames": frames,
            "frame": self._current_frame(),
        }

    def step_instructions(self, params: dict[str, Any]) -> dict[str, Any]:
        count = max(_num(params.get("count", params.get("frames", 1))), 1)
        return self.step({"unit": "instructions", "frames": count})

    def _frames_op(self, name: str, frames: int) -> str | None:
        return self._deferred_lua_op(name, str(frames), frames)

    def _deferred_lua_op(self, name: str, arg: str, budget_frames: int) -> str | None:
        """프레임 진행 명령(runframes/framestep)을 보내되 recv 타임아웃을 N에 맞춰 늘린다.

        프레임 진행은 벽시계라(plugin은 N프레임이 끝나야 OK를 보냄) recv 5초 고정 타임아웃으로는
        큰 N(예: 3000+)이 끝나기 전에 타임아웃 난다. recv는 데이터 도착 즉시 반환하므로 타임아웃을
        넉넉히 늘려도 무해하다(상한 600s — hung plugin은 결국 에러). hit이 나면 plugin이 즉시 T05를
        보내 일찍 반환한다.

        트레이싱 중이면 프레임마다 수십만 명령을 디스어셈+기록하므로 무트레이스 50ms/frame 예산으론
        타임아웃→지연 stop이 recv 창 밖에 도착해 다음 요청에 오배달(desync)된다. 트레이스일 때 프레임당
        예산을 크게 잡아 지연 응답이 이 recv 창 안에서 매칭되게 한다(Rust 브리지 _frames_op와 동일).
        """
        prev = self.gdb.get_timeout()
        per_frame = 5.0 if self.tracing else 0.05
        self.gdb.set_timeout(min(600.0, 5.0 + budget_frames * per_frame))
        try:
            return self._lua_cmd_allow_stop(name, arg)
        finally:
            self.gdb.set_timeout(prev)

    def run_frames(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("run_frames")
        frames = max(_num(params.get("n", params.get("frames", 1))), 1)
        stop = self._frames_op("runframes", frames)
        if stop:
            self.frozen = True
            return {
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": stop,
                "frame": self._current_frame(),
            }
        self.frozen = False
        return {
            "status": "completed",
            "frames": frames,
            "frame": self._current_frame(),
            "state": "running",
        }

    def set_breakpoint(self, params: dict[str, Any]) -> dict[str, Any]:
        kind = params.get("kind", "exec")
        zkind = {"exec": "0", "write": "2", "read": "3", "access": "4"}.get(str(kind))
        if zkind is None:
            raise BridgeError("MAME PC-98 supports exec/read/write/access breakpoints")
        memory_type = str(params.get("memory_type", "physical"))
        base = MEM_BASE.get(memory_type)
        if base is None:
            raise BridgeError(f"unsupported memory_type: {memory_type}")
        addr = base + _num(_need(params, "start"))
        snapshots = self._parse_snapshot_specs(params.get("snapshot") or [])
        condition = self._breakpoint_condition(params, str(kind))
        pause_on_hit = bool(params.get("pause_on_hit", True))
        end = params.get("end")
        if end is None:
            size = 1
        else:
            size = max(base + _num(end) - addr + 1, 1)
        # pause는 condition 앞에 둔다(condition은 마지막 필드라 '|' 포함 가능; pause는 0/1 단일).
        resp = self._lua_cmd_reply(
            "setpoint", f"{zkind}|{addr:x}|{size:x}|{1 if pause_on_hit else 0}|{condition}"
        )
        match = re.fullmatch(r"(BP|WP):(\d+)", resp)
        if not match:
            raise BridgeError(f"MAME breakpoint set failed: {resp}")
        bid = self.next_bp
        self.next_bp += 1
        self.bps[bid] = {
            "kind": str(kind),
            "zkind": zkind,
            "addr": addr,
            "size": size,
            "backend": "bp" if match.group(1) == "BP" else "wp",
            "backend_id": int(match.group(2)),
            "condition": condition,
            "snapshots": snapshots,
            "pause_on_hit": pause_on_hit,
        }
        return {"id": bid}

    def watch_register(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("watch_register")
        raw_reg = str(params.get("register", "sp"))
        expr_reg, state_key = self._normalize_debug_register(raw_reg)
        lo = _num(params.get("min", 0))
        hi = _num(params.get("max", 0xFFFFFFFF))
        if lo > hi:
            raise BridgeError("min must be <= max")
        condition = f"({expr_reg} < {lo:X}) || ({expr_reg} > {hi:X})"
        pause_on_hit = bool(params.get("pause_on_hit", True))
        # pause는 prefix(condition은 '||' 포함; pause는 0/1 단일이라 앞에 두면 파싱 안전).
        resp = self._lua_cmd_reply("setregpoint", f"{1 if pause_on_hit else 0}|{condition}")
        match = re.fullmatch(r"RP:(\d+)", resp)
        if not match:
            raise BridgeError(f"MAME registerpoint set failed: {resp}")
        bid = self.next_bp
        self.next_bp += 1
        self.bps[bid] = {
            "kind": "reg",
            "backend": "rp",
            "backend_id": int(match.group(1)),
            "register": raw_reg,
            "condition_register": expr_reg,
            "state_key": state_key,
            "min": lo,
            "max": hi,
            "condition": condition,
            "pause_on_hit": pause_on_hit,
        }
        return {"id": bid}

    def set_trace(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("set_trace")
        enabled = bool(params.get("enabled"))
        if enabled:
            if not self.trace_path:
                fd, path = tempfile.mkstemp(prefix="emucap_pc98_trace_", suffix=".log")
                os.close(fd)
                try:
                    os.unlink(path)
                except OSError:
                    pass
                self.trace_path = path
            else:
                try:
                    os.unlink(self.trace_path)
                except OSError:
                    pass
            self._lua_cmd("tracestart", self.trace_path)
            self.tracing = True
            return {"tracing": True, "path": self.trace_path}
        if self.tracing:
            self._lua_cmd("traceflush")
            self._lua_cmd("tracestop")
        self.tracing = False
        return {"tracing": False, "path": self.trace_path}

    def get_trace(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("get_trace")
        count = max(_num(params.get("count", 64)), 1)
        count = min(count, TRACE_CAP)
        rows = self._read_trace_rows()
        return {
            "trace": rows[-count:],
            "tracing": self.tracing,
            "total": len(rows),
            "path": self.trace_path,
        }

    def call_stack(self, _params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("call_stack")
        # 트레이싱 중이면 call/ret 트레이스 스캔이 정확하니 그대로 쓴다. 아니면 정지 상태의
        # BP(EBP) 체인을 걸어 트레이스 없이 복원한다 — method 필드로 신뢰도를 알린다.
        if self.tracing:
            return self._call_stack_from_trace()
        return self._call_stack_from_frame_pointer()

    def _call_stack_from_trace(self) -> dict[str, Any]:
        rows = self._read_trace_rows()
        stack: list[int] = []
        frames: list[dict[str, Any]] = []
        for row in rows:
            text = str(row.get("text", "")).strip().lower()
            pc = row.get("pc")
            if text.startswith("call") and isinstance(pc, int):
                stack.append(pc)
                frames.append({"pc": pc, "text": row.get("text", "")})
            elif text.startswith("ret") and stack:
                stack.pop()
                frames.pop()
        return {
            "call_stack": stack,
            "frames": frames,
            "depth": len(stack),
            "method": "trace",
            "tracing": self.tracing,
            "total": len(rows),
        }

    def _call_stack_from_frame_pointer(self) -> dict[str, Any]:
        # 표준 BP 프롤로그(push bp; mov bp,sp)를 가정한다 — 모든 루틴이 지키진 않으므로
        # method="frame_pointer"로 알려 호출자가 신뢰도를 판단하게 한다.
        state = self._state_from_regs_hex(self._read_regs_hex())
        ebp = state.get("cpu.ebp", 0)
        esp = state.get("cpu.esp", 0)
        eip = state.get("cpu.eip", 0)
        ss = state.get("cpu.ss", 0)
        # CR0.PE는 RSP 레지스터 셋에 없다. 값 크기로 real16 vs protected32를 추정한다(caveat:
        # 라이브 검증 필요).
        real_mode = ebp <= 0xFFFF and esp <= 0xFFFF and eip <= 0xFFFF
        if real_mode:
            ptr_size, seg_base, bp_mask = 2, ss << 4, 0xFFFF
        else:
            ptr_size, seg_base, bp_mask = 4, 0, 0xFFFFFFFF
        bp = ebp & bp_mask
        stack: list[int] = []
        frames: list[dict[str, Any]] = []
        for _ in range(64):
            if bp == 0:
                break
            base = seg_base + bp
            if base + 2 * ptr_size > 0x0011_0000:
                break
            saved_bp = self._read_ptr_le(base, ptr_size)
            ret_addr = self._read_ptr_le(base + ptr_size, ptr_size)
            if saved_bp is None or ret_addr is None:
                break
            stack.append(ret_addr)
            frames.append({"pc": ret_addr, "frame_pointer": bp})
            if saved_bp <= bp:
                break
            bp = saved_bp & bp_mask
        return {
            "call_stack": stack,
            "frames": frames,
            "depth": len(stack),
            "method": "frame_pointer",
            "mode": "real16" if real_mode else "protected32",
            "pointer_size": ptr_size,
            "frame_pointer": ebp & bp_mask,
            "tracing": self.tracing,
        }

    def _read_ptr_le(self, addr: int, size: int) -> int | None:
        try:
            return int.from_bytes(bytes.fromhex(self._read_abs_hex(addr, size)), "little")
        except (BridgeError, ValueError):
            return None

    def disassemble(self, params: dict[str, Any]) -> dict[str, Any]:
        self._require_lua_backend("disassemble")
        addr = _num(_need(params, "address"))
        count = max(_num(params.get("count", 8)), 1)
        count = min(count, 256)
        byte_len = max(count * 16, 16)
        fd, path = tempfile.mkstemp(prefix="emucap_pc98_dasm_", suffix=".txt")
        os.close(fd)
        try:
            self._lua_cmd("dasm", f"{path}|{addr:x}|{byte_len:x}")
            with open(path, "r", encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
        finally:
            try:
                os.unlink(path)
            except OSError:
                pass
        instructions = self._parse_dasm(lines, count)
        if not instructions:
            raise BridgeError("MAME disassemble produced no instructions")
        return {"instructions": instructions}

    def clear_breakpoint(self, params: dict[str, Any]) -> dict[str, Any]:
        bid = int(_need(params, "id"))
        bp = self.bps.get(bid)
        if bp is None:
            raise BridgeError(f"unknown breakpoint id: {bid}")
        backend = str(bp.get("backend") or "")
        backend_id = bp.get("backend_id")
        if backend and backend_id is not None:
            self._require_lua_backend("clearpoint")
            payload = "qEmucap,clearpoint," + f"{backend}|{int(backend_id)}".encode().hex()
            resp = self._send_extension(payload)
            if resp == "E00":
                # Not found (stale after MAME reset) — idempotent success.
                del self.bps[bid]
                return {"cleared": bid}
            if resp != "OK":
                # Any other failure (connection closed, other E-code) must propagate.
                raise BridgeError(f"MAME breakpoint clear failed: {resp}")
        else:
            self._send_command(f"z{bp['zkind']},{bp['addr']:x},{bp['size']:x}")
        del self.bps[bid]
        return {"cleared": bid}

    def list_breakpoints(self, _params: dict[str, Any]) -> dict[str, Any]:
        return {
            "breakpoints": [
                {
                    "id": bid,
                    "kind": bp["kind"],
                    "start": bp["addr"],
                    "end": int(bp["addr"]) + int(bp["size"]) - 1,
                    "condition": bp.get("condition") or None,
                }
                if bp.get("kind") != "reg"
                else {
                    "id": bid,
                    "kind": "reg",
                    "register": bp.get("register"),
                    "min": bp.get("min"),
                    "max": bp.get("max"),
                    "condition": bp.get("condition") or None,
                }
                for bid, bp in sorted(self.bps.items())
            ]
        }

    def clear_all_breakpoints(self, _params: dict[str, Any]) -> dict[str, Any]:
        cleared = []
        for bid in list(self.bps):
            try:
                self.clear_breakpoint({"id": bid})
                cleared.append(bid)
            except BridgeError:
                pass
        return {"cleared": cleared}

    def poll_events(self, params: dict[str, Any]) -> dict[str, Any]:
        self._drain_stop()
        saw_reset = self._drain_reset_event()
        raw_bpid = params.get("breakpoint_id")
        filter_bpid: int | None = int(raw_bpid) if raw_bpid is not None else None
        events: list[dict[str, Any]] = []
        remaining: list[dict[str, Any]] = []
        for event in self.events:
            if saw_reset and event.get("type") == "stop" and event.get("raw") == "S05":
                continue
            self._enrich_event(event)
            event.pop("_pc98_enriched", None)
            if filter_bpid is not None and event.get("id") != filter_bpid:
                remaining.append(event)
                continue
            events.append(event)
        # When a filter is active, preserve unmatched events for subsequent polls.
        # When no filter, remaining is empty and self.events is fully consumed.
        self.events = remaining
        return {"events": events, "dropped": 0}

    def _drain_reset_event(self) -> bool:
        resp = self._lua_cmd_reply("pollreset")
        if resp == "NONE":
            return False
        if not resp.startswith("RESET:"):
            raise BridgeError(f"MAME reset poll failed: {resp}")
        pc_hex, sep, regs_hex = resp[len("RESET:") :].partition("|")
        event: dict[str, Any] = {"type": "reset", "raw": resp}
        try:
            pc = int.from_bytes(bytes.fromhex(pc_hex), "little")
            event["pc"] = pc
            event["address"] = pc
        except ValueError:
            event["pc_error"] = pc_hex
        if sep and regs_hex:
            try:
                event["regs"] = self._state_from_regs_hex(regs_hex)
            except ValueError:
                event["regs_error"] = "decode_failed"
        self.events.append(event)
        return True

    def _read_abs_hex(self, addr: int, length: int) -> str:
        if length < 0:
            raise BridgeError("length must be non-negative")
        resp = self._send_command(f"m{addr:x},{length:x}")
        if resp is None or resp.startswith("E"):
            raise BridgeError(f"GDB memory read failed: {resp}")
        return resp

    @staticmethod
    def _sha1_file(path: str) -> str:
        h = hashlib.sha1()
        with open(path, "rb") as f:
            while True:
                chunk = f.read(1024 * 1024)
                if not chunk:
                    break
                h.update(chunk)
        return h.hexdigest()

    def _read_region_bytes(self, memory_type: str, start: int, length: int) -> bytes:
        base = MEM_BASE.get(memory_type)
        if base is None:
            raise BridgeError(f"unsupported memory_type: {memory_type}")
        out = bytearray()
        offset = 0
        while offset < length:
            chunk = min(MAX_READ_CHUNK, length - offset)
            out.extend(bytes.fromhex(self._read_abs_hex(base + start + offset, chunk)))
            offset += chunk
        return bytes(out)

    def _write_region_bytes(self, memory_type: str, start: int, data: bytes) -> None:
        base = MEM_BASE.get(memory_type)
        if base is None:
            raise BridgeError(f"unsupported memory_type: {memory_type}")
        offset = 0
        while offset < len(data):
            chunk = data[offset : offset + MAX_READ_CHUNK]
            self._write_abs_hex(base + start + offset, chunk.hex())
            offset += len(chunk)

    def _write_abs_hex(self, addr: int, hexstr: str) -> None:
        if len(hexstr) % 2:
            raise BridgeError("hex must have even length")
        resp = self._send_command(f"M{addr:x},{len(hexstr) // 2:x}:{hexstr}")
        if resp != "OK":
            raise BridgeError(f"GDB memory write failed: {resp}")

    def _read_regs_hex(self) -> str:
        resp = self._send_command("g")
        if resp is None or resp.startswith("E"):
            raise BridgeError(f"GDB register read failed: {resp}")
        return resp

    def _write_regs_hex(self, regs_hex: str) -> None:
        if len(regs_hex) % 2:
            raise BridgeError("register hex must have even length")
        resp = self._send_command(f"G{regs_hex}")
        if resp != "OK":
            raise BridgeError(f"GDB register write failed: {resp}")

    def _load_regs_via_lua(self, regs_hex: str) -> dict[str, int]:
        resp = self._lua_cmd_reply("regload", regs_hex)
        if not resp.startswith("OK|"):
            raise BridgeError(f"MAME register load failed: {resp}")
        return self._state_from_regs_hex(resp[3:])

    def _register_probe(self, regs_hex: str, frames: int, addr: int, length: int) -> dict[str, Any]:
        resp = self._lua_cmd_reply("regprobe", f"{regs_hex}|{frames}|{addr:x}|{length:x}")
        parsed = self._parse_register_probe_response(resp)
        if len(parsed["hex"]) != length * 2:
            raise BridgeError(
                f"MAME register probe returned {len(parsed['hex']) // 2} bytes, expected {length}"
            )
        return parsed

    @staticmethod
    def _parse_register_probe_response(resp: str) -> dict[str, Any]:
        if not resp.startswith("HEX:"):
            raise BridgeError(f"MAME register probe failed: {resp}")
        fields: dict[str, str] = {}
        for part in resp.split("|"):
            key, sep, value = part.partition(":")
            if sep:
                fields[key] = value
        hexstr = fields.get("HEX", "")
        if len(hexstr) % 2:
            raise BridgeError(f"MAME register probe returned odd-length hex: {hexstr}")
        result: dict[str, Any] = {
            "hex": hexstr,
            "state_restore": STATE_RESTORE_INFO,
        }
        if frame := fields.get("FRAME"):
            try:
                result["frame"] = int(frame)
            except ValueError:
                result["frame_error"] = frame
        if regs_hex := fields.get("REGS"):
            try:
                result["regs"] = Bridge._state_from_regs_hex(regs_hex)
            except ValueError:
                result["regs_error"] = "decode_failed"
        return result

    def _lua_cmd(self, name: str, arg: str | None = None) -> str:
        resp = self._lua_cmd_reply(name, arg)
        if resp != "OK":
            raise BridgeError(f"MAME Lua command {name} failed: {resp}")
        return resp

    def _lua_cmd_reply(self, name: str, arg: str | None = None) -> str:
        self._require_lua_backend(f"Lua command {name}")
        payload = f"qEmucap,{name}"
        if arg is not None:
            payload += "," + arg.encode("utf-8").hex()
        resp = self._send_extension(payload)
        if not resp or resp.startswith("E"):
            raise BridgeError(f"MAME Lua command {name} failed: {resp}")
        return resp

    def _current_frame(self) -> int | None:
        try:
            return int(self._lua_cmd_reply("frame"))
        except (BridgeError, ValueError):
            return None

    def _lua_cmd_allow_stop(self, name: str, arg: str | None = None) -> str | None:
        payload = f"qEmucap,{name}"
        if arg is not None:
            payload += "," + arg.encode("utf-8").hex()
        resp = self._send_extension(payload, allow_stop=True)
        if resp == "OK":
            return self._drain_immediate_stops()
        if resp and resp[0] in ("S", "T"):
            self._drain_immediate_stops()
            return resp
        raise BridgeError(f"MAME Lua command {name} failed: {resp}")

    def _send_command(self, payload: str, allow_stop: bool = False) -> str | None:
        return self._consume_response(self.gdb.send(payload), allow_stop=allow_stop)

    def _send_extension(self, payload: str, allow_stop: bool = False) -> str:
        resp = self._consume_response(self.gdb.send_extension(payload), allow_stop=allow_stop)
        return "" if resp is None else resp

    def _consume_response(self, resp: str | None, allow_stop: bool = False) -> str | None:
        while resp and resp[0] in ("S", "T"):
            self._note_stop(resp, enrich=allow_stop)
            if allow_stop:
                return resp
            resp = self.gdb.recv()
        return resp

    def _normalize_buttons(self, raw_buttons: Any) -> list[str]:
        if not isinstance(raw_buttons, list):
            raise BridgeError("buttons must be a list")
        out = []
        allowed = set(PC98_INPUT_BUTTONS)
        for raw in raw_buttons:
            key = str(raw).strip().lower()
            key = PC98_INPUT_ALIASES.get(key, key)
            if key not in allowed:
                raise BridgeError(f"unsupported PC-98 key: {raw}")
            out.append(key)
        return out

    @staticmethod
    def _decode_regs(resp: str, names: list[str], width: int, state: dict[str, int]) -> None:
        chars = width * 2
        for idx, name in enumerate(names):
            start = idx * chars
            end = start + chars
            if end > len(resp):
                break
            state[f"cpu.{name}"] = int.from_bytes(bytes.fromhex(resp[start:end]), "little")

    @staticmethod
    def _state_from_regs_hex(resp: str) -> dict[str, int]:
        state: dict[str, int] = {}
        if len(resp) >= len(I386_REGS) * 8:
            Bridge._decode_regs(resp, I386_REGS, 4, state)
            if "cpu.eip" in state:
                state["cpu.offset_pc"] = state["cpu.eip"]
                state["cpu.pc"] = Bridge._segmented_pc(state, "cpu.cs", "cpu.eip")
        elif len(resp) >= len(I86_REGS) * 4:
            Bridge._decode_regs(resp, I86_REGS, 2, state)
            if "cpu.ip" in state:
                state["cpu.offset_pc"] = state["cpu.ip"]
                state["cpu.pc"] = Bridge._segmented_pc(state, "cpu.cs", "cpu.ip")
        else:
            state["cpu.raw_register_bytes"] = len(resp) // 2
        return state

    @staticmethod
    def _segmented_pc(state: dict[str, int], cs_key: str, ip_key: str) -> int:
        cs = state.get(cs_key, 0)
        ip = state.get(ip_key, 0)
        return ((cs << 4) + ip) & 0xFFFFFFFF

    @staticmethod
    def _parse_dasm(lines: list[str], count: int) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        for line in lines:
            if len(out) >= count:
                break
            raw = line.strip()
            if not raw:
                continue
            match = re.match(r"^([0-9A-Fa-f]+):\s*(.*)$", raw)
            if not match:
                continue
            addr = int(match.group(1), 16)
            rest = match.group(2).strip()
            parts = rest.split()
            byte_parts = []
            idx = 0
            while idx < len(parts) and re.fullmatch(r"[0-9A-Fa-f]{2}", parts[idx]):
                byte_parts.append(parts[idx].lower())
                idx += 1
            text = " ".join(parts[idx:]) if idx < len(parts) else rest
            item: dict[str, Any] = {"addr": addr, "text": text}
            if byte_parts:
                item["bytes"] = "".join(byte_parts)
            out.append(item)
        return out

    def _read_trace_rows(self) -> list[dict[str, Any]]:
        if self.tracing:
            try:
                self._lua_cmd("traceflush")
            except BridgeError:
                pass
        if not self.trace_path or not os.path.exists(self.trace_path):
            return []
        with open(self.trace_path, "r", encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
        rows = []
        for line in lines[-(TRACE_CAP * 4) :]:
            row = self._parse_trace_line(line)
            if row:
                rows.append(row)
        return rows[-TRACE_CAP:]

    @staticmethod
    def _parse_trace_line(line: str) -> dict[str, Any] | None:
        raw = line.strip()
        if not raw:
            return None
        match = re.match(r"^([0-9A-Fa-f]+):\s*(.*)$", raw)
        if not match:
            match = re.search(r"\b([0-9A-Fa-f]{4,8}):\s*(.*)$", raw)
        if not match:
            return {"raw": raw}
        pc = int(match.group(1), 16)
        rest = match.group(2).strip()
        parts = rest.split()
        byte_parts = []
        idx = 0
        while idx < len(parts) and re.fullmatch(r"[0-9A-Fa-f]{2}", parts[idx]):
            byte_parts.append(parts[idx].lower())
            idx += 1
        text = " ".join(parts[idx:]) if idx < len(parts) else rest
        row: dict[str, Any] = {"pc": pc, "text": text, "raw": raw}
        if byte_parts:
            row["bytes"] = "".join(byte_parts)
        return row

    def _note_stop(self, stop: str, enrich: bool = True) -> None:
        if stop and stop[0] in ("S", "T"):
            self.frozen = True
            event = self._stop_event(stop)
            if enrich:
                self._enrich_event(event)
            self.events.append(event)

    @staticmethod
    def _stop_event(stop: str) -> dict[str, Any]:
        event: dict[str, Any] = {"type": "stop", "signal": stop[1:3], "raw": stop}
        if not stop.startswith("T"):
            return event
        body = stop[3:]
        key, sep, rest = body.partition(":")
        if not sep:
            return event
        fields = {}
        parts = rest.split(";")
        raw_hex = parts[0]
        for item in parts[1:]:
            fkey, fsep, fval = item.partition(":")
            if fsep:
                fields[fkey] = fval
        try:
            addr = int.from_bytes(bytes.fromhex(raw_hex), "little")
        except ValueError:
            return event
        kind = {
            "hwbreak": "exec",
            "watch": "write",
            "rwatch": "read",
            "awatch": "access",
        }.get(key)
        if kind:
            event.update({"type": "breakpoint_hit", "kind": kind, "address": addr})
        elif key == "reset":
            event.update({"type": "reset", "pc": addr, "address": addr})
        elif key == "regwatch":
            event.update({"type": "register_break", "pc": addr, "address": addr})
        idx = fields.get("idx")
        if idx is not None:
            try:
                event["backend_id"] = int(idx)
            except ValueError:
                event["backend_id_error"] = idx
        regs_hex = fields.get("regs")
        if regs_hex:
            try:
                event["regs"] = Bridge._state_from_regs_hex(regs_hex)
            except ValueError:
                event["regs_error"] = "decode_failed"
        return event

    @staticmethod
    def _normalize_debug_register(raw_reg: str) -> tuple[str, str]:
        key = raw_reg.strip().lower()
        if key.startswith("cpu."):
            key = key[4:]
        found = PC98_DEBUG_REGS.get(key)
        if not found:
            valid = ", ".join(sorted(PC98_DEBUG_REGS))
            raise BridgeError(f"unsupported PC-98 register: {raw_reg}; valid: {valid}")
        return found

    def _breakpoint_condition(self, params: dict[str, Any], kind: str) -> str:
        clauses = []
        pc_min = params.get("pc_min")
        pc_max = params.get("pc_max")
        if pc_min is not None:
            pc_min_num = _num(pc_min)
            clauses.append(f"pc >= {pc_min_num:X}")
        if pc_max is not None:
            pc_max_num = _num(pc_max)
            clauses.append(f"pc <= {pc_max_num:X}")
        if pc_min is not None and pc_max is not None and _num(pc_min) > _num(pc_max):
            raise BridgeError("pc_min must be <= pc_max")

        has_value_filter = (
            params.get("value") is not None
            or params.get("value_mask") is not None
            or params.get("value_len") is not None
        )
        if has_value_filter:
            if kind not in ("read", "write"):
                raise BridgeError("value filters only apply to read/write breakpoints")
            if params.get("value") is None:
                raise BridgeError("value filter requires value")
            value_len = _num(params.get("value_len", 1))
            if value_len < 1 or value_len > 4:
                raise BridgeError("value_len must be 1..4 for MAME PC-98")
            all_bits = (1 << (value_len * 8)) - 1
            value = _num(params["value"]) & all_bits
            mask = _num(params.get("value_mask", all_bits)) & all_bits
            clauses.append(f"(wpdata & {mask:X}) == {value & mask:X}")

        return " && ".join(f"({clause})" for clause in clauses)

    def _parse_snapshot_specs(self, raw_specs: Any) -> list[dict[str, Any]]:
        if raw_specs is None:
            return []
        if not isinstance(raw_specs, list):
            raise BridgeError("snapshot must be a list")
        out = []
        for raw in raw_specs:
            parts = str(raw).split(":")
            if len(parts) != 3:
                raise BridgeError(f"invalid snapshot spec: {raw}")
            memory_type = parts[0]
            if memory_type not in MEM_BASE:
                raise BridgeError(f"unsupported snapshot memory_type: {memory_type}")
            address = _num(parts[1])
            length = _num(parts[2])
            if length < 0:
                raise BridgeError("snapshot length must be non-negative")
            if length > MAX_READ_CHUNK:
                raise BridgeError(f"snapshot length exceeds {MAX_READ_CHUNK} bytes")
            out.append({"memory_type": memory_type, "address": address, "length": length})
        return out

    def _find_bp_for_event(self, event: dict[str, Any]) -> tuple[int, dict[str, Any]] | None:
        if event.get("type") != "breakpoint_hit":
            return None
        event_kind = event.get("kind")
        event_addr = event.get("address")
        if not isinstance(event_addr, int):
            return None
        backend_id = event.get("backend_id")
        if isinstance(backend_id, int):
            for bid, bp in sorted(self.bps.items()):
                if int(bp.get("backend_id", -1)) == backend_id and bp.get("kind") == event_kind:
                    return bid, bp
        for bid, bp in sorted(self.bps.items()):
            start = int(bp["addr"])
            end = start + int(bp["size"]) - 1
            if start <= event_addr <= end and bp.get("kind") == event_kind:
                return bid, bp
        return None

    def _find_regwatch_for_event(self, event: dict[str, Any]) -> tuple[int, dict[str, Any]] | None:
        if event.get("type") != "register_break":
            return None
        backend_id = event.get("backend_id")
        if isinstance(backend_id, int):
            for bid, bp in sorted(self.bps.items()):
                if int(bp.get("backend_id", -1)) == backend_id and bp.get("kind") == "reg":
                    return bid, bp
        return None

    def _enrich_event(self, event: dict[str, Any]) -> None:
        if event.get("_pc98_enriched"):
            return
        if event.get("type") == "stop":
            if "regs" not in event and not self.enriching_stop:
                self.enriching_stop = True
                try:
                    event["regs"] = self._state_from_regs_hex(self._read_regs_hex())
                except BridgeError as err:
                    event["regs_error"] = str(err)
                finally:
                    self.enriching_stop = False
            regs = event.get("regs")
            pc_values = []
            if isinstance(regs, dict):
                for key in ("cpu.offset_pc", "cpu.pc"):
                    value = regs.get(key)
                    if isinstance(value, int):
                        pc_values.append(value)
                if "pc" not in event and isinstance(regs.get("cpu.pc"), int):
                    event["pc"] = regs["cpu.pc"]
            for bid, bp in sorted(self.bps.items()):
                if bp.get("kind") == "exec" and int(bp.get("addr", -1)) in pc_values:
                    event.update(
                        {
                            "type": "breakpoint_hit",
                            "kind": "exec",
                            "address": int(bp["addr"]),
                            "id": bid,
                            "breakpoint_id": bid,
                        }
                    )
                    if bp.get("pause_on_hit", True):
                        self.frozen = True
                    break
            if event.get("type") == "stop":
                event["_pc98_enriched"] = True
                return
        if event.get("type") == "register_break":
            match = self._find_regwatch_for_event(event)
            if match:
                bid, bp = match
                event["id"] = bid
                event["breakpoint_id"] = bid
                event["register"] = bp.get("register")
                event["min"] = bp.get("min")
                event["max"] = bp.get("max")
                state_key = str(bp.get("state_key"))
            else:
                state_key = ""
            if "regs" not in event and not self.enriching_stop:
                self.enriching_stop = True
                try:
                    event["regs"] = self._state_from_regs_hex(self._read_regs_hex())
                except BridgeError as err:
                    event["regs_error"] = str(err)
                finally:
                    self.enriching_stop = False
            regs = event.get("regs")
            if isinstance(regs, dict):
                if "pc" not in event and isinstance(regs.get("cpu.pc"), int):
                    event["pc"] = regs["cpu.pc"]
                if state_key and isinstance(regs.get(state_key), int):
                    event["value"] = regs[state_key]
            event["_pc98_enriched"] = True
            return
        if event.get("type") != "breakpoint_hit":
            event["_pc98_enriched"] = True
            return
        match = self._find_bp_for_event(event)
        if match:
            bid, bp = match
            event["id"] = bid
            if bp.get("pause_on_hit", True):
                self.frozen = True
            snapshots = bp.get("snapshots") or []
        else:
            snapshots = []
        if "regs" not in event and not self.enriching_stop:
            self.enriching_stop = True
            try:
                event["regs"] = self._state_from_regs_hex(self._read_regs_hex())
            except BridgeError as err:
                event["regs_error"] = str(err)
            finally:
                self.enriching_stop = False
        if snapshots and "snapshot" not in event and not self.enriching_stop:
            captured = []
            self.enriching_stop = True
            try:
                for spec in snapshots:
                    memory_type = str(spec["memory_type"])
                    address = int(spec["address"])
                    length = int(spec["length"])
                    captured.append(
                        {
                            "memory_type": memory_type,
                            "address": address,
                            "hex": self._read_region_bytes(memory_type, address, length).hex(),
                        }
                    )
                event["snapshot"] = captured
            except BridgeError as err:
                event["snapshot_error"] = str(err)
            finally:
                self.enriching_stop = False
        event["_pc98_enriched"] = True

    def _drain_stop(self) -> None:
        if self.frozen:
            return
        stop = self.gdb.recv_nonblocking()
        if stop and stop[0] in ("S", "T"):
            self._note_stop(stop)

    def _drain_immediate_stops(self) -> str | None:
        first = None
        for _ in range(12):
            stop = self.gdb.recv_nonblocking()
            if stop and stop[0] in ("S", "T"):
                if first is None:
                    first = stop
                self._note_stop(stop, enrich=False)
                continue
            if stop is None:
                time.sleep(0.005)
                continue
            return first
        return first


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    emucap_port = int(sys.argv[1])
    gdb_host, gdb_port = "127.0.0.1", 3264
    if len(sys.argv) >= 3 and ":" in sys.argv[2]:
        gdb_host, raw_port = sys.argv[2].rsplit(":", 1)
        gdb_port = int(raw_port)
    print(
        f"[mame-pc98] connecting backend={LUA_BACKEND} gdb={gdb_host}:{gdb_port} emucap=127.0.0.1:{emucap_port}",
        file=sys.stderr,
    )
    bridge = Bridge(GdbRsp(gdb_host, gdb_port))
    retry = 0.05
    while True:
        try:
            sock = socket.create_connection(("127.0.0.1", emucap_port), timeout=1.0)
            sock.settimeout(None)
            print("[mame-pc98] connected", file=sys.stderr)
            _serve_emucap_session(sock, bridge)
            print("[mame-pc98] emucap disconnected; reconnecting", file=sys.stderr)
            retry = 0.05
        except OSError as err:
            print(f"[mame-pc98] emucap unavailable ({err}); retrying", file=sys.stderr)
        time.sleep(retry)
        retry = min(retry * 2.0, 2.0)


def _serve_emucap_session(sock: socket.socket, bridge: Bridge) -> None:
    with sock, sock.makefile("rwb", buffering=0) as fp:
      for raw in fp:
        if not raw.strip():
            continue
        try:
            env = json.loads(raw)
        except json.JSONDecodeError:
            continue
        rid = env.get("id", 0)
        method = env.get("method", "")
        params = env.get("params") or {}
        handler = getattr(bridge, method, None)
        if handler is None:
            resp = {
                "id": rid,
                "ok": False,
                "error": {"kind": "unknown_method", "message": str(method)},
            }
        else:
            try:
                resp = {"id": rid, "ok": True, "result": handler(params)}
            except BridgeError as err:
                resp = {
                    "id": rid,
                    "ok": False,
                    "error": {"kind": "emulator_error", "message": str(err)},
                }
            except Exception as err:  # noqa: BLE001 - PoC bridge returns errors to MCP.
                resp = {
                    "id": rid,
                    "ok": False,
                    "error": {"kind": "bridge_error", "message": repr(err)},
                }
        payload = (json.dumps(resp, separators=(",", ":")) + "\n").encode("utf-8")
        if not _write_front_response(sock, payload):
            return


def _write_front_response(sock: socket.socket, payload: bytes) -> bool:
    """Bound a bridge-to-MCP write while leaving the next request read blocking."""
    ok = False
    try:
        sock.settimeout(FRONT_WRITE_TIMEOUT)
        sock.sendall(payload)
        ok = True
    except OSError:
        pass
    finally:
        try:
            sock.settimeout(None)
        except OSError:
            ok = False
    return ok


if __name__ == "__main__":
    raise SystemExit(main())
