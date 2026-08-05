#!/usr/bin/env python3
"""Verify negotiated SNES OBJ boundary events against a compatible Mesen host.

Usage: snes-obj-boundary-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from support import (
    LAUNCHER,
    ROOT,
    RecordingSink,
    Session,
    default_binary,
    require_ok,
    terminate_owned,
)

EVENT_KINDS = {"snes_obj_eval_start", "snes_obj_handoff"}
DEEP_EVENT_CLASSES = (
    "snes_cpu_instruction",
    "snes_content_read",
    "snes_transfer_enable",
    "snes_transfer_access",
    "snes_device_port_write",
    "snes_interrupt_delivery",
    "snes_ppu_obj_consumption_read",
)
SNAPSHOT_LENGTHS = {"snesSpriteRam": 0x220, "snesWorkRam": 16}


def semantic_recording_params(hello: dict, sink: RecordingSink) -> dict:
    recording = hello.get("recording", {})
    advertised = {item.get("id"): item for item in recording.get("event_classes", [])}
    selected_ids = [
        "frame_boundary",
        "snes_ppu_obj_evaluation_start",
        "snes_ppu_obj_handoff",
    ]
    if not all(item in advertised for item in selected_ids):
        raise RuntimeError(f"semantic recording classes were not advertised: {recording}")
    return {
        "capture_id": sink.capture_id,
        "launch_id": hello["launch_id"],
        "request_digest_sha256": "ab" * 32,
        "capability_revision": recording["revision"],
        "origin": "next_frame_boundary",
        "frames": 1,
        "event_classes": [
            {
                "id": item,
                "contract_sha256": advertised[item]["contract_sha256"],
            }
            for item in selected_ids
        ],
        "limits": {
            "max_frames": 1,
            "max_events": 1000,
            "max_bytes": 1024 * 1024,
            "max_line_bytes": 4096,
            "max_host_ms": 30000,
            "progress_interval_ms": 250,
        },
        "sink": {"endpoint": sink.endpoint, "token": sink.token},
    }


def deep_recording_params(hello: dict, sink: RecordingSink) -> dict:
    recording = hello.get("recording", {})
    advertised = {item.get("id"): item for item in recording.get("event_classes", [])}
    selected_ids = ["frame_boundary", *DEEP_EVENT_CLASSES]
    if recording.get("event_order") != "guest_emission":
        raise RuntimeError(f"deep recording order was not advertised: {recording}")
    if not all(item in advertised for item in selected_ids):
        raise RuntimeError(f"deep recording classes were not advertised: {recording}")
    return {
        "capture_id": sink.capture_id,
        "launch_id": hello["launch_id"],
        "request_digest_sha256": "cd" * 32,
        "capability_revision": recording["revision"],
        "origin": "reset_release",
        "frames": 1,
        "event_classes": [
            {"id": item, "contract_sha256": advertised[item]["contract_sha256"]}
            for item in selected_ids
        ],
        "limits": {
            "max_frames": 1,
            "max_events": 100000,
            "max_bytes": 64 * 1024 * 1024,
            "max_line_bytes": 4096,
            "max_host_ms": 30000,
            "progress_interval_ms": 250,
        },
        "sink": {"endpoint": sink.endpoint, "token": sink.token},
    }


def validate_deep_capture(result: dict, events: list[dict]) -> dict:
    if result.get("status") != "completed" or result.get("integrity") != "complete":
        raise RuntimeError(f"deep recording was not complete: {result}")
    if [event.get("sequence") for event in events] != list(range(len(events))):
        raise RuntimeError("deep recording sequence is not dense")
    f_start = result.get("f_start")
    f_end = result.get("f_end")
    if not all(
        isinstance(event.get("frame"), int) and f_start <= event["frame"] < f_end
        for event in events
    ):
        raise RuntimeError(f"deep events escaped terminal frame scope [{f_start},{f_end})")
    facts = {item.get("id"): item for item in result.get("event_classes", [])}
    unarmed = [
        event_class
        for event_class in DEEP_EVENT_CLASSES
        if not facts.get(event_class, {}).get("armed")
    ]
    if unarmed:
        raise RuntimeError(f"deep classes were not armed: {unarmed}; {facts}")
    dropped = [
        event_class
        for event_class in DEEP_EVENT_CLASSES
        if facts[event_class].get("dropped", 0) != 0
    ]
    if dropped:
        raise RuntimeError(f"deep classes reported loss: {dropped}; {facts}")
    required_samples = (
        "snes_cpu_instruction",
        "snes_content_read",
        "snes_device_port_write",
        "snes_interrupt_delivery",
        "snes_ppu_obj_consumption_read",
    )
    missing_samples = [
        event_class
        for event_class in required_samples
        if facts[event_class].get("observed", 0) == 0
    ]
    if missing_samples:
        raise RuntimeError(f"expected deep callback facts were absent: {missing_samples}; {facts}")
    return {
        "events": len(events),
        "event_classes": {
            event_class: facts[event_class]["observed"]
            for event_class in DEEP_EVENT_CLASSES
        },
        "first": events[1] if len(events) > 1 else events[0],
    }


def validate_semantic_capture(result: dict, events: list[dict]) -> dict:
    if result.get("status") != "completed" or result.get("integrity") != "complete":
        raise RuntimeError(f"semantic recording was not complete: {result}")
    if len(events) <= 256 or result.get("events") != len(events):
        raise RuntimeError(f"semantic recording did not cross the old queue ceiling: {result}")
    if [event.get("sequence") for event in events] != list(range(len(events))):
        raise RuntimeError("semantic recording sequence is not dense")
    facts = {item.get("id"): item for item in result.get("event_classes", [])}
    for event_class in (
        "snes_ppu_obj_evaluation_start",
        "snes_ppu_obj_handoff",
    ):
        fact = facts.get(event_class, {})
        observed = sum(event.get("class") == event_class for event in events)
        if not fact.get("armed") or fact.get("observed") != observed or fact.get("dropped") != 0:
            raise RuntimeError(f"semantic class accounting differs: {facts}")
        if observed == 0:
            raise RuntimeError(f"semantic class produced no records: {event_class}")
    sample = next(
        event for event in events if event.get("class") == "snes_ppu_obj_handoff"
    )
    if set(sample.get("payload", {})) != {"cpu", "ppu", "forced_blank"}:
        raise RuntimeError(f"semantic payload shape differs: {sample}")
    return {"events": len(events), "event_classes": facts, "sample": sample}


def validate_event(event: dict, expected_kind: str, scanline: int) -> dict:
    if (
        event.get("type") != "device_event"
        or event.get("device") != "snes_ppu_obj"
        or event.get("kind") != expected_kind
    ):
        raise RuntimeError(f"unexpected device event: {event}")
    ppu = event.get("ppu", {})
    for field in ("frame", "scanline", "dot", "hclock", "master_clock"):
        if not isinstance(ppu.get(field), int):
            raise RuntimeError(f"missing integer ppu.{field}: {event}")
    if ppu["scanline"] != scanline:
        raise RuntimeError(f"scanline filter leaked an event: {event}")

    snapshots = event.get("snapshot")
    if not isinstance(snapshots, list):
        raise RuntimeError(f"snapshot missing from event: {event}")
    by_type = {item.get("memory_type"): item for item in snapshots}
    if set(by_type) != set(SNAPSHOT_LENGTHS):
        raise RuntimeError(f"snapshot memory types differ: {snapshots}")
    hashes = {}
    for memory_type, byte_length in SNAPSHOT_LENGTHS.items():
        item = by_type[memory_type]
        raw = bytes.fromhex(item.get("hex", ""))
        if item.get("address") != 0 or len(raw) != byte_length:
            raise RuntimeError(f"snapshot length/address differs: {item}")
        hashes[memory_type] = hashlib.sha256(raw).hexdigest()
    return {"ppu": ppu, "snapshot_sha256": hashes}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--scanline", type=int, default=32)
    parser.add_argument("--event-timeout", type=float, default=30)
    args = parser.parse_args()

    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    if content.suffix.lower() not in {".sfc", ".smc"}:
        raise SystemExit(f"SNES content required: {content}")
    if not content.is_file() or not binary.is_file():
        raise SystemExit(f"missing content/binary: content={content} binary={binary}")
    if args.scanline < 0 or args.scanline > 0xFFFF:
        raise SystemExit("--scanline must be in 0..65535")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(4)
    listener.settimeout(float(os.environ.get("EMUCAP_TEST_ACCEPT_TIMEOUT", "90")))
    port = listener.getsockname()[1]
    token = "mesen-snes-obj-boundary-token"
    owned_pid: int | None = None
    session: Session | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-snes-obj-") as temp:
        home = Path(temp)
        log_path = home / "mesen-snes-obj-boundary.log"
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "MESEN_BIN": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_LAUNCH_ID": "launch-mesen-semantic-runtime-test",
                "EMUCAP_LAUNCH_WAIT": os.environ.get("EMUCAP_TEST_LAUNCH_WAIT", "45"),
                "EMUCAP_POST_CONNECT_GRACE": "0",
                "EMUCAP_LOG": str(log_path),
            }
        )
        launched = subprocess.run(
            [str(LAUNCHER), str(content), str(port), "mesen-snes-obj-boundary", "snes"],
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
            check=False,
        )
        if launched.returncode != 0:
            raise RuntimeError(f"launch failed:\n{launched.stdout}\n{launched.stderr}")
        pidfile = home / "mesen2" / str(port) / "mesen.pid"
        owned_pid = int(pidfile.read_text().strip())

        try:
            socket_, _ = listener.accept()
            session = Session(socket_)
            hello = require_ok(session.request("hello", {"session_token": token}), "hello")
            if (
                hello.get("adapter") != "mesen2-live"
                or hello.get("system") != "snes"
                or hello.get("mesen_host_api") != 2
                or hello.get("session_token") != token
            ):
                raise RuntimeError(f"runtime identity differs: {hello}")
            if "snes_ppu_obj_events" not in hello.get("host_features", []):
                raise RuntimeError(f"native OBJ event feature was not negotiated: {hello}")
            advertised = {
                entry.get("kind"): entry for entry in hello.get("breakpoint_kinds", [])
            }
            if not EVENT_KINDS.issubset(advertised):
                raise RuntimeError(f"OBJ breakpoint kinds were not advertised: {advertised}")
            for kind in EVENT_KINDS:
                expected = {
                    "kind": kind,
                    "range_unit": "ppu_scanline",
                    "range_mode": "inclusive",
                    "memory_type_used": False,
                    "snapshot": True,
                }
                if advertised[kind] != expected:
                    raise RuntimeError(f"breakpoint metadata differs: {advertised[kind]}")

            invalid = session.request(
                "set_breakpoint",
                {
                    "kind": "snes_obj_eval_start",
                    "start": args.scanline,
                    "end": args.scanline,
                    "pause_on_hit": False,
                    "snapshot": ["snesSpriteRam:0x21f:2"],
                },
            )
            if invalid.get("ok") or invalid.get("error", {}).get("kind") != "bad_params":
                raise RuntimeError(f"cross-boundary snapshot did not fail loud: {invalid}")

            ids = {}
            for kind in sorted(EVENT_KINDS):
                result = require_ok(
                    session.request(
                        "set_breakpoint",
                        {
                            "kind": kind,
                            "start": args.scanline,
                            "end": args.scanline,
                            "pause_on_hit": False,
                            "snapshot": [
                                "snesSpriteRam:0:0x220",
                                "snesWorkRam:0:16",
                            ],
                        },
                    ),
                    f"set_breakpoint({kind})",
                )
                ids[kind] = result["id"]

            by_frame: dict[int, dict[str, dict]] = {}
            deadline = time.monotonic() + args.event_timeout
            pair: dict[str, dict] | None = None
            while time.monotonic() < deadline and pair is None:
                polled = require_ok(session.request("poll_events"), "poll_events")
                for event in polled.get("events", []):
                    kind = event.get("kind")
                    if kind not in EVENT_KINDS or event.get("breakpoint_id") != ids[kind]:
                        continue
                    frame = event.get("ppu", {}).get("frame")
                    if isinstance(frame, int):
                        by_frame.setdefault(frame, {})[kind] = event
                        if EVENT_KINDS.issubset(by_frame[frame]):
                            pair = by_frame[frame]
                            break
                if pair is None:
                    time.sleep(0.02)
            if pair is None:
                raise RuntimeError(f"no same-frame OBJ event pair before timeout: {by_frame}")

            samples = {
                kind: validate_event(pair[kind], kind, args.scanline) for kind in EVENT_KINDS
            }
            start_clock = samples["snes_obj_eval_start"]["ppu"]["master_clock"]
            handoff_clock = samples["snes_obj_handoff"]["ppu"]["master_clock"]
            if start_clock >= handoff_clock:
                raise RuntimeError(f"OBJ boundary ordering differs: {samples}")
            if samples["snes_obj_eval_start"]["ppu"]["hclock"] != 0:
                raise RuntimeError(f"evaluation start was not observed at H=0: {samples}")

            cleared = require_ok(session.request("clear_all_breakpoints"), "clear_all_breakpoints")
            if cleared.get("cleared") != len(EVENT_KINDS):
                raise RuntimeError(f"unexpected clear count: {cleared}")
            listed = require_ok(session.request("list_breakpoints"), "list_breakpoints")
            if listed.get("breakpoints") != []:
                raise RuntimeError(f"breakpoints remained armed: {listed}")

            failed_sink = RecordingSink("capture-semantic-failure", fail_after_auth=True)
            failed_sink.start()
            failed_capture = require_ok(
                session.request(
                    "record_window", semantic_recording_params(hello, failed_sink)
                ),
                "record_window(sink failure)",
            )
            failed_sink.finish()
            if (
                failed_capture.get("status") != "failed"
                or failed_capture.get("integrity") == "complete"
                or failed_capture.get("cleanup", {}).get("hooks") != "released"
            ):
                raise RuntimeError(f"sink failure did not fail-stop cleanly: {failed_capture}")
            resumed = require_ok(session.request("resume"), "resume after sink failure")
            if resumed.get("state") != "running":
                raise RuntimeError(f"sink-failure recovery did not resume: {resumed}")

            deep_sink = RecordingSink("capture-deep-clean")
            deep_sink.start()
            deep_capture = require_ok(
                session.request("record_window", deep_recording_params(hello, deep_sink)),
                "record_window(deep classes)",
            )
            deep_sample = validate_deep_capture(deep_capture, deep_sink.finish())
            resumed = require_ok(session.request("resume"), "resume after deep recording")
            if resumed.get("state") != "running":
                raise RuntimeError(f"deep recording recovery did not resume: {resumed}")

            clean_sink = RecordingSink("capture-semantic-clean")
            clean_sink.start()
            clean_capture = require_ok(
                session.request(
                    "record_window", semantic_recording_params(hello, clean_sink)
                ),
                "record_window(clean retry)",
            )
            semantic_sample = validate_semantic_capture(clean_capture, clean_sink.finish())
            session.close()
            session = None
            reconnected_socket, _ = listener.accept()
            session = Session(reconnected_socket)
            reconnected_hello = require_ok(
                session.request("hello", {"session_token": token}), "reconnected hello"
            )
            if reconnected_hello.get("launch_id") != hello.get("launch_id"):
                raise RuntimeError(f"reconnected launch identity differs: {reconnected_hello}")
            status_after_capture = require_ok(session.request("status"), "recording status")
            if (
                status_after_capture.get("state") != "frozen"
                or status_after_capture.get("recording", {}).get("last", {}).get("capture_id")
                != "capture-semantic-clean"
            ):
                raise RuntimeError(
                    f"reconnect/status terminal recording identity differs: {status_after_capture}"
                )

            paused = require_ok(session.request("pause"), "pause")
            if paused.get("state") != "frozen":
                raise RuntimeError(f"pause did not freeze: {paused}")
            state_path = home / "obj-boundary-regression.mss"
            require_ok(session.request("save_state", {"path": str(state_path)}), "save_state")
            if not state_path.is_file() or state_path.stat().st_size == 0:
                raise RuntimeError("save_state produced no state file")
            loaded = require_ok(
                session.request("load_state", {"path": str(state_path)}), "load_state"
            )
            if loaded.get("state") != "frozen":
                raise RuntimeError(f"load_state lost freeze ownership: {loaded}")
            resumed = require_ok(session.request("resume"), "resume")
            if resumed.get("state") != "running":
                raise RuntimeError(f"resume did not run: {resumed}")

            print(
                json.dumps(
                    {
                        "ok": True,
                        "pid": owned_pid,
                        "host_api": hello["mesen_host_api"],
                        "host_build": hello.get("host_build"),
                        "host_feature": "snes_ppu_obj_events",
                        "launch_id": hello.get("launch_id"),
                        "recording_capability_revision": hello.get("recording", {}).get(
                            "revision"
                        ),
                        "breakpoint_kinds": sorted(EVENT_KINDS),
                        "scanline": args.scanline,
                        "samples": samples,
                        "semantic_recording": semantic_sample,
                        "deep_recording": deep_sample,
                        "sink_failure_recovered": True,
                        "cross_boundary_rejected": True,
                        "pause_save_load_resume": True,
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
            log_tail = (
                "\n".join(
                    log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-100:]
                )
                if log_path.is_file()
                else "<missing Mesen log>"
            )
            raise RuntimeError(
                f"{error}\nowned_pid={owned_pid} alive={alive}\nMesen log tail:\n{log_tail}"
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
