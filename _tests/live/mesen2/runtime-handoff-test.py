#!/usr/bin/env python3
"""Prove explicit handoff of one returned managed Mesen generation.

Usage: runtime-handoff-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

from support import McpProcess, ROOT, default_binary, terminate_owned


def free_port() -> int:
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        return reservation.getsockname()[1]


def process_env(home: Path, port: int, binary: Path, session_id: str) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "EMUCAP_EMU_HOME": str(home),
            "EMUCAP_PORT": str(port),
            "EMUCAP_REPO_ROOT": str(ROOT),
            "EMUCAP_SESSION_ID": session_id,
            "MESEN_BIN": str(binary),
        }
    )
    return env


def require_current_build(bootstrap: dict) -> str:
    source_revision = subprocess.check_output(
        ["git", "rev-parse", "--short=7", "HEAD"], cwd=ROOT, text=True
    ).strip()
    if bootstrap.get("server_build") != source_revision:
        raise RuntimeError(
            "release MCP does not identify the current committed source: "
            f"server={bootstrap.get('server_build')} source={source_revision}"
        )
    return source_revision


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--mcp-binary")
    args = parser.parse_args()
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    mcp_binary = (
        Path(args.mcp_binary).resolve()
        if args.mcp_binary
        else (ROOT / "target/release/emucap-mcp").resolve()
    )
    if not content.is_file() or not binary.is_file() or not mcp_binary.is_file():
        raise SystemExit("content, compatible Mesen, and release MCP binaries must exist")

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-handoff-") as temp:
        home = Path(temp)
        base_port = free_port()
        origin = McpProcess(
            mcp_binary,
            process_env(home, base_port, binary, "mesen-handoff-origin"),
        )
        recipient: McpProcess | None = None
        launch_id: str | None = None
        emulator_pid: int | None = None
        stopped = False
        try:
            origin.initialize()
            source_revision = require_current_build(origin.tool("bootstrap", {}))
            launched = origin.tool(
                "launch",
                {
                    "content_path": str(content),
                    "system": "snes",
                    "name": "mesen-runtime-handoff-test",
                    "display": False,
                    "start_frozen": True,
                },
                timeout=60,
            )
            launch_id = launched["launch_id"]
            emulator_pid = launched["pid"]
            origin_status = origin.tool("status", {})
            if (
                origin_status.get("state") != "frozen"
                or origin_status.get("continuity", {})
                .get("runtime_binding", {})
                .get("state")
                != "bound"
            ):
                raise RuntimeError(f"origin generation was not bound and frozen: {origin_status}")
            origin_port = origin_status["listening_port"]
            origin.close()

            recipient = McpProcess(
                mcp_binary,
                process_env(home, base_port, binary, "mesen-handoff-recipient"),
            )
            recipient.initialize()
            inventory = recipient.tool("bootstrap", {"include": ["runtimes"]})
            require_current_build(inventory)
            candidates = [
                item
                for item in inventory.get("runtime_reservations", [])
                if item.get("launch_id") == launch_id
            ]
            if len(candidates) != 1 or not candidates[0].get("reattach", {}).get("available"):
                raise RuntimeError(f"returned generation was not explicitly available: {inventory}")
            recipient_port = inventory["listener"]["port"]
            if recipient_port == origin_port:
                raise RuntimeError("recipient silently selected the reserved generation port")

            handed_off = recipient.tool("reattach", {"launch_id": launch_id}, timeout=30)
            if not handed_off.get("reattached"):
                raise RuntimeError(f"explicit generation handoff failed: {handed_off}")
            status = recipient.tool("status", {})
            if (
                status.get("state") != "frozen"
                or status.get("emulator_identity", {}).get("launch_id") != launch_id
                or status.get("continuity", {}).get("runtime_binding", {}).get("state")
                != "bound"
                or status.get("listening_port") != origin_port
            ):
                raise RuntimeError(f"recipient did not bind the exact live generation: {status}")
            state = recipient.tool("get_state", {})
            guest_state = state.get("state")
            if not isinstance(guest_state, dict) or not isinstance(
                guest_state.get("cpu.pc"), int
            ):
                raise RuntimeError(f"handed-off control channel could not inspect guest state: {state}")
            stopped_result = recipient.tool("stop", {"launch_id": launch_id}, timeout=20)
            stopped = stopped_result.get("stopped") is True
            if not stopped:
                raise RuntimeError(f"exact generation cleanup failed: {stopped_result}")
            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": source_revision,
                        "launch_id": launch_id,
                        "origin_port": origin_port,
                        "recipient_initial_port": recipient_port,
                        "reattached_port": status.get("listening_port"),
                        "final_state": status.get("state"),
                    },
                    separators=(",", ":"),
                )
            )
            return 0
        finally:
            if recipient is not None:
                if launch_id is not None and not stopped:
                    try:
                        recipient.tool("stop", {"launch_id": launch_id}, timeout=20)
                        stopped = True
                    except Exception:
                        pass
                recipient.close()
            elif origin.process.poll() is None:
                if launch_id is not None and not stopped:
                    try:
                        origin.tool("stop", {"launch_id": launch_id}, timeout=20)
                        stopped = True
                    except Exception:
                        pass
                origin.close()
            if not stopped and emulator_pid is not None:
                terminate_owned(emulator_pid)


if __name__ == "__main__":
    raise SystemExit(main())
