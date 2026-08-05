#!/usr/bin/env python3
"""Prove SNES manual-DMA and HDMA observation facts with an assembled fixture."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile

from support import McpProcess, ROOT, default_binary


HERE = Path(__file__).resolve().parent
ASSEMBLY = HERE / "snes-dma-observation.asm"
LINK = HERE / "snes-dma-observation.link"
SELECTED_CLASSES = (
    "frame_boundary",
    "snes_cpu_instruction",
    "snes_content_read",
    "snes_transfer_enable",
    "snes_transfer_access",
    "snes_device_port_write",
)


def build_fixture(directory: Path) -> Path:
    assembler = shutil.which("wla-65816")
    linker = shutil.which("wlalink")
    if assembler is None or linker is None:
        raise RuntimeError("wla-65816 and wlalink are required for the live DMA fixture")
    object_path = directory / "snes-dma-observation.o"
    rom_path = directory / "snes-dma-observation.sfc"
    subprocess.run(
        [assembler, "-o", str(object_path), str(ASSEMBLY)],
        cwd=directory,
        check=True,
    )
    subprocess.run(
        [linker, "-S", str(LINK), str(rom_path)],
        cwd=directory,
        check=True,
    )
    if rom_path.stat().st_size != 0x8000:
        raise RuntimeError(f"DMA fixture has unexpected size: {rom_path.stat().st_size}")
    return rom_path


def require_capability(status: dict) -> None:
    capability = status.get("recording_capability", {})
    if capability.get("event_order") != "guest_emission":
        raise RuntimeError(f"guest emission order is absent: {capability}")
    advertised = {item.get("id"): item for item in capability.get("event_classes", [])}
    if not all(event_class in advertised for event_class in SELECTED_CLASSES):
        raise RuntimeError(f"DMA observation classes are absent: {capability}")


def recording_params(output_root: Path) -> dict:
    return {
        "output_root": str(output_root),
        "origin": "reset_release",
        "frames": 2,
        "event_classes": list(SELECTED_CLASSES),
        "limits": {
            "max_events": 100000,
            "max_bytes": 64 * 1024 * 1024,
            "max_host_ms": 30000,
        },
    }


def payloads(events: list[dict], event_class: str) -> list[dict]:
    return [event["payload"] for event in events if event.get("class") == event_class]


def exact_enable_group(enables: list[dict], *, hdma: bool, mask: int) -> list[dict]:
    group = [item for item in enables if item["hdma"] is hdma and item["written_mask"] == mask]
    if sorted(item["channel"] for item in group) != list(range(8)):
        raise RuntimeError(f"enable write did not snapshot every channel: {group}")
    return group


def validate_capture(manifest: dict, events: list[dict]) -> dict:
    terminal = manifest.get("terminal", {})
    if (
        terminal.get("operation_outcome") != "completed"
        or terminal.get("integrity") != "complete"
    ):
        raise RuntimeError(f"DMA recording was not complete: {terminal}")
    scope = manifest.get("scope", {})
    if scope.get("event_order") != "guest_emission":
        raise RuntimeError(f"published bundle lost guest emission order: {scope}")
    if [event.get("sequence") for event in events] != list(range(len(events))):
        raise RuntimeError("DMA recording sequence is not dense guest emission order")
    f_start = scope.get("f_start")
    f_end = scope.get("f_end")
    if not all(
        isinstance(event.get("frame"), int) and f_start <= event["frame"] < f_end
        for event in events
    ):
        raise RuntimeError(f"DMA events escaped terminal frame scope [{f_start},{f_end})")

    requested = [item.get("id") for item in manifest.get("request", {}).get("event_classes", [])]
    if requested != list(SELECTED_CLASSES):
        raise RuntimeError(f"published selected-class identity differs: {requested}")
    facts = {item["id"]: item for item in terminal.get("event_classes", [])}
    for event_class in SELECTED_CLASSES:
        fact = facts.get(event_class, {})
        observed = sum(event.get("class") == event_class for event in events)
        if not fact.get("armed") or fact.get("dropped") != 0 or fact.get("observed") != observed:
            raise RuntimeError(f"terminal class accounting differs for {event_class}: {fact}")

    enables = payloads(events, "snes_transfer_enable")
    manual_group = exact_enable_group(enables, hdma=False, mask=0x01)
    hdma_group = exact_enable_group(enables, hdma=True, mask=0x02)
    if len(enables) != 16:
        raise RuntimeError(f"unexpected extra transfer-enable writes: {enables}")

    manual_descriptor = manual_group[0]
    if (
        manual_descriptor["destination"] != 0x2104
        or manual_descriptor["size"] != 4
        or manual_descriptor["mode"] != 0
        or manual_descriptor["direction_to_bus_a"]
        or manual_descriptor["fixed"]
        or manual_descriptor["decrement"]
    ):
        raise RuntimeError(f"manual DMA descriptor differs: {manual_descriptor}")
    hdma_descriptor = hdma_group[1]
    if (
        hdma_descriptor["destination"] != 0x2104
        or hdma_descriptor["mode"] != 0
        or hdma_descriptor["hdma_indirect"]
    ):
        raise RuntimeError(f"HDMA descriptor differs: {hdma_descriptor}")

    accesses = payloads(events, "snes_transfer_access")
    manual = [item for item in accesses if item["channel"] == 0 and not item["hdma"]]
    hdma = [item for item in accesses if item["channel"] == 1 and item["hdma"]]
    manual_ports = [item["value"] for item in manual if item["bus_address"] == 0x2104]
    hdma_ports = [item["value"] for item in hdma if item["bus_address"] == 0x2104]
    if manual_ports != [0x11, 0x22, 0x33, 0x44]:
        raise RuntimeError(f"manual DMA port writes differ: {manual}")
    if hdma_ports != [0x55]:
        raise RuntimeError(f"HDMA port writes differ: {hdma}")

    by_sequence = {event["sequence"]: event for event in events}
    source_accesses = [
        event
        for event in events
        if event.get("class") == "snes_transfer_access"
        and event["payload"]["bus_address"] != 0x2104
    ]
    for access in source_accesses:
        preceding = by_sequence.get(access["sequence"] - 1)
        if (
            preceding is None
            or preceding.get("class") != "snes_content_read"
            or preceding["payload"]["bus_address"] != access["payload"]["bus_address"]
            or preceding["payload"]["value"] != access["payload"]["value"]
        ):
            raise RuntimeError(f"transfer source lacks its serviced-content fact: {access}")

    device_writes = payloads(events, "snes_device_port_write")
    transfer_ports = [item for item in device_writes if item["port"] == 0x2104]
    expected_ports = [(False, 0, value) for value in [0x11, 0x22, 0x33, 0x44]] + [
        (True, 1, 0x55)
    ]
    actual_ports = [(item["hdma"], item["channel"], item["value"]) for item in transfer_ports]
    if actual_ports != expected_ports or not all(item["transfer_active"] for item in transfer_ports):
        raise RuntimeError(f"device writes lost transfer attribution: {transfer_ports}")

    return {
        "events": len(events),
        "enable_records": len(enables),
        "manual_accesses": len(manual),
        "hdma_accesses": len(hdma),
        "manual_values": manual_ports,
        "hdma_values": hdma_ports,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--mcp-binary")
    args = parser.parse_args()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    mcp_binary = (
        Path(args.mcp_binary).resolve()
        if args.mcp_binary
        else (ROOT / "target/release/emucap-mcp").resolve()
    )
    if not binary.is_file():
        raise SystemExit(f"Mesen binary not found: {binary}")
    if not mcp_binary.is_file():
        raise SystemExit(f"release MCP binary not found: {mcp_binary}")

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-dma-") as temp:
        home = Path(temp)
        rom = build_fixture(home)
        output_root = home / "bundles"
        output_root.mkdir()
        port_socket = socket.socket()
        port_socket.bind(("127.0.0.1", 0))
        port = port_socket.getsockname()[1]
        port_socket.close()
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(port),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mesen-dma-observation-test",
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
                    "content_path": str(rom),
                    "system": "snes",
                    "name": "mesen-dma-observation",
                    "display": False,
                },
                timeout=60,
            )
            if not launched.get("launched") or not launched.get("connected"):
                raise RuntimeError(f"Core launch failed: {launched}")
            launch_id = launched["launch_id"]
            status = mcp.tool("status", {})
            require_capability(status)
            if "snes_deep_observation_events" not in status.get("emulator_identity", {}).get(
                "host_features", []
            ):
                raise RuntimeError(f"deep observation host feature is absent: {status}")

            captured = mcp.tool(
                "record_window", recording_params(output_root), timeout=90
            )
            bundle = Path(captured["bundle_path"])
            manifest = json.loads((bundle / "manifest.json").read_text())
            events = [
                json.loads(line)
                for line in (bundle / "events/segment-000.ndjson").read_text().splitlines()
            ]
            evidence = validate_capture(manifest, events)
            status = mcp.tool("status", {})
            if status.get("state") != "frozen" or captured.get("integrity") != "complete":
                raise RuntimeError(f"DMA recording did not retain complete terminal freeze: {status}")
            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "host_build": status.get("host_build"),
                        "recording_revision": status.get("recording_capability", {}).get(
                            "revision"
                        ),
                        "manifest_sha256": captured.get("manifest_sha256"),
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
