#!/usr/bin/env python3
"""Exercise state-backed recording fail-stop and recovery against a real Mesen host.

Usage: snes-state-backed-failure-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

from support import (
    LAUNCHER,
    RecordingSink,
    Session,
    default_binary,
    require_ok,
    terminate_owned,
)


def file_identity(path: Path) -> dict:
    data = path.read_bytes()
    return {
        "path": str(path),
        "format": "frame-full-state-1",
        "port": 0,
        "frames": len(data.splitlines()),
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def recording_params(
    hello: dict,
    sink: RecordingSink,
    state_path: Path,
    state_bytes: int,
    state_frame: int,
    movie_path: Path,
    capture_id: str,
) -> dict:
    capability = hello.get("recording", {})
    advertised = {
        event_class.get("id"): event_class
        for event_class in capability.get("event_classes", [])
    }
    boundary = advertised.get("frame_boundary")
    state_load = capability.get("state_load", {})
    if (
        "state_load" not in capability.get("origins", [])
        or state_load.get("format") != "mesen-savestate"
        or state_load.get("alignment") != "restored_frame_boundary"
        or not boundary
    ):
        raise RuntimeError(f"state-backed recording is not advertised: {capability}")
    state_data = state_path.read_bytes()
    return {
        "capture_id": capture_id,
        "launch_id": hello["launch_id"],
        "request_digest_sha256": hashlib.sha256(capture_id.encode()).hexdigest(),
        "capability_revision": capability["revision"],
        "origin": "state_load",
        "frames": 2,
        "event_classes": [
            {
                "id": "frame_boundary",
                "contract_sha256": boundary["contract_sha256"],
            }
        ],
        "limits": {
            "max_frames": 2,
            "max_events": 16,
            "max_bytes": 64 * 1024,
            "max_line_bytes": 4096,
            "max_host_ms": 30000,
            "progress_interval_ms": 250,
        },
        "input_movie": file_identity(movie_path),
        "initial_state": {
            "path": str(state_path),
            "format": "mesen-savestate",
            "bytes": state_bytes,
            "sha256": hashlib.sha256(state_data).hexdigest(),
            "frame": state_frame,
            "boundary": "frame_boundary",
        },
        "sink": {"endpoint": sink.endpoint, "token": sink.token},
    }


def require_frozen(session: Session, label: str) -> dict:
    status = require_ok(session.request("status"), label)
    if status.get("state") != "frozen" or not status.get("connected"):
        raise RuntimeError(f"{label} did not preserve a connected frozen host: {status}")
    return status


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    args = parser.parse_args()
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    if content.suffix.lower() not in {".sfc", ".smc"}:
        raise SystemExit(f"SNES content required: {content}")
    if not content.is_file() or not binary.is_file():
        raise SystemExit(f"missing content/binary: content={content} binary={binary}")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(2)
    listener.settimeout(90)
    port = listener.getsockname()[1]
    token = "mesen-state-backed-failure-token"
    owned_pid: int | None = None
    session: Session | None = None

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-state-failure-") as temp:
        home = Path(temp)
        log_path = home / "mesen-state-backed-failure.log"
        state_path = home / "valid-state.mss"
        invalid_state_path = home / "invalid-state.mss"
        movie_path = home / "neutral.movie"
        invalid_movie_path = home / "invalid.movie"
        movie_path.write_text("0:\n1:\n")
        invalid_movie_path.write_text("0:not-a-button\n1:\n")
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "MESEN_BIN": str(binary),
                "EMUCAP_SESSION_TOKEN": token,
                "EMUCAP_LAUNCH_ID": "launch-mesen-state-failure-test",
                "EMUCAP_LAUNCH_WAIT": "45",
                "EMUCAP_POST_CONNECT_GRACE": "0",
                "EMUCAP_LOG": str(log_path),
            }
        )
        launched = subprocess.run(
            [str(LAUNCHER), str(content), str(port), "mesen-state-failure", "snes"],
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
            check=False,
        )
        if launched.returncode != 0:
            raise RuntimeError(f"launch failed:\n{launched.stdout}\n{launched.stderr}")
        owned_pid = int((home / "mesen2" / str(port) / "mesen.pid").read_text().strip())

        try:
            socket_, _ = listener.accept()
            session = Session(socket_)
            hello = require_ok(session.request("hello", {"session_token": token}), "hello")
            require_ok(session.request("pause"), "pause")
            stepped = require_ok(
                session.request("step", {"count": 1, "unit": "frames"}), "frame step"
            )
            if stepped.get("state") != "frozen":
                raise RuntimeError(f"frame step did not return frozen: {stepped}")
            saved = require_ok(
                session.request("save_state", {"path": str(state_path)}), "save_state"
            )
            if (
                saved.get("boundary") != "frame_boundary"
                or saved.get("frame") is None
                or saved.get("bytes") != state_path.stat().st_size
            ):
                raise RuntimeError(f"save did not prove a frame-boundary state: {saved}")
            frame = saved["frame"]
            state_bytes = saved["bytes"]

            # Corrupt bytes pass the adapter's bounded wire shape but must fail native loading
            # before a sink, hook, input override, or guest tick is acquired.
            invalid_state_path.write_bytes(b"not-a-mesen-savestate")
            unused_sink = RecordingSink("capture-invalid-state")
            invalid_load = session.request(
                "record_window",
                recording_params(
                    hello,
                    unused_sink,
                    invalid_state_path,
                    invalid_state_path.stat().st_size,
                    frame,
                    movie_path,
                    "capture-invalid-state",
                ),
            )
            unused_sink.listener.close()
            if invalid_load.get("ok") or invalid_load.get("error", {}).get("kind") != "io_error":
                raise RuntimeError(f"invalid state did not fail load: {invalid_load}")
            require_frozen(session, "status after invalid state")

            # Movie decoding is producer-owned and precedes state mutation. Invalid input must fail
            # without connecting the sink or weakening the existing frozen generation.
            unused_sink = RecordingSink("capture-invalid-input")
            invalid_input = session.request(
                "record_window",
                recording_params(
                    hello,
                    unused_sink,
                    state_path,
                    state_bytes,
                    frame,
                    invalid_movie_path,
                    "capture-invalid-input",
                ),
            )
            unused_sink.listener.close()
            if invalid_input.get("ok") or invalid_input.get("error", {}).get("kind") != "bad_params":
                raise RuntimeError(f"invalid movie did not fail before load: {invalid_input}")
            require_frozen(session, "status after invalid input")

            # This sink accepts authentication and then resets the stream. The valid state has
            # already loaded; the first record must therefore produce a failed, cleaned terminal.
            failed_sink = RecordingSink("capture-state-sink-failure", fail_after_auth=True)
            failed_sink.start()
            failed_capture = require_ok(
                session.request(
                    "record_window",
                    recording_params(
                        hello,
                        failed_sink,
                        state_path,
                        state_bytes,
                        frame,
                        movie_path,
                        "capture-state-sink-failure",
                    ),
                ),
                "state-backed sink failure",
            )
            failed_sink.finish()
            cleanup = failed_capture.get("cleanup", {})
            if (
                failed_capture.get("status") != "failed"
                or failed_capture.get("integrity") == "complete"
                or cleanup.get("hooks") != "not_acquired"
                or cleanup.get("transient_input") != "released"
                or cleanup.get("sink") != "released"
            ):
                raise RuntimeError(f"sink failure did not clean up: {failed_capture}")
            require_frozen(session, "status after sink failure")

            clean_sink = RecordingSink("capture-state-recovery")
            clean_sink.start()
            clean_capture = require_ok(
                session.request(
                    "record_window",
                    recording_params(
                        hello,
                        clean_sink,
                        state_path,
                        state_bytes,
                        frame,
                        movie_path,
                        "capture-state-recovery",
                    ),
                ),
                "state-backed recovery",
            )
            events = clean_sink.finish()
            if (
                clean_capture.get("status") != "completed"
                or clean_capture.get("integrity") != "complete"
                or clean_capture.get("final_execution_state") != "frozen"
                or [event.get("frame") for event in events] != [frame, frame + 1]
            ):
                raise RuntimeError(f"healthy retry differs: {clean_capture}; {events}")

            session.close()
            session = None
            reconnected_socket, _ = listener.accept()
            session = Session(reconnected_socket)
            reconnected = require_ok(
                session.request("hello", {"session_token": token}), "reconnected hello"
            )
            if reconnected.get("launch_id") != hello.get("launch_id"):
                raise RuntimeError(f"reconnected generation differs: {reconnected}")
            require_frozen(session, "reconnected status")

            print(
                json.dumps(
                    {
                        "ok": True,
                        "host_build": hello.get("host_build"),
                        "frame": frame,
                        "invalid_state_failed_frozen": True,
                        "invalid_input_failed_frozen": True,
                        "sink_failure_cleaned_frozen": True,
                        "healthy_retry_complete": True,
                        "reconnect_frozen": True,
                    },
                    separators=(",", ":"),
                )
            )
        except Exception as error:
            log_tail = (
                "\n".join(
                    log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-100:]
                )
                if log_path.is_file()
                else "<missing Mesen log>"
            )
            raise RuntimeError(f"{error}\nMesen log tail:\n{log_tail}") from error
        finally:
            if session is not None:
                session.close()
            if owned_pid is not None:
                terminate_owned(owned_pid)
            listener.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
