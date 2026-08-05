# Changelog

Actively developed beta software — interfaces may still change.

## Unreleased

## 0.13.0-rc.1

### Added
- Added opt-in bounded recording through the capability-scoped debug surface. A recording owns its
  guest-frame interval, returns frozen, and atomically publishes a validated, hashed evidence
  bundle. Negotiated capabilities cover reset-origin capture, warmup intervals, dense input movies,
  typed event classes and stops, event-aligned initial memory, and terminal memory or state without
  changing the default bounded request.
- Added capability-scoped input and debugger drawers while retaining memory access, disassembly,
  call stacks, the complete breakpoint lifecycle, event polling, exact `step`, and live media
  replacement as direct basic controls.
- Added final MCP 2026-07-28 stdio discovery and request metadata support while preserving the
  legacy initialization lifecycle.

### Fixed
- Mesen now separates the current halt cause from recording-boundary eligibility, keeps internal
  occurrence bookkeeping out of public event anchors, and reconciles terminal recording state
  after reconnect or cancellation.
- Direct sessions report the listener port actually selected by the operating system instead of
  treating the port-search base as the assigned endpoint.
- Nintendo DS disassembly and call-stack requests preserve explicit ARM9/ARM7 routing, and
  disassembly also preserves explicit ARM, Thumb, or automatic instruction-mode selection.

### Changed
- Recording event classes use generic producer contracts with exact per-class accounting. Mesen
  installs requested observation hooks only for the active bounded window, and unsupported
  Mednafen profiles remain unchanged.
- Consolidated persistent, real-time, and device-specific controls behind capability discovery.
  Exact `step` and `tap` return frozen; free-running waits are not exposed as basic MCP controls,
  and real-time input operations are named by their running terminal state.
- Runtime-advertised memory regions provide exact finite bounds where supported, and optional
  recording snapshots are admitted only inside those live bounds.

## 0.12.1

### Fixed
- A malformed, unreadable, or oversized `adapter-failure.json` now degrades exact crash evidence
  without blocking a new emulator generation when the ownership, process, and lease records still
  prove that the transition is safe. Corrupt ownership metadata remains fail-closed.
