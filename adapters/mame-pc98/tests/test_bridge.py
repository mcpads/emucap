#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
BRIDGE_PATH = ROOT / "emucap-gdb-bridge.py"
SLED_PATH = ROOT / "scripts" / "make_atomic_restore_sled.py"

spec = importlib.util.spec_from_file_location("pc98_bridge", BRIDGE_PATH)
assert spec is not None and spec.loader is not None
pc98_bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pc98_bridge)

sled_spec = importlib.util.spec_from_file_location("pc98_sled", SLED_PATH)
assert sled_spec is not None and sled_spec.loader is not None
pc98_sled = importlib.util.module_from_spec(sled_spec)
sled_spec.loader.exec_module(pc98_sled)


class FakeGdb:
    def __init__(self, responses: dict[str, str | list[str]] | None = None):
        self.responses = dict(responses or {})
        self.calls: list[str] = []
        self.extension_calls: list[str] = []
        self.timeouts: list[float] = []
        self._timeout = 5.0

    def get_timeout(self) -> float:
        return self._timeout

    def set_timeout(self, t: float) -> None:
        self._timeout = t
        self.timeouts.append(t)

    def send(self, payload: str, expect_reply: bool = True) -> str | None:
        self.calls.append(payload)
        if not expect_reply:
            return None
        return self._pop_response(payload)

    def send_extension(self, payload: str) -> str:
        self.extension_calls.append(payload)
        return self._pop_response(payload)

    def recv(self) -> str | None:
        self.calls.append("<recv>")
        return self._pop_response("<recv>")

    def recv_nonblocking(self) -> str | None:
        self.calls.append("<recv_nonblocking>")
        return self._pop_response("<recv_nonblocking>", default=None)

    def _pop_response(self, payload: str, default: str | None = "") -> str | None:
        if payload not in self.responses:
            return default
        response = self.responses[payload]
        if isinstance(response, list):
            if not response:
                return default
            return response.pop(0)
        return response


def i386_regs_hex(**values: int) -> str:
    out = bytearray()
    for name in pc98_bridge.I386_REGS:
        out.extend(int(values.get(name, 0)).to_bytes(4, "little"))
    return out.hex()


class BridgeTests(unittest.TestCase):
    def test_front_response_write_is_bounded_and_restores_blocking_reads(self) -> None:
        class FakeFront:
            def __init__(self, fail: bool = False) -> None:
                self.fail = fail
                self.timeouts: list[float | None] = []
                self.payloads: list[bytes] = []

            def settimeout(self, timeout: float | None) -> None:
                self.timeouts.append(timeout)

            def sendall(self, payload: bytes) -> None:
                if self.fail:
                    raise TimeoutError("front write stalled")
                self.payloads.append(payload)

        success = FakeFront()
        self.assertTrue(pc98_bridge._write_front_response(success, b"reply\n"))
        self.assertEqual(success.timeouts, [pc98_bridge.FRONT_WRITE_TIMEOUT, None])
        self.assertEqual(success.payloads, [b"reply\n"])

        stalled = FakeFront(fail=True)
        self.assertFalse(pc98_bridge._write_front_response(stalled, b"reply\n"))
        self.assertEqual(stalled.timeouts, [pc98_bridge.FRONT_WRITE_TIMEOUT, None])

    def test_hello_advertises_lua_control_methods(self) -> None:
        bridge = pc98_bridge.Bridge(FakeGdb({"?": ""}))
        hello = bridge.hello({})
        self.assertEqual(hello["backend"], "lua-gdbstub")
        self.assertIn("screenshot", hello["methods"])
        self.assertIn("set_input", hello["methods"])
        self.assertIn("run_frames", hello["methods"])
        self.assertIn("load_state", hello["methods"])
        self.assertIn("probe", hello["methods"])
        self.assertTrue(hello["capability_notes"]["frame_step"])
        self.assertTrue(hello["state_restore"]["deterministic_replay"])
        self.assertTrue(hello["state_restore"]["post_restore_instruction_exact"])

    def test_hello_advertises_memory_types_matching_read(self) -> None:
        # hello.memory_types는 read_memory가 받는 집합(MEM_BASE 키)과 정확히 일치해야 한다
        # (MCP가 status.memory_types로 표면화 — 누락/과잉 광고 금지, 계약 #1).
        bridge = pc98_bridge.Bridge(FakeGdb({"?": ""}))
        hello = bridge.hello({})
        self.assertEqual(set(hello["memory_types"]), set(pc98_bridge.MEM_BASE.keys()))
        for mt in ("ram", "tvram", "gvram_b", "gvram_r", "gvram_g", "gvram_i"):
            self.assertIn(mt, hello["memory_types"])
        self.assertTrue(hello["state_restore"]["hidden_device_state"])
        self.assertTrue(hello["state_restore"]["save_manager_items"])

    def test_read_memory_rejects_access_straddling_region_end(self) -> None:
        gdb = FakeGdb()
        bridge = pc98_bridge.Bridge(gdb)
        with self.assertRaisesRegex(pc98_bridge.BridgeError, "tvram access out of range"):
            bridge.read_memory({"memory_type": "tvram", "address": 0x3FFF, "length": 2})
        self.assertEqual(gdb.calls, ["?"], "reject before GDB read")

    def test_write_memory_rejects_access_straddling_region_end(self) -> None:
        gdb = FakeGdb()
        bridge = pc98_bridge.Bridge(gdb)
        with self.assertRaisesRegex(pc98_bridge.BridgeError, "tvram access out of range"):
            bridge.write_memory({"memory_type": "tvram", "address": 0x3FFF, "hex": "aabb"})
        self.assertEqual(gdb.calls, ["?"], "reject before GDB write")

    def test_memory_access_ending_exactly_at_region_end_is_allowed(self) -> None:
        gdb = FakeGdb({"ma3fff,1": "7f", "Ma3fff,1:80": "OK"})
        bridge = pc98_bridge.Bridge(gdb)
        self.assertEqual(
            bridge.read_memory({"memory_type": "tvram", "address": 0x3FFF, "length": 1}),
            {"hex": "7f"},
        )
        self.assertEqual(
            bridge.write_memory({"memory_type": "tvram", "address": 0x3FFF, "hex": "80"}),
            {"written": 1},
        )

    def test_screenshot_reports_frame_state_and_hash_provenance(self) -> None:
        bridge = pc98_bridge.Bridge(FakeGdb())
        frames = iter((42, 42))
        bridge._current_frame = lambda: next(frames)  # type: ignore[method-assign]

        png = b"\x89PNG\r\n\x1a\nfake"

        def fake_lua(name: str, path: str) -> None:
            self.assertEqual(name, "snapshot")
            pathlib.Path(path).write_bytes(png)

        bridge._lua_cmd = fake_lua  # type: ignore[method-assign]
        result = bridge.screenshot({})

        self.assertEqual(result["png_base64"], pc98_bridge.base64.b64encode(png).decode("ascii"))
        self.assertEqual(result["sha256"], pc98_bridge.hashlib.sha256(png).hexdigest())
        self.assertEqual(result["byte_len"], len(png))
        self.assertEqual(result["state"], "frozen")
        self.assertEqual(result["frame_before"], 42)
        self.assertEqual(result["frame_after"], 42)
        self.assertTrue(result["frame_stable"])
        self.assertEqual(result["freshness"], "unverified")
        self.assertEqual(result["frame_binding"], "unverified")

    def test_load_state_reports_lua_register_write_drift_without_extra_stop(self) -> None:
        regs = i386_regs_hex(eip=0x8000, cs=0)
        drifted_regs = i386_regs_hex(eip=0x8004, cs=0)
        ram_hex = bytes(0x20).hex()
        gdb = FakeGdb(
            {
                "?": "",
                "qEmucap,stop": "OK",
                f"M0,20:{ram_hex}": "OK",
                "qEmucap,regload," + regs.encode("utf-8").hex(): "OK|" + drifted_regs,
            }
        )
        bridge = pc98_bridge.Bridge(gdb)

        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            manifest = {
                "format": pc98_bridge.LEGACY_STATE_FORMAT,
                "regions": [{"memory_type": "ram", "file": "ram.bin"}],
                "registers_hex": regs,
            }
            with zipfile.ZipFile(state_path, "w") as zf:
                zf.writestr("state.json", json.dumps(manifest))
                zf.writestr("ram.bin", bytes(0x20))

            result = bridge.load_state({"path": str(state_path)})

        self.assertEqual(result["restore_strategy"], "lua_register_load_hold")
        self.assertFalse(result["post_restore_instruction_exact"])
        self.assertFalse(result["observed_register_packet_matches_target"])
        self.assertEqual(result["observed_eip"], 0x8004)
        self.assertEqual(
            gdb.extension_calls,
            ["qEmucap,stop", "qEmucap,regload," + regs.encode("utf-8").hex()],
        )
        self.assertEqual(
            [call for call in gdb.calls if call != "?"],
            [
                f"M0,20:{ram_hex}",
            ],
        )

    def test_save_state_writes_v2_region_bundle(self) -> None:
        regs = i386_regs_hex(eip=0x4567, cs=0x1234)
        gdb = FakeGdb(
            {
                "?": "",
                "qEmucap,stop": "OK",
                "g": regs,
            }
        )
        bridge = pc98_bridge.Bridge(gdb)

        def fake_read_region(memory_type: str, start: int, length: int) -> bytes:
            return bytes([len(memory_type) & 0xFF]) * length

        def fake_save_items(path: str) -> dict[str, int]:
            (pathlib.Path(path) / "manifest.txt").write_text("0|1|2|2|item_000000.bin\n")
            (pathlib.Path(path) / "item_000000.bin").write_bytes(b"\x12\x34")
            return {"items": 1, "skipped": 0}

        bridge._read_region_bytes = fake_read_region  # type: ignore[method-assign]
        bridge._save_lua_save_items = fake_save_items  # type: ignore[method-assign]

        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            result = bridge.save_state({"path": str(state_path)})
            with zipfile.ZipFile(state_path, "r") as zf:
                manifest = json.loads(zf.read("state.json"))
                ram = zf.read("ram.bin")
                save_manifest = zf.read("saveitems/manifest.txt")
                save_item = zf.read("saveitems/item_000000.bin")

        self.assertEqual(result["format"], pc98_bridge.STATE_FORMAT)
        self.assertEqual(manifest["format"], pc98_bridge.STATE_FORMAT)
        self.assertEqual(manifest["registers_hex"], regs)
        self.assertEqual(manifest["save_items"], {"items": 1, "skipped": 0, "dir": "saveitems"})
        self.assertEqual(result["save_items"], {"items": 1, "skipped": 0, "dir": "saveitems"})
        self.assertEqual(ram, bytes([3]) * pc98_bridge.REGION_SIZE["ram"])
        self.assertEqual(save_manifest, b"0|1|2|2|item_000000.bin\n")
        self.assertEqual(save_item, b"\x12\x34")
        self.assertTrue(result["state_restore"]["deterministic_replay"])

    def test_load_state_uses_lua_regload_for_v2_bundle(self) -> None:
        regs = i386_regs_hex(eip=0x8000, cs=0)
        ram_hex = bytes(0x20).hex()
        gdb = FakeGdb({"?": "", "qEmucap,stop": "OK"})
        gdb.responses[f"M0,20:{ram_hex}"] = "OK"
        gdb.responses["qEmucap,regload," + regs.encode("utf-8").hex()] = "OK|" + regs
        bridge = pc98_bridge.Bridge(gdb)
        restored_paths: list[str] = []

        def fake_load_items(path: str) -> dict[str, int]:
            restored_paths.append(path)
            self.assertEqual((pathlib.Path(path) / "manifest.txt").read_text(), "0|1|1|1|item_000000.bin\n")
            self.assertEqual((pathlib.Path(path) / "item_000000.bin").read_bytes(), b"\xaa")
            return {"save_items_restored": 1, "save_items_skipped": 0}

        bridge._load_lua_save_items = fake_load_items  # type: ignore[method-assign]

        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            manifest = {
                "format": pc98_bridge.STATE_FORMAT,
                "regions": [{"memory_type": "ram", "file": "ram.bin"}],
                "registers_hex": regs,
                "save_items": {"dir": "saveitems", "items": 1, "skipped": 0},
            }
            with zipfile.ZipFile(state_path, "w") as zf:
                zf.writestr("state.json", json.dumps(manifest))
                zf.writestr("ram.bin", bytes(0x20))
                zf.writestr("saveitems/manifest.txt", "0|1|1|1|item_000000.bin\n")
                zf.writestr("saveitems/item_000000.bin", b"\xaa")

            result = bridge.load_state({"path": str(state_path)})

        self.assertEqual(len(restored_paths), 1)
        self.assertEqual(result["restore_strategy"], "lua_register_load_hold")
        self.assertEqual(result["save_items_restored"], 1)
        self.assertEqual(result["save_items_skipped"], 0)
        self.assertTrue(result["post_restore_instruction_exact"])
        self.assertTrue(result["observed_register_packet_matches_target"])
        self.assertEqual(result["observed_eip"], 0x8000)

    def test_probe_restores_regions_then_uses_lua_regprobe(self) -> None:
        regs = i386_regs_hex(eip=0x8000, cs=0)
        ram_hex = bytes(0x20).hex()
        gdb = FakeGdb({"?": ""})
        gdb.responses["qEmucap,stop"] = "OK"
        gdb.responses[f"M0,20:{ram_hex}"] = "OK"
        bridge = pc98_bridge.Bridge(gdb)

        calls: list[tuple[str, int, int, int]] = []

        def fake_probe(regs_hex: str, frames: int, addr: int, length: int) -> dict[str, object]:
            self.assertEqual(regs_hex, regs)
            calls.append((regs_hex, frames, addr, length))
            return {"hex": "81", "frame": 12}

        restored_paths: list[str] = []

        def fake_load_items(path: str) -> dict[str, int]:
            restored_paths.append(path)
            self.assertEqual((pathlib.Path(path) / "item_000000.bin").read_bytes(), b"\xbb")
            return {"save_items_restored": 1, "save_items_skipped": 0}

        bridge._register_probe = fake_probe  # type: ignore[method-assign]
        bridge._load_lua_save_items = fake_load_items  # type: ignore[method-assign]

        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            manifest = {
                "format": pc98_bridge.STATE_FORMAT,
                "regions": [{"memory_type": "ram", "file": "ram.bin"}],
                "registers_hex": regs,
                "save_items": {"dir": "saveitems", "items": 1, "skipped": 0},
            }
            with zipfile.ZipFile(state_path, "w") as zf:
                zf.writestr("state.json", json.dumps(manifest))
                zf.writestr("ram.bin", bytes(0x20))
                zf.writestr("saveitems/manifest.txt", "0|1|1|1|item_000000.bin\n")
                zf.writestr("saveitems/item_000000.bin", b"\xbb")

            result = bridge.probe(
                {"state": str(state_path), "frame": 5, "memory_type": "ram", "address": 0x100, "length": 1}
            )

        self.assertEqual(result["hex"], "81")
        self.assertEqual(result["save_items_restored"], 1)
        self.assertEqual(result["save_items_skipped"], 0)
        self.assertEqual(len(restored_paths), 1)
        self.assertEqual(len(calls), 1)
        _, frames, addr, length = calls[0]
        self.assertEqual((frames, addr, length), (5, 0x100, 1))

    def test_extract_save_items_rejects_unsafe_members(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            with zipfile.ZipFile(state_path, "w") as zf:
                zf.writestr("saveitems/manifest.txt", "")
                zf.writestr("saveitems/../bad.bin", b"")
            with zipfile.ZipFile(state_path, "r") as zf:
                with self.assertRaises(pc98_bridge.BridgeError):
                    pc98_bridge.Bridge._extract_save_items(
                        zf,
                        {"save_items": {"dir": "saveitems"}},
                        td,
                    )

    def test_extract_save_items_requires_manifest_when_declared(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            state_path = pathlib.Path(td) / "pc98.state.zip"
            with zipfile.ZipFile(state_path, "w") as zf:
                zf.writestr("saveitems/item_000000.bin", b"")
            with zipfile.ZipFile(state_path, "r") as zf:
                with self.assertRaises(pc98_bridge.BridgeError):
                    pc98_bridge.Bridge._extract_save_items(
                        zf,
                        {"save_items": {"dir": "saveitems"}},
                        td,
                    )

    def test_atomic_restore_sled_discards_stale_save_items(self) -> None:
        regs = i386_regs_hex(eip=0x1000, cs=0)
        with tempfile.TemporaryDirectory() as td:
            base_path = pathlib.Path(td) / "base.state.zip"
            sled_path = pathlib.Path(td) / "sled.state.zip"
            manifest = {
                "format": pc98_bridge.STATE_FORMAT,
                "regions": [{"memory_type": "ram", "file": "ram.bin"}],
                "registers_hex": regs,
                "save_items": {"dir": "saveitems", "items": 1, "skipped": 0},
            }
            with zipfile.ZipFile(base_path, "w") as zf:
                zf.writestr("state.json", json.dumps(manifest))
                zf.writestr("ram.bin", bytes(pc98_bridge.REGION_SIZE["ram"]))
                zf.writestr("saveitems/manifest.txt", "0|1|1|1|item_000000.bin\n")
                zf.writestr("saveitems/item_000000.bin", b"\x00")

            probe = pc98_sled.make_sled(base_path, sled_path, 0x8000, 0x9000)

            with zipfile.ZipFile(sled_path, "r") as zf:
                out_manifest = json.loads(zf.read("state.json"))
                names = zf.namelist()

        self.assertTrue(probe["discarded_save_items"])
        self.assertNotIn("save_items", out_manifest)
        self.assertFalse(any(name.startswith("saveitems/") for name in names))

    # ── pause_on_hit stored in bps + freezes bridge on matching hit ────

    def test_set_breakpoint_pause_on_hit_stored_and_freezes_on_hit(self) -> None:
        setpoint_key = "qEmucap,setpoint," + "0|100|1|1|".encode().hex()
        gdb = FakeGdb({"?": "", setpoint_key: "BP:3"})
        bridge = pc98_bridge.Bridge(gdb)
        result = bridge.set_breakpoint({"kind": "exec", "start": 0x100, "pause_on_hit": True})
        bid = result["id"]
        # Flag must be stored
        self.assertIn("pause_on_hit", bridge.bps[bid])
        self.assertTrue(bridge.bps[bid]["pause_on_hit"])

        # Simulate _enrich_event receiving a stop whose PC matches the BP.
        # Pre-populate regs so _enrich_event skips the GDB read.
        regs = pc98_bridge.Bridge._state_from_regs_hex(i386_regs_hex(eip=0x100, cs=0))
        event: dict = {"type": "stop", "signal": "05", "raw": "S05", "regs": regs}
        bridge.frozen = False
        bridge._enrich_event(event)
        # The stop must be promoted to breakpoint_hit
        self.assertEqual(event.get("type"), "breakpoint_hit")
        self.assertEqual(event.get("id"), bid)
        # And the bridge must have set frozen=True
        self.assertTrue(bridge.frozen)

    def test_set_breakpoint_pause_on_hit_false_sends_zero_and_no_freeze(self) -> None:
        # pause_on_hit=false → setpoint pause 필드 "0", hit해도 bridge frozen 안 됨(트레이스포인트).
        setpoint_key = "qEmucap,setpoint," + "0|200|1|0|".encode().hex()
        gdb = FakeGdb({"?": "", setpoint_key: "BP:4"})
        bridge = pc98_bridge.Bridge(gdb)
        result = bridge.set_breakpoint({"kind": "exec", "start": 0x200, "pause_on_hit": False})
        bid = result["id"]
        self.assertFalse(bridge.bps[bid]["pause_on_hit"])
        regs = pc98_bridge.Bridge._state_from_regs_hex(i386_regs_hex(eip=0x200, cs=0))
        event: dict = {"type": "stop", "signal": "05", "raw": "S05", "regs": regs}
        bridge.frozen = False
        bridge._enrich_event(event)
        self.assertEqual(event.get("type"), "breakpoint_hit")
        self.assertFalse(bridge.frozen)

    # ── kind="access" maps to zkind "4" ────────────────────────────────

    def test_set_breakpoint_access_kind_sends_zkind_4(self) -> None:
        setpoint_key = "qEmucap,setpoint," + "4|0|1|1|".encode().hex()
        gdb = FakeGdb({"?": "", setpoint_key: "WP:7"})
        bridge = pc98_bridge.Bridge(gdb)
        result = bridge.set_breakpoint({"kind": "access", "start": 0})
        self.assertIn("id", result)
        bid = result["id"]
        self.assertEqual(bridge.bps[bid]["kind"], "access")
        self.assertEqual(bridge.bps[bid]["zkind"], "4")

    # ── poll_events honours breakpoint_id filter ───────────────────────

    def test_poll_events_filters_by_breakpoint_id(self) -> None:
        gdb = FakeGdb({"?": "", "qEmucap,pollreset": "NONE"})
        bridge = pc98_bridge.Bridge(gdb)
        # bridge.frozen is already True from __init__ (? → "")
        bridge.events = [
            {"type": "breakpoint_hit", "id": 1, "kind": "exec", "address": 0x100, "_pc98_enriched": True},
            {"type": "breakpoint_hit", "id": 2, "kind": "exec", "address": 0x200, "_pc98_enriched": True},
            {"type": "stop", "signal": "05", "raw": "S05", "_pc98_enriched": True},
        ]
        result = bridge.poll_events({"breakpoint_id": 1})
        self.assertEqual(len(result["events"]), 1)
        self.assertEqual(result["events"][0]["id"], 1)
        # Unmatched events (id=2 and the stop) must be preserved for later polls
        self.assertEqual(len(bridge.events), 2)

    def test_poll_events_no_filter_returns_all(self) -> None:
        gdb = FakeGdb({"?": "", "qEmucap,pollreset": "NONE"})
        bridge = pc98_bridge.Bridge(gdb)
        bridge.events = [
            {"type": "breakpoint_hit", "id": 1, "kind": "exec", "address": 0x100, "_pc98_enriched": True},
            {"type": "breakpoint_hit", "id": 2, "kind": "exec", "address": 0x200, "_pc98_enriched": True},
        ]
        result = bridge.poll_events({})
        self.assertEqual(len(result["events"]), 2)

    def test_poll_events_filter_preserves_unmatched_for_later_poll(self) -> None:
        """filtered poll must NOT destroy events for other ids."""
        gdb = FakeGdb({"?": "", "qEmucap,pollreset": "NONE"})
        bridge = pc98_bridge.Bridge(gdb)
        bridge.events = [
            {"type": "breakpoint_hit", "id": 1, "kind": "exec", "address": 0x100, "_pc98_enriched": True},
            {"type": "breakpoint_hit", "id": 2, "kind": "exec", "address": 0x200, "_pc98_enriched": True},
        ]
        # Poll for id=1 only
        result1 = bridge.poll_events({"breakpoint_id": 1})
        self.assertEqual(len(result1["events"]), 1)
        self.assertEqual(result1["events"][0]["id"], 1)
        # id=2 event must still be in the queue
        self.assertEqual(len(bridge.events), 1)
        self.assertEqual(bridge.events[0].get("id"), 2)
        # Second poll (unfiltered) must return the preserved id=2 event
        result2 = bridge.poll_events({})
        self.assertEqual(len(result2["events"]), 1)
        self.assertEqual(result2["events"][0]["id"], 2)
        self.assertEqual(bridge.events, [])

    # ── clear_breakpoint is idempotent when Lua returns not-found ───────

    def test_clear_breakpoint_stale_id_is_idempotent(self) -> None:
        clearpoint_key = "qEmucap,clearpoint," + "bp|5".encode().hex()
        gdb = FakeGdb({"?": "", clearpoint_key: "E00"})
        bridge = pc98_bridge.Bridge(gdb)
        # Manually register a stale BP (backend_id 5 no longer exists in MAME)
        bridge.bps[1] = {
            "kind": "exec",
            "zkind": "0",
            "addr": 0x100,
            "size": 1,
            "backend": "bp",
            "backend_id": 5,
            "condition": "",
            "snapshots": [],
        }
        # Must not raise; must remove the entry
        result = bridge.clear_breakpoint({"id": 1})
        self.assertEqual(result["cleared"], 1)
        self.assertNotIn(1, bridge.bps)

    def test_clear_breakpoint_connection_error_propagates(self) -> None:
        """a real transport failure must NOT be swallowed as idempotent success."""

        class BrokenGdb(FakeGdb):
            def send_extension(self, payload: str) -> str:
                raise pc98_bridge.BridgeError("connection closed")

        gdb = BrokenGdb({"?": ""})
        bridge = pc98_bridge.Bridge(gdb)
        bridge.bps[1] = {
            "kind": "exec",
            "zkind": "0",
            "addr": 0x100,
            "size": 1,
            "backend": "bp",
            "backend_id": 5,
            "condition": "",
            "snapshots": [],
        }
        # Must raise — connection error must not be swallowed
        with self.assertRaises(pc98_bridge.BridgeError):
            bridge.clear_breakpoint({"id": 1})
        # Entry must NOT have been silently removed
        self.assertIn(1, bridge.bps)


    def test_run_frames_large_n_extends_recv_timeout(self) -> None:
        gdb = FakeGdb({"?": ""})
        gdb.responses["qEmucap,runframes," + "3000".encode("utf-8").hex()] = "OK"
        bridge = pc98_bridge.Bridge(gdb)
        bridge.run_frames({"n": 3000})
        self.assertTrue(gdb.timeouts, "run_frames must scale the recv timeout for large N")
        self.assertGreater(max(gdb.timeouts), 5.0)
        self.assertEqual(gdb.get_timeout(), 5.0)  # restored after the op

    def test_press_buttons_waits_for_terminal_reply_and_scales_timeout(self) -> None:
        command = "qEmucap,press," + "3000:enter".encode("utf-8").hex()
        gdb = FakeGdb({"?": "", command: "OK", "qEmucap,frame": "42"})
        bridge = pc98_bridge.Bridge(gdb)

        result = bridge.press_buttons({"buttons": ["start"], "frames": 3000})

        self.assertEqual(
            result,
            {
                "status": "completed",
                "buttons": ["enter"],
                "frames": 3000,
                "frame": 42,
                "state": "running",
            },
        )
        self.assertGreater(max(gdb.timeouts), 5.0)
        self.assertEqual(gdb.get_timeout(), 5.0)

    def test_press_buttons_reports_breakpoint_interruption(self) -> None:
        command = "qEmucap,press," + "10:enter".encode("utf-8").hex()
        stop = "T05hwbreak:01000000;idx:2;"
        gdb = FakeGdb({"?": "", command: stop, "qEmucap,frame": "77"})
        bridge = pc98_bridge.Bridge(gdb)

        result = bridge.press_buttons({"buttons": ["start"], "frames": 10})

        self.assertEqual(result["status"], "interrupted")
        self.assertEqual(result["reason"], "breakpoint")
        self.assertEqual(result["raw"], stop)
        self.assertEqual(result["buttons"], ["enter"])
        self.assertEqual(result["frame"], 77)
        self.assertTrue(bridge.frozen)

    def test_run_frames_restores_timeout_on_small_n(self) -> None:
        gdb = FakeGdb({"?": ""})
        gdb.responses["qEmucap,runframes," + "10".encode("utf-8").hex()] = "OK"
        bridge = pc98_bridge.Bridge(gdb)
        bridge.run_frames({"n": 10})
        self.assertEqual(gdb.get_timeout(), 5.0)

    def test_step_frames_extends_recv_timeout(self) -> None:
        gdb = FakeGdb({"?": ""})
        gdb.responses["qEmucap,framestep," + "5000".encode("utf-8").hex()] = "OK"
        bridge = pc98_bridge.Bridge(gdb)
        bridge.step({"frames": 5000, "unit": "frames"})
        self.assertGreater(max(gdb.timeouts), 5.0)
        self.assertEqual(gdb.get_timeout(), 5.0)


if __name__ == "__main__":
    unittest.main()
