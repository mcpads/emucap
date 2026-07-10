#!/usr/bin/env python3
"""Verify that Mesen2 cancels an in-flight request and handshakes a replacement MCP session.

Usage: live-reconnect-test.py <bootable.sfc|smc|nes|gba> [mesen-binary]
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "adapters/mesen2/launch.sh"
DEFAULT_BINARY = Path("/Applications/Mesen.app/Contents/MacOS/Mesen")
FREEZE_HOLD_SECONDS = 31
ENTRY_BY_SUFFIX = {
    ".sfc": "emucap-snes.lua",
    ".smc": "emucap-snes.lua",
    ".nes": "emucap-nes.lua",
    ".gba": "emucap-gba.lua",
}


class Session:
    def __init__(self, socket_: socket.socket):
        self.socket = socket_
        self.socket.settimeout(30)
        self.file = socket_.makefile("rwb", buffering=0)
        self.next_id = 0

    def request(self, method: str, params: dict | None = None) -> dict:
        request_id = self.next_id
        self.next_id += 1
        request = {
            "v": 1,
            "id": request_id,
            "method": method,
            "params": params or {},
        }
        self.file.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        line = self.file.readline()
        if not line:
            raise RuntimeError(f"connection closed while waiting for {method}")
        response = json.loads(line)
        if response.get("id") != request_id:
            raise RuntimeError(
                f"stale response preceded {method} handshake: expected id={request_id}, got {response}"
            )
        return response

    def send_without_reading(self, method: str, params: dict) -> int:
        request_id = self.next_id
        self.next_id += 1
        request = {"v": 1, "id": request_id, "method": method, "params": params}
        self.file.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        return request_id

    def reset(self) -> None:
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        self.file.close()
        self.socket.close()

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


def accept(listener: socket.socket, token: str) -> Session:
    socket_, _ = listener.accept()
    session = Session(socket_)
    hello = session.request("hello", {"session_token": token})
    if not hello.get("ok"):
        raise RuntimeError(f"hello failed: {hello}")
    result = hello.get("result", {})
    if result.get("session_token") != token or result.get("adapter") != "mesen2-live":
        raise RuntimeError(f"identity mismatch: {result}")
    return session


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    args = parser.parse_args()
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else DEFAULT_BINARY
    if not content.is_file() or not binary.is_file():
        raise SystemExit(f"missing content/binary: content={content} binary={binary}")
    entry_name = ENTRY_BY_SUFFIX.get(content.suffix.lower())
    if entry_name is None:
        raise SystemExit(f"unsupported content suffix for this live test: {content.suffix}")
    entry = ROOT / "adapters/mesen2" / entry_name

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(4)
    listener.settimeout(90)
    port = listener.getsockname()[1]
    token = "mesen-live-reconnect-token"
    owned_pid: int | None = None
    session: Session | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-reconnect-") as temp:
        home = Path(temp)
        env = os.environ.copy()
        env.pop("EMUCAP_DEADMAN_MS", None)
        env.pop("EMUCAP_RECONNECT_GIVEUP_MS", None)
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "MESEN_BIN": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_MESEN_LUA": str(entry),
                "EMUCAP_LAUNCH_WAIT": "45",
                "EMUCAP_POST_CONNECT_GRACE": "0",
                "EMUCAP_LOG": str(home / "mesen-live-reconnect.log"),
            }
        )
        launched = subprocess.run(
            [str(LAUNCHER), str(content), str(port), "mesen-live-reconnect"],
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
            session = accept(listener, token)
            abandoned_id = session.send_without_reading("run_frames", {"n": 18000})
            time.sleep(0.25)
            session.reset()
            session = None

            replacement = accept(listener, token)
            session = replacement
            status = session.request("status")
            if not status.get("ok") or status.get("result", {}).get("state") not in {
                "running",
                "frozen",
            }:
                raise RuntimeError(f"replacement session is not usable: {status}")
            freeze_policy = status["result"].get("freeze_policy", {})
            if freeze_policy.get("idle_auto_resume_ms") != 0 or freeze_policy.get(
                "disconnect_auto_resume_ms"
            ) != 0:
                raise RuntimeError(f"default freeze persistence is not indefinite: {freeze_policy}")
            paused = session.request("pause")
            if not paused.get("ok") or paused.get("result", {}).get("state") != "frozen":
                raise RuntimeError(f"pause did not enter frozen: {paused}")
            time.sleep(FREEZE_HOLD_SECONDS)
            held = session.request("status")
            if not held.get("ok") or held.get("result", {}).get("state") != "frozen":
                raise RuntimeError(
                    f"pause auto-resumed during {FREEZE_HOLD_SECONDS}s idle hold: {held}"
                )
            resumed = session.request("resume")
            if not resumed.get("ok") or resumed.get("result", {}).get("state") != "running":
                raise RuntimeError(f"explicit resume failed after persistent freeze: {resumed}")
            try:
                os.kill(owned_pid, 0)
            except ProcessLookupError as error:
                raise RuntimeError("Mesen exited instead of reconnecting") from error
            print(
                json.dumps(
                    {
                        "ok": True,
                        "pid": owned_pid,
                        "port": port,
                        "abandoned_request_id": abandoned_id,
                        "same_process": True,
                        "replacement_hello": True,
                        "status_after_reconnect": status["result"]["state"],
                        "freeze_policy": freeze_policy,
                        "pause_held_seconds": FREEZE_HOLD_SECONDS,
                    },
                    separators=(",", ":"),
                )
            )
        finally:
            if session is not None:
                session.close()
            if owned_pid is not None:
                terminate_owned(owned_pid)
            listener.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
