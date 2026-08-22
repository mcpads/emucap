# Changelog

Actively developed beta software — interfaces may still change.

## Unreleased

## 0.14.5

### Added
- Maintained Mesen SNES recordings can opt into exact modes 0–6 tiled-background character-data
  VRAM-word fetches, with producer-declared address, layer, and scanline filters and exact
  occurrence stops. The event does not claim final compositing or displayed-pixel contribution.

### Fixed
- SNES OBJ, CGRAM, and BG-fetch records now take scanline, dot, and horizontal-clock coordinates
  from the live PPU counters instead of a debugger state snapshot that could be stale.
- Tracking and regression identifiers now reject path syntax and punctuation before filesystem
  access, missing runs return stable `run_not_found` errors, and stored record identities must
  match their managed paths.
- Managed ledger, dump, regression, recording, and legacy-bundle members reject symlink escapes,
  oversized metadata, and identity or size mismatches. CUE launches require a closed regular-file
  graph under the CUE directory, PC-98 state ZIP extraction is bounded, and explicit JSON offloads
  replace their destination atomically without following a destination symlink.
- Screenshot, state, runtime-copy, and regression payload writes now use collision-resistant
  sibling staging and atomic replacement; file-backed screenshots and debugger output are bounded,
  large state hashes are streamed, and PC-98 trace reads inspect only a bounded tail. Emulator build
  sidecars, openMSX session manifests and screenshots, and diagnostic PID files are also read through
  bounded regular-file paths; standalone Neo Geo scratch directories no longer reuse a PID-only name.

## 0.14.4

### Added
- Expanded the baseline debugger surface with exact Mesen main-CPU instruction stepping, exact
  VBlank-boundary frame stepping for Nintendo DS and PSP, frozen best-effort call stacks for GBA,
  Nintendo 64, Neo Geo, and PSP, and bounded Dolphin read/write watchpoints and native reset.
- Recording event classes can opt into producer-advertised transaction or observation scopes.
  Omitted overrides preserve each producer's existing default, and invalid selections fail before
  guest mutation.
- MCP discovery now states that `write_memory` accepts either inline hexadecimal data or a bounded
  raw-file slice on the MCP host.

### Fixed
- Mesen runtime refresh now preserves battery saves and other portable data. Repeatable SNES
  launches use a separate disposable profile instead of clearing the ordinary emucap profile.
- PPSSPP frame stepping preserves native debugger stop reasons, and Mednafen breakpoint handling
  preserves the requested address identity and terminal execution provenance.
- WonderSwan reset-origin recording now arms its native reset guard without requiring an address
  space that the event does not have.

## 0.14.3

### Added
- PC-98 launches can select a PC-9801-26K or PC-9801-86 C-bus sound board independently of
  host audio output. Omitting the option keeps the sound slot empty, and launch planning reports
  the available choices and ROM requirement.

### Fixed
- PC-98 managed and legacy launches now honor explicit audio independently of display visibility.
  Silent-by-default probes remain silent, while `sound:true` uses MAME's host-selected audio
  provider without implying that a C-bus sound board or its ROMs are installed.
- PC-98 save states now preserve and restore the last presented raster image without advancing
  guest time, preventing a restored frozen state from displaying a stale black frame.

## 0.14.2

### Added
- Added capability-scoped relative pointer movement, left/right/middle clicks, and drags for
  PC-98. Each operation advances an explicit number of guest frames, releases transient buttons,
  and returns frozen without replacing native input ownership after completion.

### Fixed
- PC-98 managed launch now enables the MAME mouse device and establishes a real GDB interrupt
  before reporting its initial frozen state, instead of mistaking the initial stop reply for a
  completed pause.

## 0.14.1

### Added
- Added an opt-in Mesen SNES event for bounded CGRAM renderer lookups. Its payload distinguishes
  shared pre-routing, main-screen, and sub-screen reads without claiming final display contribution,
  and supports capability-scoped filters and exact occurrence stops.

### Fixed
- Exact Mesen steps now let pausing debugger stops preempt the requested advance. The response
  returns frozen interruption evidence while preserving the breakpoint and its hit event instead of
  continuing past the stop.
- Partial-frame recording stops now read terminal memory and state at the actual frozen terminal
  frame, so published terminal members and manifest coordinates describe the same guest position.

## 0.14.0

### Added
- Added opt-in controlled launch that returns at a frozen guest boundary, plus a capability-bound
  Mesen SNES execution profile for repeatable reset-origin recordings with explicit input movies.
  Maintained Mednafen systems now halt before their first guest instruction when controlled launch
  is requested, without claiming a repeatable initial-state profile.
- Added capability-scoped recording filters and occurrence stops. The maintained Mesen SNES profile
  can narrow OBJ-consumption reads by memory kind and address, then close the capture on an exact
  persisted matching occurrence without routing high-rate records through the live event queue.
- Added native Mesen power-cycle control and exact managed-generation discovery and reattachment.
  A returned generation can move to another control session only through its advertised
  `launch_id`, private reconnect capability, process identity, and returned lease.

### Fixed
- Mednafen managed launches now use a per-port emucap-owned home and stage canonical BIOS files from
  the shared emucap firmware inventory instead of reading or changing `~/.mednafen`.
- Recording preserves frame-domain terminal coordinates on interrupted and partial-frame paths,
  keeps a non-frame stop's own clock domain, and publishes only canonical terminal stop facts.
- Foreign or unverifiable runtime generations no longer become implicit cleanup targets. Runtime
  inventory reports the exact safe handoff action without requiring edits to session files.

### Changed
- Broker sessions no longer transfer control because a heartbeat is absent or time elapsed.
  Reconnection requires the same broker registration, or an already-returned front session and the
  same durable managed launch identity.

## 0.13.0

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
