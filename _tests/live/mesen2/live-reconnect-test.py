#!/usr/bin/env python3
"""Verify native-halt zero drift and same-process reconnect with the compatible Mesen host.

Usage: live-reconnect-test.py <bootable.sfc|smc|nes|gb|gbc|gg|gba> [mesen-binary]
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
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
LAUNCHER = ROOT / "adapters/mesen2/launch.sh"
FREEZE_HOLD_SECONDS = 31
ENTRY_BY_SUFFIX = {
    ".sfc": "emucap-snes.lua",
    ".smc": "emucap-snes.lua",
    ".nes": "emucap-nes.lua",
    ".gb": "emucap-gb.lua",
    ".gbc": "emucap-gb.lua",
    ".gg": "emucap-sms.lua",
    ".gba": "emucap-gba.lua",
}


def default_binary() -> Path:
    import platform

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
    features = set(result.get("host_features", []))
    if result.get("mesen_host_api") != 2 or not {
        "code_break_idle",
        "native_halt_service",
        "native_halt_savestate",
    }.issubset(features):
        raise RuntimeError(f"mesen-patch-required: runtime hello lacks native halt: {result}")
    return session


def freeze_signature(session: Session) -> dict:
    status = session.request("status")
    state = session.request("get_state", {"groups": ["cpu"]})
    if not status.get("ok") or not state.get("ok"):
        raise RuntimeError(f"freeze observation failed: status={status} state={state}")
    if status.get("result", {}).get("state") != "frozen":
        raise RuntimeError(f"freeze observation ran outside native halt: {status}")
    return {
        "state": "frozen",
        "frame": status["result"].get("frame"),
        "cpu": state["result"].get("state", {}),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--hold-seconds", type=float, default=FREEZE_HOLD_SECONDS)
    parser.add_argument("--reset-repeats", type=int, default=1)
    args = parser.parse_args()
    if args.reset_repeats < 1:
        raise SystemExit("--reset-repeats must be at least 1")
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
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
    listener.settimeout(float(os.environ.get("EMUCAP_TEST_ACCEPT_TIMEOUT", "90")))
    port = listener.getsockname()[1]
    token = "mesen-live-reconnect-token"
    owned_pid: int | None = None
    session: Session | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-reconnect-") as temp:
        home = Path(temp)
        wrapper = home / "idle-error-once.lua"
        load_error_path = home / "adapter-load-error.txt"
        lifecycle_path = home / "adapter-lifecycle.txt"
        wrapper.write_text(
            """local entry = assert(os.getenv("EMUCAP_IDLE_PROBE_ENTRY"))
local loaded, load_error = pcall(dofile, entry)
if not loaded then
  local file = io.open(assert(os.getenv("EMUCAP_IDLE_PROBE_ERROR")), "wb")
  if file then file:write(tostring(load_error)); file:close() end
  error(load_error)
end
local lifecycle = io.open(assert(os.getenv("EMUCAP_IDLE_PROBE_LIFECYCLE")), "ab")
if lifecycle then lifecycle:write("loaded\\n"); lifecycle:close() end
local first_frame = true
emu.addEventCallback(function()
  if first_frame then
    first_frame = false
    local frame_file = io.open(assert(os.getenv("EMUCAP_IDLE_PROBE_LIFECYCLE")), "ab")
    if frame_file then frame_file:write("frame\\n"); frame_file:close() end
  end
end, emu.eventType.startFrame)
local fail_once = true
emu.addEventCallback(function()
  if fail_once then
    fail_once = false
    error("emucap intentional one-shot codeBreakIdle test error")
  end
end, emu.eventType.codeBreakIdle)
emu.addEventCallback(function()
  if fail_once then
    fail_once = false
    error("emucap intentional one-shot codeBreakIdleSavestate test error")
  end
end, emu.eventType.codeBreakIdleSavestate)
""",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env.pop("EMUCAP_DEADMAN_MS", None)
        env.pop("EMUCAP_RECONNECT_GIVEUP_MS", None)
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "MESEN_BIN": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_MESEN_LUA": str(wrapper),
                "EMUCAP_IDLE_PROBE_ENTRY": str(entry),
                "EMUCAP_IDLE_PROBE_ERROR": str(load_error_path),
                "EMUCAP_IDLE_PROBE_LIFECYCLE": str(lifecycle_path),
                "EMUCAP_LAUNCH_WAIT": os.environ.get("EMUCAP_TEST_LAUNCH_WAIT", "45"),
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
            load_error = (
                load_error_path.read_text(encoding="utf-8", errors="replace")
                if load_error_path.is_file()
                else "<no adapter load error artifact>"
            )
            raise RuntimeError(
                f"launch failed:\n{launched.stdout}\n{launched.stderr}"
                f"\nadapter load error: {load_error}"
            )
        pidfile = home / "mesen2" / str(port) / "mesen.pid"
        owned_pid = int(pidfile.read_text().strip())

        try:
            session = accept(listener, token)
            status = session.request("status")
            freeze_policy = status.get("result", {}).get("freeze_policy", {})
            if freeze_policy.get("mode") != "native_halt_service" or freeze_policy.get(
                "instruction_drift"
            ) != 0:
                raise RuntimeError(f"native halt policy not active: {freeze_policy}")
            if freeze_policy.get("idle_auto_resume_ms") != 0 or freeze_policy.get(
                "disconnect_auto_resume_ms"
            ) != 0:
                raise RuntimeError(f"default freeze persistence is not indefinite: {freeze_policy}")

            after_reset = {}
            for reset_index in range(args.reset_repeats):
                reset = session.request("reset")
                if not reset.get("ok") or reset.get("result", {}).get("reconnect") is not True:
                    raise RuntimeError(
                        f"reset {reset_index + 1} was not acknowledged before reconnect: {reset}"
                    )
                session.close()
                session = accept(listener, token)
                after_reset = session.request("status")
                if (
                    not after_reset.get("ok")
                    or after_reset.get("result", {}).get("state") != "running"
                ):
                    raise RuntimeError(
                        f"post-reset session {reset_index + 1} is not usable: {after_reset}"
                    )

            paused = session.request("pause")
            if not paused.get("ok") or paused.get("result", {}).get("state") != "frozen":
                raise RuntimeError(f"pause did not enter frozen: {paused}")
            baseline = freeze_signature(session)

            # Drop the MCP transport while native halt owns execution. The same process must
            # reconnect and expose exactly the same guest-time signature.
            session.reset()
            session = None
            session = accept(listener, token)
            after_reconnect = freeze_signature(session)
            if after_reconnect != baseline:
                raise RuntimeError(
                    f"guest drifted across reconnect: before={baseline} after={after_reconnect}"
                )

            # A request burst lasting over the old 1s watchdog must not kill the Lua service.
            burst_start = time.monotonic()
            burst_count = 0
            while time.monotonic() - burst_start < 1.25:
                burst = session.request("status")
                if not burst.get("ok") or burst.get("result", {}).get("state") != "frozen":
                    raise RuntimeError(f"request burst lost native halt service: {burst}")
                burst_count += 1

            time.sleep(args.hold_seconds)
            held = freeze_signature(session)
            if held != baseline:
                raise RuntimeError(
                    f"guest drifted during {args.hold_seconds}s native halt: before={baseline} after={held}"
                )
            screenshot = session.request("screenshot")
            if not screenshot.get("ok") or not screenshot.get("result", {}).get("png_base64"):
                raise RuntimeError(f"screenshot failed while frozen: {screenshot}")

            state_path = home / "safe-halt.mss"
            old_state = b"previous-state-must-not-be-truncated"
            state_path.write_bytes(old_state)
            saved = session.request("save_state", {"path": str(state_path)})
            if not saved.get("ok"):
                raise RuntimeError(f"safe frozen save failed: {saved}")
            if state_path.read_bytes() == old_state:
                raise RuntimeError("safe frozen save did not replace the previous state file")
            after_save = freeze_signature(session)
            if after_save != baseline:
                raise RuntimeError(
                    f"safe frozen save changed guest state: before={baseline} after={after_save}"
                )
            safe_status = session.request("status")
            if (
                safe_status.get("result", {})
                .get("freeze_policy", {})
                .get("savestate_safe")
                is not True
            ):
                raise RuntimeError(
                    f"pause halt did not advertise its savestate-safe boundary: {safe_status}"
                )

            stepped_frame = session.request("step", {"frames": 1, "unit": "frames"})
            if not stepped_frame.get("ok") or stepped_frame.get("result", {}).get(
                "status"
            ) != "completed":
                raise RuntimeError(f"one-frame step failed: {stepped_frame}")
            after_frame_step = freeze_signature(session)
            if after_frame_step["frame"] != baseline["frame"] + 1:
                raise RuntimeError(
                    f"frame step was not exact: before={baseline} after={after_frame_step}"
                )
            unsafe_state_path = home / "unsafe-halt.mss"
            unsafe_save = session.request("save_state", {"path": str(unsafe_state_path)})
            if (
                unsafe_save.get("ok")
                or unsafe_save.get("error", {}).get("kind") != "unsafe_halt"
                or unsafe_state_path.exists()
            ):
                raise RuntimeError(
                    f"frame-step halt did not fail-loud before serialization: {unsafe_save}"
                )

            stepped_instruction = session.request(
                "step", {"frames": 1, "unit": "instructions"}
            )
            if not stepped_instruction.get("ok") or stepped_instruction.get("result", {}).get(
                "status"
            ) != "completed":
                raise RuntimeError(f"one-instruction step failed: {stepped_instruction}")
            after_instruction_step = freeze_signature(session)
            # One CPU instruction may cross a frame boundary, and its visible register projection
            # may be unchanged (e.g. a self-branch or a console HALT/interrupt boundary). Mesen's
            # native StepRequest owns the exact instruction count; this live gate verifies that its
            # completed reply returns to the compatible native halt instead of free-running.
            loaded = session.request("load_state", {"path": str(state_path)})
            if (
                not loaded.get("ok")
                or loaded.get("result", {}).get("state") != "frozen"
            ):
                raise RuntimeError(f"safe frozen load failed or lost halt ownership: {loaded}")
            after_load = freeze_signature(session)
            if after_load["cpu"] != baseline["cpu"]:
                raise RuntimeError(
                    f"safe frozen load did not restore CPU projection: before={baseline} "
                    f"after={after_load}"
                )

            resumed = session.request("resume")
            if not resumed.get("ok") or resumed.get("result", {}).get("state") != "running":
                raise RuntimeError(f"explicit resume failed after persistent freeze: {resumed}")

            frozen = session.request("pause")
            if not frozen.get("ok") or frozen.get("result", {}).get("state") != "frozen":
                raise RuntimeError(f"second pause did not enter frozen: {frozen}")
            frozen_reset = session.request("reset")
            if not frozen_reset.get("ok") or frozen_reset.get("result", {}).get(
                "reconnect"
            ) is not True:
                raise RuntimeError(f"frozen reset was not acknowledged: {frozen_reset}")
            session.close()
            session = accept(listener, token)
            after_frozen_reset = session.request("status")
            if not after_frozen_reset.get("ok") or after_frozen_reset.get("result", {}).get(
                "state"
            ) != "running":
                raise RuntimeError(f"frozen reset did not return running: {after_frozen_reset}")

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
                        "same_process": True,
                        "replacement_hello": True,
                        "reset_ack_before_reconnect": True,
                        "running_reset_repeats": args.reset_repeats,
                        "post_reset_status": after_reset["result"]["state"],
                        "frozen_reset_ack_before_reconnect": True,
                        "post_frozen_reset_status": after_frozen_reset["result"]["state"],
                        "zero_drift_across_reconnect": True,
                        "zero_drift_during_hold": True,
                        "freeze_policy": freeze_policy,
                        "pause_held_seconds": args.hold_seconds,
                        "burst_requests": burst_count,
                        "one_frame_step_exact": True,
                        "one_instruction_step_refroze": True,
                        "safe_frozen_save_preserved_halt": True,
                        "unsafe_frame_halt_rejected_save": True,
                        "safe_frozen_load_restored_cpu": True,
                        "recovered_after_one_shot_idle_error": True,
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
            log_path = home / "mesen-live-reconnect.log"
            log_tail = (
                "\n".join(log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-80:])
                if log_path.is_file()
                else "<missing Mesen log>"
            )
            lifecycle = (
                lifecycle_path.read_text(encoding="utf-8", errors="replace")
                if lifecycle_path.is_file()
                else "<missing lifecycle artifact>"
            )
            raise RuntimeError(
                f"{error}\nowned_pid={owned_pid} alive={alive}"
                f"\nadapter lifecycle:\n{lifecycle}\nMesen log tail:\n{log_tail}"
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
