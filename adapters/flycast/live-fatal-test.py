#!/usr/bin/env python3
"""Live fatal contract test for the native Flycast adapter.

The test owns one loopback listener and one launcher PID. It never broad-kills Flycast processes.
Synthetic usage: live-fatal-test.py <bootable.gdi|cdi|chd> [flycast-binary]
Real usage: live-fatal-test.py --real [--expect-epc N] [--expect-event N] <content> [binary]
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
LAUNCHER = ROOT / "adapters/flycast/launch.sh"
DEFAULT_BINARY = (
    Path.home()
    / "Library/Application Support/emucap/flycast-build/work/build/Flycast.app/Contents/MacOS/Flycast"
)


class Session:
    def __init__(self, socket_: socket.socket):
        self.socket = socket_
        self.socket.settimeout(30)
        self.file = socket_.makefile("rwb", buffering=0)
        self.next_id = 1

    def close(self) -> None:
        try:
            self.file.close()
        finally:
            self.socket.close()

    def call(self, method: str, params: dict | None = None) -> dict:
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
            raise RuntimeError(f"response id mismatch for {method}: {response}")
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


def accept_session(listener: socket.socket, token: str, launch_id: str) -> tuple[Session, dict]:
    socket_, _ = listener.accept()
    session = Session(socket_)
    hello = session.call("hello", {"session_token": token})
    if not hello.get("ok"):
        raise RuntimeError(f"hello failed: {hello}")
    result = hello["result"]
    if result.get("session_token") != token or result.get("launch_id") != launch_id:
        raise RuntimeError(f"identity mismatch: {result}")
    return session, result


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


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def wait_owned_exit(pid: int, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not pid_alive(pid):
            return True
        time.sleep(0.1)
    return not pid_alive(pid)


def launch_owned(env: dict[str, str], content: Path, port: int, pidfile: Path) -> int:
    launched = subprocess.run(
        [str(LAUNCHER), str(content), str(port)],
        env=env,
        text=True,
        capture_output=True,
        timeout=45,
        check=False,
    )
    if launched.returncode != 0:
        raise RuntimeError(f"launch failed:\n{launched.stdout}\n{launched.stderr}")
    return int(pidfile.read_text().strip())


def assert_snapshot_matches_live_state(artifact: dict, state: dict) -> None:
    registers = artifact["registers"]
    pairs = [(f"cpu.r{index}", f"r{index}") for index in range(16)]
    pairs.extend(
        (f"cpu.{name}", name)
        for name in (
            "pc",
            "pr",
            "sr",
            "gbr",
            "vbr",
            "ssr",
            "spc",
            "sgr",
            "dbr",
            "mach",
            "macl",
            "fpul",
        )
    )
    mismatches = {
        live_name: (state.get(live_name), registers.get(snapshot_name))
        for live_name, snapshot_name in pairs
        if state.get(live_name) != registers.get(snapshot_name)
    }
    if mismatches:
        raise RuntimeError(f"failure snapshot differs from quarantined state: {mismatches}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real", action="store_true", help="wait for a genuine SH4 fatal")
    parser.add_argument("--expect-epc", type=lambda value: int(value, 0))
    parser.add_argument("--expect-event", type=lambda value: int(value, 0))
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    args = parser.parse_args()
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else DEFAULT_BINARY
    if not content.is_file() or not binary.is_file():
        raise SystemExit(f"missing content/binary: content={content} binary={binary}")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(4)
    listener.settimeout(120)
    port = listener.getsockname()[1]
    token = "live-fatal-reclaim-token"
    launch_id = "launch-live-fatal-test"
    owned_pid: int | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-flycast-fatal-") as temp:
        home = Path(temp)
        generation = home / "sessions" / str(port) / "generations" / launch_id
        generation.mkdir(parents=True, mode=0o700)
        failure = generation / "adapter-failure.json"
        log = home / "flycast-live-fatal.log"
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "FLYCAST_APP": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_LAUNCH_ID": launch_id,
                "EMUCAP_FAILURE_FILE": str(failure),
                "EMUCAP_FAILURE_HOLD_MS": "120000" if args.real else "30000",
                "EMUCAP_LOG": str(log),
            }
        )
        if args.real:
            env.pop("EMUCAP_ENABLE_TEST_FATAL", None)
        else:
            env["EMUCAP_ENABLE_TEST_FATAL"] = "1"
        pidfile = home / "flycast" / str(port) / "flycast.pid"
        owned_pid = launch_owned(env, content, port, pidfile)

        session: Session | None = None
        try:
            session, hello = accept_session(listener, token, launch_id)
            methods = set(hello.get("methods", []))
            if "dismiss_failure" not in methods:
                raise RuntimeError(f"dismiss_failure is not advertised: {methods}")
            if args.real and "test_fatal" in methods:
                raise RuntimeError("production hello unexpectedly advertises test_fatal")
            if not args.real and "test_fatal" not in methods:
                raise RuntimeError(f"test_fatal not advertised under test gate: {methods}")

            # Ensure a rendered frame exists before freezing so screenshot is meaningful in quarantine.
            screenshot_ok = False
            for _ in range(60):
                response = session.call("screenshot")
                if response.get("ok") and response.get("result", {}).get("png_base64"):
                    screenshot_ok = True
                    break
                time.sleep(0.25)
            if not screenshot_ok:
                raise RuntimeError("Flycast never produced a screenshot before fatal capture")

            abandoned_request_recovered = False
            if not args.real:
                session.send_without_reading("run_frames", {"n": 18000})
                time.sleep(0.25)
                session.reset()
                session = None
                session, _ = accept_session(listener, token, launch_id)
                recovered = session.call("status")
                if recovered.get("result", {}).get("state") not in {"running", "frozen"}:
                    raise RuntimeError(f"in-flight disconnect did not recover: {recovered}")
                abandoned_request_recovered = True

            if args.real:
                deadline = time.monotonic() + 90
                while True:
                    status = session.call("status")
                    if status.get("result", {}).get("state") == "crashed":
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError("no genuine SH4 fatal observed within 90 seconds")
                    time.sleep(0.25)
            else:
                scheduled = session.call("test_fatal")
                if not scheduled.get("ok") or not scheduled["result"].get("scheduled"):
                    raise RuntimeError(f"synthetic fatal was not scheduled: {scheduled}")
                status = session.call("status")
                if status.get("result", {}).get("state") != "crashed":
                    raise RuntimeError(f"quarantine status is not crashed: {status}")
            state = session.call("get_state")
            if not state.get("ok") or len(state["result"]["state"]) < 16:
                raise RuntimeError(f"get_state unavailable in quarantine: {state}")
            shot = session.call("screenshot")
            if not shot.get("ok") or not shot["result"].get("png_base64"):
                raise RuntimeError(f"screenshot unavailable in quarantine: {shot}")
            mutation = session.call("resume")
            if mutation.get("ok") or mutation.get("error", {}).get("kind") != "crashed":
                raise RuntimeError(f"mutation was not rejected in quarantine: {mutation}")

            deadline = time.monotonic() + 5
            while not failure.is_file() and time.monotonic() < deadline:
                time.sleep(0.05)
            artifact = json.loads(failure.read_text())
            registers = artifact.get("registers", {})
            if artifact.get("launch_id") != launch_id:
                raise RuntimeError(f"failure launch_id mismatch: {artifact.get('launch_id')}")
            if artifact.get("content") != str(content):
                raise RuntimeError(f"failure content mismatch: {artifact.get('content')}")
            required_context = {
                "frame",
                "epc",
                "incoming_event",
                "existing_expevt",
                "existing_intevt",
                "tea",
                "emulator_build",
            }
            if not required_context.issubset(artifact):
                raise RuntimeError(
                    f"failure artifact missing context: {required_context - set(artifact)}"
                )
            if artifact.get("epc") != status["result"].get("epc"):
                raise RuntimeError("status and failure artifact disagree on EPC")
            if artifact.get("incoming_event") != status["result"].get("incoming_event"):
                raise RuntimeError("status and failure artifact disagree on incoming event")
            if args.real and artifact.get("incoming_event") == 0xFFFFFFFF:
                raise RuntimeError("real fatal produced the synthetic sentinel event")
            if args.expect_epc is not None and artifact.get("epc") != args.expect_epc:
                raise RuntimeError(
                    f"unexpected EPC: expected {args.expect_epc:#x}, got {artifact.get('epc'):#x}"
                )
            if args.expect_event is not None and artifact.get("incoming_event") != args.expect_event:
                raise RuntimeError(
                    "unexpected event: "
                    f"expected {args.expect_event:#x}, got {artifact.get('incoming_event'):#x}"
                )
            if any(f"r{index}" not in registers for index in range(16)):
                raise RuntimeError("failure artifact does not contain R0-R15")
            assert_snapshot_matches_live_state(artifact, state["result"]["state"])
            if not 1 <= len(artifact.get("pc_ring", [])) <= 512:
                raise RuntimeError("failure artifact PC ring is empty or oversized")
            if failure.stat().st_size > 128 * 1024:
                raise RuntimeError("failure artifact exceeds 128 KiB")

            # Drop only the MCP-side socket. The fatal point and backend must survive and reconnect.
            session.close()
            session = None
            session, _ = accept_session(listener, token, launch_id)
            reattached = session.call("status")
            if reattached.get("result", {}).get("state") != "crashed":
                raise RuntimeError(f"reattached session lost fatal state: {reattached}")
            dismissed = session.call("dismiss_failure")
            if not dismissed.get("ok") or not dismissed["result"].get("dismissed"):
                raise RuntimeError(f"dismiss failed: {dismissed}")
            if dismissed["result"].get("process_will_exit") != args.real:
                raise RuntimeError(f"dismiss exit contract mismatch: {dismissed}")
            exited_after_dismiss = False
            relaunched_same_port = False
            fatal_pid = owned_pid
            if args.real:
                exited_after_dismiss = wait_owned_exit(owned_pid, 10)
                if not exited_after_dismiss:
                    raise RuntimeError("real fatal process did not exit after dismiss_failure")
                session.close()
                session = None

                relaunch_id = "launch-live-fatal-relaunch"
                relaunch_generation = home / "sessions" / str(port) / "generations" / relaunch_id
                relaunch_generation.mkdir(parents=True, mode=0o700)
                env["EMUCAP_LAUNCH_ID"] = relaunch_id
                env["EMUCAP_FAILURE_FILE"] = str(relaunch_generation / "adapter-failure.json")
                env["EMUCAP_LOG"] = str(home / "flycast-live-fatal-relaunch.log")
                owned_pid = launch_owned(env, content, port, pidfile)
                session, relaunch_hello = accept_session(listener, token, relaunch_id)
                if "test_fatal" in set(relaunch_hello.get("methods", [])):
                    raise RuntimeError("production relaunch unexpectedly advertises test_fatal")
                relaunch_status = session.call("status")
                if relaunch_status.get("result", {}).get("state") not in {"running", "frozen"}:
                    raise RuntimeError(f"same-port relaunch is not live: {relaunch_status}")
                relaunched_same_port = True

            print(
                json.dumps(
                    {
                        "ok": True,
                        "pid": fatal_pid,
                        "launch_id": launch_id,
                        "failure_bytes": failure.stat().st_size,
                        "pc_ring_count": len(artifact["pc_ring"]),
                        "reconnected": True,
                        "mutation_guarded": True,
                        "screenshot_in_quarantine": True,
                        "mode": "real" if args.real else "synthetic",
                        "epc": artifact["epc"],
                        "incoming_event": artifact["incoming_event"],
                        "snapshot_matches_live_state": True,
                        "abandoned_request_recovered": abandoned_request_recovered,
                        "exited_after_dismiss": exited_after_dismiss,
                        "relaunched_same_port": relaunched_same_port,
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
