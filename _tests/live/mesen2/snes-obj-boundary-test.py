#!/usr/bin/env python3
"""Verify negotiated SNES OBJ boundary events against a compatible Mesen host.

Usage: snes-obj-boundary-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
LAUNCHER = ROOT / "adapters/mesen2/launch.sh"
EVENT_KINDS = {"snes_obj_eval_start", "snes_obj_handoff"}
SNAPSHOT_LENGTHS = {"snesSpriteRam": 0x220, "snesWorkRam": 16}


def default_binary() -> Path:
    machine = platform.machine().lower()
    if sys.platform == "darwin":
        rid = "osx-arm64" if machine in {"arm64", "aarch64"} else "osx-x64"
        return ROOT / (
            f"adapters/mesen2/work/mesen/bin/{rid}/Release/{rid}/publish/"
            "Mesen.app/Contents/MacOS/Mesen"
        )
    if sys.platform.startswith("linux"):
        rid = "linux-arm64" if machine in {"arm64", "aarch64"} else "linux-x64"
        return ROOT / f"adapters/mesen2/work/mesen/bin/{rid}/Release/{rid}/publish/Mesen"
    return ROOT / "adapters/mesen2/work/mesen/bin/win-x64/Release/Mesen.exe"


class Session:
    def __init__(self, socket_: socket.socket):
        self.socket = socket_
        self.socket.settimeout(30)
        self.file = socket_.makefile("rwb", buffering=0)
        self.next_id = 0

    def request(self, method: str, params: dict | None = None) -> dict:
        request_id = self.next_id
        self.next_id += 1
        request = {"v": 1, "id": request_id, "method": method, "params": params or {}}
        self.file.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        while True:
            line = self.file.readline()
            if not line:
                raise RuntimeError(f"connection closed while waiting for {method}")
            response = json.loads(line)
            if response.get("id") != request_id:
                continue
            if response.get("result", {}).get("status") == "working":
                continue
            return response

    def close(self) -> None:
        try:
            self.file.close()
        finally:
            self.socket.close()


def terminate_owned(pid: int) -> None:
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    for _ in range(30):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.1)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def require_ok(response: dict, operation: str) -> dict:
    if not response.get("ok"):
        raise RuntimeError(f"{operation} failed: {response}")
    return response.get("result", {})


def validate_event(event: dict, expected_kind: str, scanline: int) -> dict:
    if (
        event.get("type") != "device_event"
        or event.get("device") != "snes_ppu_obj"
        or event.get("kind") != expected_kind
    ):
        raise RuntimeError(f"unexpected device event: {event}")
    ppu = event.get("ppu", {})
    for field in ("frame", "scanline", "dot", "hclock", "master_clock"):
        if not isinstance(ppu.get(field), int):
            raise RuntimeError(f"missing integer ppu.{field}: {event}")
    if ppu["scanline"] != scanline:
        raise RuntimeError(f"scanline filter leaked an event: {event}")

    snapshots = event.get("snapshot")
    if not isinstance(snapshots, list):
        raise RuntimeError(f"snapshot missing from event: {event}")
    by_type = {item.get("memory_type"): item for item in snapshots}
    if set(by_type) != set(SNAPSHOT_LENGTHS):
        raise RuntimeError(f"snapshot memory types differ: {snapshots}")
    hashes = {}
    for memory_type, byte_length in SNAPSHOT_LENGTHS.items():
        item = by_type[memory_type]
        raw = bytes.fromhex(item.get("hex", ""))
        if item.get("address") != 0 or len(raw) != byte_length:
            raise RuntimeError(f"snapshot length/address differs: {item}")
        hashes[memory_type] = hashlib.sha256(raw).hexdigest()
    return {"ppu": ppu, "snapshot_sha256": hashes}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--scanline", type=int, default=32)
    parser.add_argument("--event-timeout", type=float, default=10)
    args = parser.parse_args()

    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    if content.suffix.lower() not in {".sfc", ".smc"}:
        raise SystemExit(f"SNES content required: {content}")
    if not content.is_file() or not binary.is_file():
        raise SystemExit(f"missing content/binary: content={content} binary={binary}")
    if args.scanline < 0 or args.scanline > 0xFFFF:
        raise SystemExit("--scanline must be in 0..65535")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(4)
    listener.settimeout(float(os.environ.get("EMUCAP_TEST_ACCEPT_TIMEOUT", "90")))
    port = listener.getsockname()[1]
    token = "mesen-snes-obj-boundary-token"
    owned_pid: int | None = None
    session: Session | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-snes-obj-") as temp:
        home = Path(temp)
        log_path = home / "mesen-snes-obj-boundary.log"
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "MESEN_BIN": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_LAUNCH_WAIT": os.environ.get("EMUCAP_TEST_LAUNCH_WAIT", "45"),
                "EMUCAP_POST_CONNECT_GRACE": "0",
                "EMUCAP_LOG": str(log_path),
            }
        )
        launched = subprocess.run(
            [str(LAUNCHER), str(content), str(port), "mesen-snes-obj-boundary", "snes"],
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
            check=False,
        )
        if launched.returncode != 0:
            raise RuntimeError(f"launch failed:\n{launched.stdout}\n{launched.stderr}")
        pidfile = home / "mesen2" / str(port) / "mesen.pid"
        owned_pid = int(pidfile.read_text().strip())

        try:
            socket_, _ = listener.accept()
            session = Session(socket_)
            hello = require_ok(session.request("hello", {"session_token": token}), "hello")
            if (
                hello.get("adapter") != "mesen2-live"
                or hello.get("system") != "snes"
                or hello.get("mesen_host_api") != 2
                or hello.get("session_token") != token
            ):
                raise RuntimeError(f"runtime identity differs: {hello}")
            if "snes_ppu_obj_events" not in hello.get("host_features", []):
                raise RuntimeError(f"native OBJ event feature was not negotiated: {hello}")
            advertised = {
                entry.get("kind"): entry for entry in hello.get("breakpoint_kinds", [])
            }
            if not EVENT_KINDS.issubset(advertised):
                raise RuntimeError(f"OBJ breakpoint kinds were not advertised: {advertised}")
            for kind in EVENT_KINDS:
                expected = {
                    "kind": kind,
                    "range_unit": "ppu_scanline",
                    "range_mode": "inclusive",
                    "memory_type_used": False,
                    "snapshot": True,
                }
                if advertised[kind] != expected:
                    raise RuntimeError(f"breakpoint metadata differs: {advertised[kind]}")

            invalid = session.request(
                "set_breakpoint",
                {
                    "kind": "snes_obj_eval_start",
                    "start": args.scanline,
                    "end": args.scanline,
                    "pause_on_hit": False,
                    "snapshot": ["snesSpriteRam:0x21f:2"],
                },
            )
            if invalid.get("ok") or invalid.get("error", {}).get("kind") != "bad_params":
                raise RuntimeError(f"cross-boundary snapshot did not fail loud: {invalid}")

            ids = {}
            for kind in sorted(EVENT_KINDS):
                result = require_ok(
                    session.request(
                        "set_breakpoint",
                        {
                            "kind": kind,
                            "start": args.scanline,
                            "end": args.scanline,
                            "pause_on_hit": False,
                            "snapshot": [
                                "snesSpriteRam:0:0x220",
                                "snesWorkRam:0:16",
                            ],
                        },
                    ),
                    f"set_breakpoint({kind})",
                )
                ids[kind] = result["id"]

            by_frame: dict[int, dict[str, dict]] = {}
            deadline = time.monotonic() + args.event_timeout
            pair: dict[str, dict] | None = None
            while time.monotonic() < deadline and pair is None:
                polled = require_ok(session.request("poll_events"), "poll_events")
                for event in polled.get("events", []):
                    kind = event.get("kind")
                    if kind not in EVENT_KINDS or event.get("breakpoint_id") != ids[kind]:
                        continue
                    frame = event.get("ppu", {}).get("frame")
                    if isinstance(frame, int):
                        by_frame.setdefault(frame, {})[kind] = event
                        if EVENT_KINDS.issubset(by_frame[frame]):
                            pair = by_frame[frame]
                            break
                if pair is None:
                    time.sleep(0.02)
            if pair is None:
                raise RuntimeError(f"no same-frame OBJ event pair before timeout: {by_frame}")

            samples = {
                kind: validate_event(pair[kind], kind, args.scanline) for kind in EVENT_KINDS
            }
            start_clock = samples["snes_obj_eval_start"]["ppu"]["master_clock"]
            handoff_clock = samples["snes_obj_handoff"]["ppu"]["master_clock"]
            if start_clock >= handoff_clock:
                raise RuntimeError(f"OBJ boundary ordering differs: {samples}")
            if samples["snes_obj_eval_start"]["ppu"]["hclock"] != 0:
                raise RuntimeError(f"evaluation start was not observed at H=0: {samples}")

            cleared = require_ok(session.request("clear_all_breakpoints"), "clear_all_breakpoints")
            if cleared.get("cleared") != len(EVENT_KINDS):
                raise RuntimeError(f"unexpected clear count: {cleared}")
            listed = require_ok(session.request("list_breakpoints"), "list_breakpoints")
            if listed.get("breakpoints") != []:
                raise RuntimeError(f"breakpoints remained armed: {listed}")

            paused = require_ok(session.request("pause"), "pause")
            if paused.get("state") != "frozen":
                raise RuntimeError(f"pause did not freeze: {paused}")
            state_path = home / "obj-boundary-regression.mss"
            require_ok(session.request("save_state", {"path": str(state_path)}), "save_state")
            if not state_path.is_file() or state_path.stat().st_size == 0:
                raise RuntimeError("save_state produced no state file")
            loaded = require_ok(
                session.request("load_state", {"path": str(state_path)}), "load_state"
            )
            if loaded.get("state") != "frozen":
                raise RuntimeError(f"load_state lost freeze ownership: {loaded}")
            resumed = require_ok(session.request("resume"), "resume")
            if resumed.get("state") != "running":
                raise RuntimeError(f"resume did not run: {resumed}")

            print(
                json.dumps(
                    {
                        "ok": True,
                        "pid": owned_pid,
                        "host_api": hello["mesen_host_api"],
                        "host_build": hello.get("host_build"),
                        "host_feature": "snes_ppu_obj_events",
                        "breakpoint_kinds": sorted(EVENT_KINDS),
                        "scanline": args.scanline,
                        "samples": samples,
                        "cross_boundary_rejected": True,
                        "pause_save_load_resume": True,
                    },
                    separators=(",", ":"),
                )
            )
        except Exception as error:
            alive = False
            if owned_pid is not None:
                try:
                    os.kill(owned_pid, 0)
                    alive = True
                except ProcessLookupError:
                    pass
            log_tail = (
                "\n".join(
                    log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-100:]
                )
                if log_path.is_file()
                else "<missing Mesen log>"
            )
            raise RuntimeError(
                f"{error}\nowned_pid={owned_pid} alive={alive}\nMesen log tail:\n{log_tail}"
            ) from error
        finally:
            if session is not None:
                session.close()
            if owned_pid is not None:
                terminate_owned(owned_pid)
            listener.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
