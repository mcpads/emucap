#!/usr/bin/env python3
"""Validate Dolphin's long native requests and disconnect cleanup using caller-owned media.

The host is a child of this test (not a detached managed launch). Private output retains wire
responses and emulator logs; the test terminates only its own child process.
"""
import argparse
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
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(30)
    env = os.environ.copy()
    env.update(EMUCAP_PORT=str(listener.getsockname()[1]), EMUCAP_SYSTEM="wii",
               EMUCAP_CONTENT=str(args.content.resolve(strict=True)))
    log = (out / "emulator.log").open("w")
    child = subprocess.Popen([str(args.binary.resolve(strict=True)), "--user", str(out / "user"),
        "--exec", str(args.content.resolve(strict=True)), "--platform=headless",
        "--config=Dolphin.Interface.ConfirmStop=False", "--config=Dolphin.Interface.UsePanicHandlers=False",
        "--config=Dolphin.Analytics.Enabled=False", "--config=Dolphin.Analytics.PermissionAsked=True",
        "--config=Dolphin.DSP.Backend=No Audio Output", "--config=Wiimote.Wiimote1.Source=1"],
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
        for count, method in [(16, "step_instructions"), (5000, "step_instructions"), (16, "step"), (180, "step"), (5000, "step")]:
            result = call(method, {"count": count})
            assert result["status"] == "completed" and result["count"] == count and result["state"] == "frozen", result
            print(method, count, "completed", flush=True)
        assert any(r.get("result", {}).get("status") == "working" for r in records)
        pc = call("get_state")["state"]["cpu.pc"]
        bp = call("set_breakpoint", {"kind": "exec", "memory_type": "main", "start": pc, "end": pc, "pause_on_hit": True})
        result = call("step", {"count": 5000})
        assert result["status"] == "interrupted" and result["count"] < 5000, result
        assert call("poll_events")["events"]
        call("clear_breakpoint", {"id": bp["id"]})
        assert call("step", {"count": 16})["count"] == 16
        expected = send("step", {"count": 5000})
        response = read(expected)
        assert response["result"]["status"] == "working", response
        stream.close()
        connection.shutdown(socket.SHUT_RDWR)
        connection.close()
        attach()
        # A replacement connection is served only after the interrupted worker has cleaned up.
        assert call("status")["state"] == "frozen"
        assert call("step", {"count": 16})["count"] == 16
        (out / "passed.json").write_text(json.dumps({"passed": True, "pid": child.pid}, indent=2))
        print("Dolphin long advances, progress, breakpoint and disconnect cleanup passed", flush=True)
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


if __name__ == "__main__":
    main()
