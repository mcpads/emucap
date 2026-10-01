#!/usr/bin/env python3
"""Validate Dolphin's long native requests and disconnect cleanup using caller-owned media.

The host is a child of this test (not a detached managed launch). Private output retains wire
responses and emulator logs; the test terminates only its own child process.
Use --sound for native output cleanup and --deadline for the production timeout boundary.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--system", choices=("gamecube", "wii"), default="wii")
    parser.add_argument("--sound", action="store_true")
    parser.add_argument("--checkpoint", type=Path)
    parser.add_argument("--reset", action="store_true",
                        help="exercise native reset progress, breakpoint and disconnect cleanup")
    parser.add_argument("--shutdown", action="store_true",
                        help="require graceful native shutdown during an active frame advance")
    parser.add_argument("--frozen-shutdown", action="store_true",
                        help="with --reset --shutdown, terminate at a frozen one-percent boundary")
    parser.add_argument("--deadline", action="store_true",
                        help="exercise the real native operation deadline at 1 percent")
    args = parser.parse_args()
    if args.frozen_shutdown and not (args.reset and args.shutdown):
        parser.error("--frozen-shutdown requires --reset --shutdown")
    if args.reset and args.deadline:
        parser.error("--deadline applies to frame advances; reset deadline needs a fault-injection producer")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(30)
    env = os.environ.copy()
    env.update(EMUCAP_PORT=str(listener.getsockname()[1]), EMUCAP_SYSTEM=args.system,
               EMUCAP_DOLPHIN_SOUND="1" if args.sound else "0",
               EMUCAP_CONTENT=str(args.content.resolve(strict=True)))

    def digest(path):
        h = hashlib.sha256()
        with Path(path).open("rb") as f:
            while chunk := f.read(1048576):
                h.update(chunk)
        return h.hexdigest()

    artifacts = [args.binary.resolve(strict=True), args.content.resolve(strict=True), Path(__file__).resolve()]
    if args.checkpoint:
        artifacts.append(args.checkpoint.resolve(strict=True))
    hashes = {str(p): digest(p) for p in artifacts}
    (out / "producer.json").write_text(json.dumps({"revision": subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "sha256": hashes}, indent=2))
    log = (out / "emulator.log").open("w")
    child = subprocess.Popen([str(args.binary.resolve(strict=True)), "--user", str(out / "user"),
        "--exec", str(args.content.resolve(strict=True)), "--platform=headless",
        "--config=Dolphin.Interface.ConfirmStop=False", "--config=Dolphin.Interface.UsePanicHandlers=False",
        "--config=Dolphin.Analytics.Enabled=False", "--config=Dolphin.Analytics.PermissionAsked=True",
        "--config=Wiimote.Wiimote1.Source=1"],
        env=env, stdout=log, stderr=subprocess.STDOUT)
    connection = None
    stream = None
    records = []
    request_id = 0

    def attach():
        nonlocal connection, stream
        connection, _ = listener.accept()
        connection.settimeout(10)
        stream = connection.makefile("rwb", buffering=0)

    def send(method, params=None):
        nonlocal request_id
        request_id += 1
        stream.write((json.dumps({"v": 1, "id": request_id, "method": method, "params": params or {}}) + "\n").encode())
        return request_id

    def read(expected):
        raw = stream.readline()
        assert raw, "host disconnected"
        response = json.loads(raw)
        records.append(response)
        (out / "responses.json").write_text(json.dumps(records, indent=2))
        assert response["id"] == expected, response
        return response

    def frozen_output():
        state = call("status")
        assert state["state"] == "frozen", state
        audio = state["audio_output"]
        assert audio["initialized"] and audio["phase"] == "ready", audio
        assert audio["last_run_result"] == "stopped" and audio["failure"] is None, audio
        assert (audio["backend"] != "No Audio Output") == args.sound, audio
        assert audio["start_verified"], audio
        return state

    def call(method, params=None):
        expected = send(method, params)
        while True:
            response = read(expected)
            assert response["ok"], response
            result = response["result"]
            if result.get("status") != "working":
                return result

    try:
        attach()
        hello = call("hello")
        assert hello["execution_limits"]["max_sync_advance_count"] == 5000, hello
        call("pause")
        if args.checkpoint:
            call("load_state", {"path": str(args.checkpoint.resolve(strict=True))})
        if args.reset:
            for percent in (1, 10000):
                call("execution_speed", {"mode": "limited", "percent": percent})
                first_record = len(records)
                result = call("reset")
                assert result["status"] == "completed" and result["completion_boundary"] == "button_release", result
                state = frozen_output()
                assert state["execution_speed"]["percent"] == percent, state
                if percent == 1:
                    assert any(r.get("result", {}).get("status") == "working" for r in records[first_record:])
            pc = call("get_state")["state"]["cpu.pc"]
            bp = call("set_breakpoint", {"kind": "exec", "memory_type": "main", "start": pc, "end": pc, "pause_on_hit": True})
            assert call("reset")["status"] == "interrupted"
            frozen_output()
            assert call("poll_events")["events"]
            call("clear_breakpoint", {"id": bp["id"]})
            call("execution_speed", {"mode": "unlimited"})
            assert call("step", {"count": 120})["count"] == 120
            frozen_output()
            call("execution_speed", {"mode": "limited", "percent": 1})
            expected = send("reset")
            for _ in range(2):
                assert read(expected)["result"]["status"] == "working"
            stream.close()
            connection.shutdown(socket.SHUT_RDWR)
            connection.close()
            attach()
            state = frozen_output()
            assert state["execution_speed"]["percent"] == 1, state
            call("execution_speed", {"mode": "unlimited"})
            assert call("step", {"count": 120})["count"] == 120
            frozen_output()
            if args.shutdown:
                call("execution_speed", {"mode": "limited", "percent": 1})
                if args.frozen_shutdown:
                    frozen_output()
                else:
                    expected = send("reset")
                    for _ in range(2):
                        assert read(expected)["result"]["status"] == "working"
                child.terminate()
                child.wait(timeout=15)
                assert child.returncode == 0, child.returncode
            print("Native reset completion, breakpoint and disconnect passed", flush=True)
            if args.shutdown:
                print("Native graceful shutdown passed", flush=True)
        else:
            call("execution_speed", {"mode": "unlimited"})
            for count, method in [(1, "step"), (16, "step_instructions"), (5000, "step_instructions"), (16, "step"), (180, "step"), (5000, "step")]:
                result = call(method, {"count": count})
                assert result["status"] == "completed" and result["count"] == count and result["state"] == "frozen", result
                frozen_output()
                print(method, count, "completed", flush=True)
            assert any(r.get("result", {}).get("status") == "working" for r in records)
            pc = call("get_state")["state"]["cpu.pc"]
            bp = call("set_breakpoint", {"kind": "exec", "memory_type": "main", "start": pc, "end": pc, "pause_on_hit": True})
            result = call("step", {"count": 5000})
            assert result["status"] == "interrupted" and result["count"] < 5000, result
            frozen_output()
            assert call("poll_events")["events"]
            call("clear_breakpoint", {"id": bp["id"]})
            assert call("step", {"count": 16})["count"] == 16
            frozen_output()
            call("execution_speed", {"mode": "limited", "percent": 100})
            before_disconnect = frozen_output()["frame"]
            started = time.monotonic()
            expected = send("step", {"count": 5000})
            while True:
                response = read(expected)
                assert response["result"]["status"] == "working", response
                if time.monotonic() - started >= 2:
                    break
            stream.close()
            connection.shutdown(socket.SHUT_RDWR)
            connection.close()
            attach()
            # A replacement connection is served only after the interrupted worker has cleaned up.
            after_disconnect = frozen_output()
            assert 0 < after_disconnect["frame"] - before_disconnect < 5000, after_disconnect
            call("execution_speed", {"mode": "unlimited"})
            assert call("step", {"count": 16})["count"] == 16
            frozen_output()
            if args.deadline:
                call("execution_speed", {"mode": "limited", "percent": 1})
                result = call("step", {"count": 5000})
                assert result["status"] == "interrupted" and result["reason"] == "host_deadline", result
                assert 0 < result["count"] < 5000, result
                frozen_output()
                call("execution_speed", {"mode": "unlimited"})
                assert call("step", {"count": 16})["count"] == 16
                frozen_output()
                print("Native deadline cleanup and continued control passed", flush=True)
            if args.shutdown:
                call("execution_speed", {"mode": "limited", "percent": 100})
                started = time.monotonic()
                expected = send("step", {"count": 5000})
                while True:
                    response = read(expected)
                    assert response["result"]["status"] == "working", response
                    if time.monotonic() - started >= 2:
                        break
                child.terminate()
                child.wait(timeout=15)
                assert child.returncode == 0, child.returncode
                print("Graceful native shutdown during active advance passed", flush=True)
        assert all(digest(p) == value for p, value in hashes.items()), "artifact changed"
        (out / "passed.json").write_text(json.dumps({"passed": True, "pid": child.pid}, indent=2))
        print("Dolphin requested operation checks passed", flush=True)
    finally:
        if stream:
            stream.close()
        if connection:
            connection.close()
        listener.close()
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        log.close()
        (out / "cleanup.json").write_text(json.dumps({"pid": child.pid, "exit": child.returncode}))


if __name__ == "__main__":
    main()
