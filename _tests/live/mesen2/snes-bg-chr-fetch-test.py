#!/usr/bin/env python3
"""Record a dense, bounded SNES BG character-data fetch stream.

Usage: snes-bg-chr-fetch-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

from support import McpProcess, ROOT, default_binary


EVENT_CLASS = "snes_ppu_bg_chr_fetch"


def dot_from_hclock(hclock: int) -> int:
    if hclock <= 1292:
        return hclock >> 2
    if hclock <= 1310:
        return (hclock - 2) >> 2
    return (hclock - 4) >> 2


def require_capability(status: dict) -> str:
    capability = status.get("recording_capability", {})
    advertised = {
        item.get("id"): item for item in capability.get("event_classes", [])
    }
    event = advertised.get(EVENT_CLASS, {})
    fields = {
        (item.get("kind"), item.get("path"))
        for item in event.get("filterable_fields", [])
    }
    required = {
        ("u64_range", "address"),
        ("u64_range", "layer"),
        ("u64_range", "scanline"),
    }
    if (
        capability.get("event_order") != "guest_emission"
        or not event.get("exact")
        or not event.get("stoppable")
        or not required <= fields
    ):
        raise RuntimeError(f"BG CHR fetch recording is not advertised: {capability}")
    return capability["revision"]


def validate_capture(bundle: Path) -> dict:
    manifest = json.loads((bundle / "manifest.json").read_text())
    events = [
        json.loads(line)
        for line in (bundle / "events/segment-000.ndjson").read_text().splitlines()
    ]
    terminal = manifest.get("terminal", {})
    bg_events = [event for event in events if event.get("class") == EVENT_CLASS]
    if (
        terminal.get("operation_outcome") != "completed"
        or terminal.get("execution_outcome") != "target_reached"
        or terminal.get("integrity") != "complete"
        or terminal.get("publication") != "published"
        or terminal.get("final_execution_state") != "frozen"
    ):
        raise RuntimeError(f"dense BG capture terminal differs: {terminal}")
    if len(bg_events) <= 256:
        raise RuntimeError(f"dense BG capture did not cross the live-queue scale: {len(bg_events)}")
    if [event.get("sequence") for event in events] != list(range(len(events))):
        raise RuntimeError("dense BG capture sequence is not contiguous")
    if not all(
        event.get("clock", {}).get("domain") == "snes_master"
        and 0 <= event.get("payload", {}).get("address", -1) <= 0x7FFF
        and 0 <= event.get("payload", {}).get("value", -1) <= 0xFFFF
        and 0 <= event.get("payload", {}).get("layer", -1) <= 3
        for event in bg_events
    ):
        raise RuntimeError("dense BG capture escaped the registered payload contract")
    ticks = [event["clock"]["tick"] for event in bg_events]
    if ticks != sorted(ticks):
        raise RuntimeError("dense BG capture clock regressed")
    scanlines = {event["payload"]["scanline"] for event in bg_events}
    dots = {event["payload"]["dot"] for event in bg_events}
    hclocks = {event["payload"]["hclock"] for event in bg_events}
    if len(scanlines) <= 100 or not all(
        event["payload"]["dot"] == dot_from_hclock(event["payload"]["hclock"])
        for event in bg_events
    ):
        raise RuntimeError(
            "dense BG capture used stale PPU coordinates: "
            f"scanlines={len(scanlines)} dots={len(dots)} hclocks={len(hclocks)}"
        )
    facts = {
        item.get("id"): item for item in terminal.get("event_classes", [])
    }
    bg_fact = facts.get(EVENT_CLASS, {})
    if (
        bg_fact.get("observed") != len(bg_events)
        or bg_fact.get("dropped") != 0
        or not bg_fact.get("armed")
        or manifest.get("counters", {}).get("dropped") != 0
        or manifest.get("loss", {}).get("truncated")
        or manifest.get("cleanup", {}).get("hooks") != "released"
        or manifest.get("cleanup", {}).get("sink") != "released"
    ):
        raise RuntimeError(f"dense BG capture accounting differs: {manifest}")
    return {
        "events": len(events),
        "bg_events": len(bg_events),
        "scanlines": len(scanlines),
        "dots": len(dots),
        "hclocks": len(hclocks),
        "first": bg_events[0],
        "last": bg_events[-1],
        "scope": manifest.get("scope"),
    }


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

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-bg-fetch-") as temp:
        home = Path(temp)
        output_root = home / "bundles"
        output_root.mkdir()
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(port),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mesen-bg-chr-fetch-test",
                "MESEN_BIN": str(binary),
            }
        )
        mcp = McpProcess(mcp_binary, env)
        launch_id: str | None = None
        try:
            mcp.initialize()
            bootstrap = mcp.tool("bootstrap", {})
            source_revision = subprocess.check_output(
                ["git", "rev-parse", "--short=7", "HEAD"], cwd=ROOT, text=True
            ).strip()
            if bootstrap.get("server_build") != source_revision:
                raise RuntimeError(
                    "release MCP does not identify the current committed source: "
                    f"server={bootstrap.get('server_build')} source={source_revision}"
                )
            launched = mcp.tool(
                "launch",
                {
                    "content_path": str(content),
                    "system": "snes",
                    "name": "mesen-bg-chr-fetch-test",
                    "display": False,
                    "start_frozen": True,
                },
                timeout=60,
            )
            launch_id = launched["launch_id"]
            status = mcp.tool("status", {})
            capability_revision = require_capability(status)
            started = time.monotonic()
            captured = mcp.tool(
                "debug",
                {
                    "operation": "record_window",
                    "known_capability_revision": status["capability_revision"],
                    "arguments": {
                        "output_root": str(output_root),
                        "origin": "reset_release",
                        "frames": 2,
                        "event_classes": [
                            "frame_boundary",
                            "frame_completed",
                            EVENT_CLASS,
                        ],
                        "event_filters": [
                            {
                                "event_class": EVENT_CLASS,
                                "terms": [
                                    {
                                        "kind": "u64_range",
                                        "path": "address",
                                        "start": 0,
                                        "length": 32768,
                                    },
                                    {
                                        "kind": "u64_range",
                                        "path": "layer",
                                        "start": 0,
                                        "length": 4,
                                    },
                                ],
                            }
                        ],
                    },
                },
                timeout=120,
            )
            elapsed_ms = round((time.monotonic() - started) * 1000)
            evidence = validate_capture(Path(captured["bundle_path"]))
            final_status = mcp.tool("status", {})
            if final_status.get("state") != "frozen" or not final_status.get("connected"):
                raise RuntimeError(f"connection did not survive dense BG capture: {final_status}")
            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "host_build": final_status.get("host_build"),
                        "capability_revision": capability_revision,
                        "manifest_sha256": captured.get("manifest_sha256"),
                        "elapsed_ms": elapsed_ms,
                        "evidence": evidence,
                    },
                    separators=(",", ":"),
                )
            )
            return 0
        finally:
            if launch_id is not None:
                try:
                    mcp.tool("stop", {"launch_id": launch_id}, timeout=20)
                except Exception:
                    pass
            mcp.close()


if __name__ == "__main__":
    raise SystemExit(main())
