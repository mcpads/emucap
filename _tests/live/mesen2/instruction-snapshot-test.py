#!/usr/bin/env python3
"""Issue and reverify a standalone instruction receipt; no recording, load, or consumer attempt.

Supply owned SNES content. Output contains private receipts, states and exact runtime evidence.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess

from support import McpProcess, ROOT, default_binary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=default_binary())
    parser.add_argument("--mcp-binary", type=Path, default=ROOT / "target/release/emucap-mcp")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    content = args.content.resolve(strict=True)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT), "proof requires clean source"
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env = os.environ.copy()
    env.update(EMUCAP_EMU_HOME=str(output / "home"), EMUCAP_PORT=str(port),
               EMUCAP_REPO_ROOT=str(ROOT), MESEN_BIN=str(args.binary.resolve(strict=True)))
    mcp = McpProcess(args.mcp_binary.resolve(strict=True), env)
    launch_id = None
    records = []

    def call(name, arguments=None, error=None):
        response = mcp.request("tools/call", {"name": name, "arguments": arguments or {}})
        records.append({"tool": name, "arguments": arguments or {}, "response": response})
        (output / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        if error:
            assert result.get("isError") and error in json.dumps(result), response
            return result
        assert "error" not in response and not result.get("isError"), response
        return result["structuredContent"]

    def launch():
        plan = call("launch_plan", {"content_path": str(content), "system": "snes"})
        assert plan["ready_to_launch"], plan
        result = call("launch", {**plan["preferred_launcher"]["args"],
                                 "name": "instruction-snapshot-test", "start_frozen": True, "display": False})
        return result["launch_id"]

    def cpu():
        return call("get_state", {"groups": ["cpu"]})

    try:
        mcp.initialize()
        boot = call("bootstrap")
        assert boot["server_build"] == revision[:7], boot
        launch_id = launch()
        status = call("status")
        assert status["connected"] and status["state"] == "frozen", status
        assert status["server_build"] == revision[:7], status
        assert status["snapshot_capability"]["snapshot_key_required"], status
        assert "state_load" not in status["recording_capability"]["origins"], status
        call("step", {"count": 180, "unit": "frames"})
        protected = output / "protected.mss"
        protected.write_bytes(b"untouched")
        call("save_state", {"path": str(protected), "snapshot_key": "unsafe"}, error="unsafe_halt")
        assert protected.read_bytes() == b"untouched"
        call("step", {"count": 1, "unit": "instructions"})
        before = cpu()
        dest = output / "instruction.mss"
        saved = call("save_state", {"path": str(dest), "snapshot_key": "instruction"})
        assert saved["status"] == "completed" and saved["receipt_issued"], saved
        assert cpu() == before, "save advanced the guest"
        receipt = saved["snapshot_receipt"]
        body = receipt["body"]
        assert body["source"]["server_build"] == revision[:7]
        assert body["source"]["adapter_build"] == revision[:7], body
        assert body["source"]["launch_id"] == launch_id
        assert body["source"]["content"]["sha256"] == hashlib.sha256(content.read_bytes()).hexdigest()
        assert body["snapshot"]["sha256"] == hashlib.sha256(dest.read_bytes()).hexdigest()
        canonical = json.dumps(body, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
        assert receipt["sha256"] == hashlib.sha256(b"emucap-instruction-snapshot\n" + canonical).hexdigest()
        assert json.loads(Path(saved["receipt_path"]).read_text()) == receipt
        assert Path(saved["snapshot_path"]).read_bytes() == dest.read_bytes()
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2, ensure_ascii=False))
        call("screenshot", {"save_path": str(output / "saved.png")})
        replay = call("save_state", {"path": str(dest), "snapshot_key": "instruction"})
        assert replay["status"] == "verified" and replay["snapshot_receipt"] == receipt
        assert cpu() == before
        # A later halt must not be recaptured by an already used key.
        call("step", {"count": 1, "unit": "instructions"})
        replay = call("save_state", {"path": str(dest), "snapshot_key": "instruction"})
        assert replay["snapshot_receipt"] == receipt
        call("save_state", {"path": str(output / "other.mss"), "snapshot_key": "instruction"}, error="conflicts")
        failed_export = output / "directory"
        failed_export.mkdir()
        failed = call("save_state", {"path": str(failed_export), "snapshot_key": "export-failure"}, error="receipt_issued")
        assert failed["structuredContent"]["receipt_issued"]
        assert call("snapshot_receipt", {"snapshot_key": "export-failure"})["status"] == "verified"
        call("stop", {"launch_id": launch_id})
        saved_launch = launch_id
        launch_id = None
        verified = call("snapshot_receipt", {"snapshot_key": "instruction", "path": str(dest),
                                             "expected_launch_id": saved_launch})
        assert verified["status"] == "verified" and verified["snapshot_receipt"] == receipt
        mcp.close()
        mcp = McpProcess(args.mcp_binary.resolve(strict=True), env)
        mcp.initialize()
        assert call("snapshot_receipt", {"snapshot_key": "instruction"})["snapshot_receipt"] == receipt
        launch_id = launch()
        assert launch_id != saved_launch
        assert call("snapshot_receipt", {"snapshot_key": "instruction"})["status"] == "verified"
        call("snapshot_receipt", {"snapshot_key": "instruction", "expected_launch_id": launch_id}, error="launch mismatch")
        tampered = output / "tampered.mss"
        tampered.write_bytes(dest.read_bytes()[:-1])
        call("snapshot_receipt", {"snapshot_key": "instruction", "path": str(tampered)}, error="integrity mismatch")
        call("stop", {"launch_id": launch_id})
        launch_id = None
        (output / "passed.json").write_text(json.dumps({"passed": True, "producer_revision": revision,
            "receipt_sha256": receipt["sha256"], "saved_launch_id": saved_launch,
            "consumer_authentication": "not performed", "consumer_attempt": "not reserved"}, indent=2))
        print("standalone instruction receipt issued and reverified after shutdown, restart and generation replacement", flush=True)
    finally:
        if launch_id:
            call("stop", {"launch_id": launch_id})
        mcp.close()


if __name__ == "__main__":
    main()
