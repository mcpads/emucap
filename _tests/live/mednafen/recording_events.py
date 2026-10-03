"""Normalize only frame/clock origins in complete producer recording bundles."""
import hashlib
import json
from pathlib import Path


def check_frame_sequence(events: list[dict], frames: int) -> None:
    assert len(events) == 2 * frames
    for index, event in enumerate(events):
        assert event["sequence"] == index
        assert event["class"] == ("frame_boundary" if index % 2 == 0 else "frame_completed")
        assert event["frame"] == index // 2
        assert event["clock"] == {"domain": "frame", "tick": index // 2 + index % 2}


def normalized_events(bundle: Path) -> tuple[list[dict], str]:
    manifest = json.loads((bundle / "manifest.json").read_text())
    terminal = manifest.get("terminal", {})
    if (
        terminal.get("operation_outcome") != "completed"
        or terminal.get("integrity") != "complete"
    ):
        raise RuntimeError(f"recording did not complete with integrity: {terminal}")
    events = [
        json.loads(line)
        for line in (bundle / "events/segment-000.ndjson").read_text().splitlines()
    ]
    f_start = manifest["scope"]["f_start"]
    clock_origins: dict[str, int] = {}
    normalized = []
    for event in events:
        clock = event["clock"]
        domain = clock["domain"]
        clock_origins.setdefault(domain, clock["tick"])
        normalized.append(
            {
                "sequence": event["sequence"],
                "class": event["class"],
                "contract_sha256": event["contract_sha256"],
                "frame": event["frame"] - f_start,
                "clock": {
                    "domain": domain,
                    "tick": clock["tick"] - clock_origins[domain],
                },
                "payload": event["payload"],
            }
        )
    encoded = json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode()
    return normalized, hashlib.sha256(encoded).hexdigest()
