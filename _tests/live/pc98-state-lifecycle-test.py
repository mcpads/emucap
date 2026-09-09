#!/usr/bin/env python3
"""Verify PC-98 state lifecycle using a generated blank floppy and caller-owned BIOS.

No game image is required. Artifacts contain machine state and must remain private.
The managed generation created here is always stopped by its exact launch ID.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys
import zipfile

# Reuse the existing generic stdio MCP driver, with no Mesen host dependency.
sys.path.insert(0, str(Path(__file__).resolve().parent / "mesen2"))
from support import McpProcess, ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rompath", type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    disk = out / "blank.hdm"
    disk.write_bytes(bytes(1261568))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = os.environ.copy()
    env.update(EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(out / "home"),
               EMUCAP_PORT=str(port), MAME_ROMPATH=str(args.rompath.resolve(strict=True)))
    mcp = McpProcess(ROOT / "target/release/emucap-mcp", env)
    records = []
    launch_id = None

    def call(name, params=None):
        response = mcp.request("tools/call", {"name": name, "arguments": params or {}})
        records.append({"tool": name, "params": params, "response": response})
        (out / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        assert not result.get("isError") and "error" not in response, response
        return result["structuredContent"]

    def read_byte():
        return call("read_memory", {"memory_type": "ram", "address": 0x9000, "length": 1})["hex"]

    try:
        mcp.initialize()
        bootstrap = call("bootstrap")
        plan = call("launch_plan", {"content_path": str(disk), "system": "pc98"})
        assert plan["ready_to_launch"], plan
        launched = call("launch", {**plan["preferred_launcher"]["args"],
                                  "name": "pc98-state-lifecycle-test", "display": False})
        launch_id = launched["launch_id"]
        call("pause")
        status = call("status")
        assert status["connected"] and status["state"] == "frozen", status
        (out / "identity.json").write_text(json.dumps({"bootstrap": bootstrap, "launch": launched,
            "host_sha256": hashlib.sha256((ROOT / "adapters/mame-pc98/work/mame.raw").read_bytes()).hexdigest()}, indent=2))
        original = read_byte()
        full = out / "full.zip"
        saved = call("save_state", {"path": str(full)})
        assert saved["save_items"]["items"] > 0, saved
        call("write_memory", {"memory_type": "ram", "address": 0x9000, "hex": "a5" if original != "a5" else "5a"})
        loaded = call("load_state", {"path": str(full)})
        assert loaded["postload"]["status"] == "completed", loaded
        assert read_byte() == original
        partial = out / "memory-only.zip"
        with zipfile.ZipFile(full) as source, zipfile.ZipFile(partial, "w", zipfile.ZIP_DEFLATED) as dest:
            manifest = json.loads(source.read("state.json"))
            device_prefix = manifest.pop("save_items")["dir"].rstrip("/") + "/"
            for member in source.namelist():
                if member == "state.json":
                    dest.writestr(member, json.dumps(manifest))
                elif not member.startswith(device_prefix):
                    dest.writestr(member, source.read(member))
        loaded = call("load_state", {"path": str(partial)})
        assert loaded["postload"]["status"] == "skipped", loaded
        assert read_byte() == original
        debug = call("debug", {"operation": "describe"})
        assert "probe" in debug["operations"], debug
        for path, expected in [(full, "completed"), (partial, "skipped")]:
            probed = call("debug", {"operation": "probe", "known_capability_revision": debug["capability_revision"],
                                    "arguments": {"state": str(path), "frame": 0, "memory_type": "ram", "address": 0x9000, "length": 1}})
            assert probed["hex"] == original, probed
            # Core may compose probe from load/step/read; the terminal result still must be frozen.
            assert call("status")["state"] == "frozen"
            if "postload" in probed:
                assert probed["postload"]["status"] == expected
        call("screenshot", {"save_path": str(out / "restored.png")})
        (out / "passed.json").write_text(json.dumps({"passed": True, "items": saved["save_items"]}, indent=2))
        print("PC-98 managed state lifecycle checks passed", flush=True)
    finally:
        if launch_id:
            assert call("stop", {"launch_id": launch_id})["stopped"]
        mcp.close()


if __name__ == "__main__":
    main()
