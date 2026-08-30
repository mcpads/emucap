# Changelog

Actively developed beta software — interfaces may still change.

## Unreleased

## 0.16.0-alpha.1

### Added
- Added a managed original Xbox path based on a pinned xemu fork. It provides isolated machine
  generations, controlled pre-first-instruction start, bounded QMP/GDB control, CPU memory and
  debugging, exact frame and instruction stepping, explicit audio output with a silent default,
  native-safe button and analog input, screenshots, reset, and optical-disc replacement. Analog
  sticks and triggers are exposed only through live per-port axis capabilities; invalid ranges and
  conflicting full-trigger aliases fail before mutation, while empty state returns native ownership.
  A representative game XISO
  reached game-visible menus and responded to injected confirm and directional input; audible output
  was also observed with `sound:true`. Startup now retries a listener that has not emitted its QMP
  greeting yet without accepting malformed protocol data. Frame stepping now consumes its terminal
  GDB stop before any post-continue QMP query, preventing immediate watchpoints from poisoning the
  control connection. The pinned host patch preserves existing QEMU big-lock ownership during TCG
  watchpoint re-entry and defers BQL-held x86 interrupt-access stops until the matched access has
  completed. Ordinary MTTCG watchpoints now retranslate one instruction without invalidating their
  still-executing translation block, instead of aborting or stalling the host. Instruction stepping
  reports watchpoint and exec-breakpoint preemption instead of consuming it. Disassembly uses the
  absolute CPU address view, while controlled launch advertises the exact x86 reset linear address
  separately from the segmented EIP register. Decoder lookahead is bounded at the selected address
  space, so the valid `0xfffffff0` reset vector remains disassemblable. The `main` memory view now
  uses xemu's physical-RAM mode for complete bounded reads, writes, searches, and dumps, restoring
  virtual CPU addressing before every terminal response instead of failing on an unmapped
  `0x80000000` page. Frozen state save/load now publishes a generation-bound container over the
  internal VM/HDD snapshot, complete EEPROM bytes, exact current disc, host build, machine inputs,
  and controller topology. Load verifies QMP/GDB serviceability and restores the prior frozen
  snapshot on failure; foreign generations and changed media fail before snapshot mutation. A
  negotiated debug probe composes one exact state load, bounded frame advance, and frozen memory
  read without a caller-latency gap, preserving breakpoint preemption and identifying the exact
  state-container bytes it consumed.

### Changed
- Original Xbox capability metadata no longer exposes obsolete duplicate controls or optional
  instrumentation as `planned_methods`; the live method list and negotiated drawers are the whole
  callable surface for that generation.

## 0.15.1

### Fixed
- PC-98 MAME state restore now refreshes discovered input fields, reapplies persistent holds, and
  keeps request-local adapter failures from tearing down the bridge connection. A frozen
  `change_media` → `load_state` → input or status sequence therefore remains serviceable.
- PC-98 state-load responses now disclose that mounted media is external to the state bundle and
  no longer claim deterministic replay unless the caller provides and verifies the media identity.

## 0.15.0

### Added
- Added an explicit NP2kai compatibility backend for PC-98 `.hdi` media that MAME cannot model
  faithfully. It runs a pinned, license-audited libretro core in an emucap-owned headless profile
  and exposes the standard PC-98 Control/Debug surface with capability-reported device limits,
  identity-bound states, and verified `hdd0` replacement.
- Mednafen Mega Drive sessions can opt into repeatable reset-origin recordings. A disposable home
  captures a bounded pre-instruction guest state and restores it with declared non-savestate
  controls before reset and dense movie input, without clearing ordinary Mednafen saves.
- Maintained Mesen SNES sessions can explicitly preserve a frozen frame-boundary state as a
  producer-managed receipt and use its opaque ID as the origin of an atomic bounded recording
  window. The transaction requires a dense input movie, publishes the exact initial state in the
  bundle, and leaves ordinary save/load and recording behavior unchanged.
- PCSX2 managed sessions can now inspect and replace the two main `.ps2` memory-card slots through
  the generic frozen-only `change_media` contract. Busy-card rejection, rollback status, writable
  attach-time identity, target-slot isolation, and PCSX2's guest-visible reinsertion delay are
  reported without hidden guest-time advancement.

### Fixed
- PC-98 now exposes numeric-keypad inputs as `kp0` through `kp9`, keeps them distinct from the
  ordinary digit row, accepts common MAME/keypad aliases, and reports only callable canonical names
  in the runtime `input_buttons.available` list.
- Mesen frame steps now stop at a main-CPU opcode boundary suitable for native savestates, allowing
  a frame-boundary receipt without serializing a half-executed instruction. Recording cleanup also
  distinguishes input ownership that was never acquired from ownership whose release is unknown.
- Generated Mesen work trees disable Git fsmonitor so a detached repository daemon cannot inherit
  and indefinitely retain the adapter build lock.

## 0.14.6

### Fixed
- Managed CUE, GDI, CCD, TOC, and M3U launches now bind a SHA-256 identity over the entry and every
  loader-declared member before starting the emulator. This includes Mednafen's implicit SBI
  sidecar. `get_rom_info` reports the pre-launch identity bound to the managed generation instead of
  treating a layout identifier, descriptor-only hash, or later mutable source read as that identity.
- Composite-media admission now rejects path escape, symlinks in any member component, special or
  empty files, self-reference, ambiguous CloneCD metadata, malformed track boundaries, oversized
  graphs, and excessive playlist recursion before guest execution.
- Selecting a descriptor no longer authorizes or probes every file under its directory. `launch_plan`
  discloses an exact bounded member set for review before metadata, content, or hashes are read;
  nested M3U descriptors reveal one approved frontier at a time, and direct launch without the
  server-produced approval fails before changing a runtime generation.

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
