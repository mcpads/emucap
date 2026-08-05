from __future__ import annotations

import json
import os
from pathlib import Path
import platform
import queue
import signal
import socket
import struct
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[3]
LAUNCHER = ROOT / "adapters/mesen2/launch.sh"


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


class RecordingSink:
    def __init__(self, capture_id: str, fail_after_auth: bool = False):
        self.capture_id = capture_id
        self.fail_after_auth = fail_after_auth
        self.token = "mesen-recording-sink-token"
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.listener.settimeout(30)
        self.endpoint = f"127.0.0.1:{self.listener.getsockname()[1]}"
        self.events: list[dict] = []
        self.error: Exception | None = None
        self.thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self.thread.start()

    def _run(self) -> None:
        try:
            connection, _ = self.listener.accept()
            with connection:
                file = connection.makefile("rb")
                auth = json.loads(file.readline())
                if auth != {"capture_id": self.capture_id, "token": self.token}:
                    raise RuntimeError(f"recording sink authentication differs: {auth}")
                if self.fail_after_auth:
                    connection.setsockopt(
                        socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0)
                    )
                    return
                for line in file:
                    self.events.append(json.loads(line))
        except Exception as error:
            self.error = error
        finally:
            self.listener.close()

    def finish(self) -> list[dict]:
        self.thread.join(timeout=35)
        if self.thread.is_alive():
            raise RuntimeError("recording sink did not close")
        if self.error is not None:
            raise self.error
        return self.events


class McpProcess:
    def __init__(self, binary: Path, env: dict[str, str]):
        self.process = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        self.responses: queue.Queue[str] = queue.Queue()
        self.reader = threading.Thread(target=self._read_stdout, daemon=True)
        self.reader.start()
        self.next_id = 1

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self.responses.put(line)

    def request(self, method: str, params: dict, timeout: float = 90) -> dict:
        request_id = self.next_id
        self.next_id += 1
        wire = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(wire, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                response = json.loads(
                    self.responses.get(timeout=max(0.001, deadline - time.monotonic()))
                )
            except queue.Empty as error:
                raise RuntimeError(f"MCP response timed out for {method}") from error
            if response.get("id") == request_id:
                return response
        raise RuntimeError(f"MCP response timed out for {method}")

    def initialize(self) -> None:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "mesen-live-test", "version": "1"},
            },
        )
        if "error" in response:
            raise RuntimeError(f"MCP initialize failed: {response}")
        assert self.process.stdin is not None
        self.process.stdin.write(
            '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}\n'
        )
        self.process.stdin.flush()

    def tool(self, name: str, arguments: dict, timeout: float = 90) -> dict:
        response = self.request(
            "tools/call", {"name": name, "arguments": arguments}, timeout=timeout
        )
        result = response.get("result", {})
        if "error" in response or result.get("isError"):
            raise RuntimeError(f"MCP {name} failed: {response}")
        structured = result.get("structuredContent")
        if not isinstance(structured, dict):
            raise RuntimeError(f"MCP {name} returned no structured content: {response}")
        return structured

    def close(self) -> None:
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.reader.join(timeout=2)
        if self.process.returncode not in {0, None}:
            assert self.process.stderr is not None
            error = self.process.stderr.read()
            raise RuntimeError(
                f"MCP process exited with {self.process.returncode}: {error}"
            )
