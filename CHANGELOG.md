# Changelog

Prerelease software — interfaces may still change.

## 0.9.0-alpha.5

### Added
- Dolphin now exposes exact frame stepping, PowerPC disassembly, best-effort ABI call stacks, full register context on exec-breakpoint hits, and atomic cleanup of all emucap-owned breakpoints.

### Changed
- Host-composed frozen input operations can reconnect only to the same control session and launch generation when a cleanup response is lost. The replacement connection must confirm its `launch_id` before an idempotent input release, breakpoint cleanup, or frozen-state restoration is retried; explicit persistent input holds are not released by reconnect alone.
- NDS, PSP, and PC-98 instruction stepping apply one wall-clock deadline across preparation, every backend step, and terminal CPU-state collection. Each blocking GDB or WebSocket exchange is limited to the remaining budget, and the transport's ordinary timeout is restored afterward.
- Dolphin limits each synchronous frame or instruction step request to 15 units. Its launcher uses an isolated per-port user directory for both core and Qt state and rejects symlink redirection out of that portable tree.

### Fixed
- `tap`, `hold_until`, and input replay no longer strand request-scoped input merely because the front TCP connection disappears after an operation took effect. A replacement generation, an unverifiable legacy connection, or unconfirmed terminal cleanup fails loudly instead of mutating an unrelated emulator or reporting completion.
- A delayed final debugger exchange can no longer outlive the instruction-step budget and still return `completed`. Deadline errors report the number of acknowledged steps and leave the CPU halted or frozen.
- Dolphin rejects malformed input, unaligned exec breakpoints, and memory requests that overflow or cross mapped ranges before mutation. Timed-out frame or instruction steps now detach their pending completion state before returning, preventing a late callback from accessing expired request memory.

## 0.9.0-alpha.4

### Changed
- A single synchronous frame or instruction advance is limited to 5,000 units and rejected before emulator mutation when it exceeds the active limit. Longer advances must be split into terminally acknowledged calls; `status.contracts.constraints` reports the public `step` and `run_frames` limits, including PC-98's smaller trace-mode frame limit.
- Rust debugger bridges emit `working` responses while a backend request is active, preventing a slow GDB or WebSocket operation from appearing disconnected. If the front session disappears, the bridge finishes the current backend cleanup before accepting a replacement session.

### Fixed
- `tap` and `hold_until` now attempt input release and frozen-state restoration even when the initial input response, a step, a read, or the release edge times out after taking effect. Cleanup failure remains an error and is never reported as completion.
- NDS, PSP, and PC-98 instruction stepping have a backend deadline below the outer request deadline. PC-98 frame operations also reject requests whose current trace-mode time estimate cannot finish within that budget.
- Mesen2, Mednafen, and Flycast enforce the synchronous advance limit in their adapter wire handlers, so direct or older clients cannot bypass the Control MCP check.

## 0.9.0-alpha.3

### Added
- `status.continuity.runtime_diagnostics` reports invalid, oversized, inaccessible, or otherwise unreadable runtime artifacts without promoting damaged adapter evidence to an exact crash. `bootstrap` and `status` remain usable and return a safe next action.

### Changed
- Direct-mode compatibility tokens and preferred listener ports now live under the private emucap runtime directory. Existing files must be regular, user-owned, and private on Unix; read and write failures are surfaced instead of silently ignored.
- Flycast builds use a pinned commit and recursive submodule manifest, while Mednafen builds verify the pinned release archive SHA-256 before every extraction. Adapter build identities now include both the emucap and upstream revisions.
- Shared POSIX adapter work trees use kernel advisory locks for the complete reset, patch, and build critical section. Windows Mesen builds use a work-tree-specific named mutex.

### Fixed
- Parallel adapter builds can no longer steal a live build lock based on file age or mutate the same work tree concurrently. Kernel locks are released automatically after failure or process exit, so an immediate retry is safe.
- Launch and direct-session tests now share one process-wide environment lock and restore changed values, preventing parallel tests from leaking emulator paths or session identities into one another.

## 0.9.0-alpha.2

### Added
- `write_memory` accepts a bounded raw-binary `input_file` slice as an alternative to inline hex. The Control MCP snapshots and hashes the slice before contacting the adapter, optionally enforces an expected SHA-256, and returns input provenance with the write result.

### Changed
- All Control MCP memory writes reject empty, malformed, overflowing, or over-limit payloads before transport. `status.contracts.constraints` reports the accepted input sources, byte cap, and file-load deadline.

### Fixed
- Direct, broker, and reconnecting bridge transports now enforce one bounded NDJSON frame size while preserving partial frames across read timeouts. Oversized, truncated, or invalid UTF-8 frames close their connection; direct and broker clients also discard malformed or ID-desynchronized response streams before reconnecting.
- Broker mode probes for an existing broker before auto-spawning one, and a reaper waits for every spawned broker child so fast bind failures cannot accumulate as zombies.
- DeSmuME GDB packet transmission and ACK handling now have a two-second total deadline and at most three attempts. Slow readers, missing ACKs, and peer resets close only that GDB connection; subsequent bridge connections can attach without restarting the emulator.

### Removed
- Removed the Control MCP `tap_sequence` convenience method. Call `tap` repeatedly instead; each call releases input and returns frozen, so host latency between calls does not advance emulator time.

## 0.9.0-alpha.1

### Fixed
- Legacy Mesen launchers now select the per-system Lua entry from the normalized `SYSTEM` supplied by `launch_plan`, or from an unambiguous ROM extension during direct use. Game Boy Color no longer silently falls back to the SNES entry; ambiguous media fail loudly unless `EMUCAP_MESEN_LUA` explicitly selects an entry.
- Host-specific launch helpers and their tests now compile only on the platforms that use them, so source builds do not pull in unsupported platform paths.

### Changed
- Frame and instruction stepping now share `step(count, unit, cpu?)`. The separate Control MCP `step_instructions` method was removed, while adapter wire compatibility remains internal. Runtime unit constraints and feature contracts moved to catalog v3.

### Removed
- The Control MCP no longer exposes the host-composed `bisect` tool. Agents can binary-search the frame boundary with repeated atomic `probe` calls, preserving emulator-time semantics without a dedicated MCP method.

## 0.8.0

### Added
- Runtime feature contracts now have a validated catalog and structured per-adapter exceptions. `hello`/`status` expose the active exception IDs, and composed MCP methods are admitted only when their primitive and contract requirements are satisfied.
- Input-override ownership is observable across the supported adapter paths, so agents can distinguish native input, persistent holds, and timed pulses before composing or cleaning up operations.
- Public source snapshots include the `_tests/` development, legacy-comparison, and live-validation assets. They remain outside Cargo targets and runtime launch paths.

### Changed
- PC-98 uses the Rust `emucap-mame-pc98-bridge` as its only supported runtime bridge. The launcher fails explicitly when that bridge is unavailable instead of installing or selecting a Python implementation.
- Large Rust bridge and launch modules are split by responsibility, and unit-test bodies live in sibling `*_tests.rs` files. Build guards reject inline Rust test modules and production Rust source files over 1,200 lines.
- Adapter parameter domains are fail-loud: unsupported controller ports, Mednafen state groups, and oversized or non-atomic timed-input requests are rejected before mutation and described through structured constraints.
- Host-composed temporal operations share terminal cleanup for input release, breakpoint cleanup, and frozen-state restoration; a cleanup failure is no longer reported as successful completion.

### Fixed
- Mesen `reset` sends its terminal response before recreating the Lua session, and the Control MCP waits for the replacement session to answer `status` before reporting success.
- Flycast refuses screenshots after `load_state` until a fresh rendered frame is captured, instead of returning a stale pre-load image.
- Flycast returns valid `status` JSON when input ownership is reported, and captures the completed render rather than sampling before the frame is drawn.
- `launch` waits for the adapter to report a live connection before publishing the runtime generation. Startup failure now returns an error and terminates only the processes owned by that attempted launch.
- Mesen returns input ownership to native controls after `set_input([])` instead of continuing to apply an empty persistent override.
- NDS and PSP report a released input override as `mode: "native"` instead of leaving its mode missing or persistent.
- Mednafen timed input returns ownership to native input on completion, interruption, reset, and disconnect, while explicit `set_input` holds remain visible and releasable.
- NDS, PC-98, and PPSSPP timed-input limits and terminal states are enforced before the bridge can outlive the synchronous request that owns them.
- MCP initialization reports each emucap server's own name and package version instead of the transport library's metadata.

## 0.7.3

### Fixed
- The supported Rust/MCP Mednafen launch path now accepts `sound: true` and passes `-sound 1`, so PC Engine CD and other Mednafen systems can enable audio without using the legacy shell launcher. Audio remains off by default, is independent of `display`, and unsupported adapters reject `sound: true` instead of silently ignoring it.

## 0.7.2

### Added
- Screenshot results now include the decoded PNG's SHA-256 and byte length. Backend-provided state, frame, freshness, and frame-binding metadata is returned as provenance instead of being implied by image timing alone.

### Changed
- PC-98 `launch(display: true)` opens the MAME display while the default launch remains headless.
- PC-98 screenshots explicitly report unverified freshness and frame binding. Because MAME save states do not restore the screen bitmap, the server instructions require a frozen `step(1)` before judging the screen after `load_state`.

### Fixed
- PC-98 named-memory reads and writes reject any request whose offset plus length crosses the selected region boundary, before sending it to GDB.
- Windows builds no longer fail in `process_alive()` when `windows-sys 0.61` exposes `STILL_ACTIVE` as `i32`; the comparison now uses the corresponding `u32` value ([#2](https://github.com/mcpads/emucap/pull/2), thanks @Pesumelga).

## 0.7.1

### Added
- Mesen live control now uses a locally built, pinned MesenCE 2.2.1 host with a repository-owned GPLv3 patch stack and verifiable build sidecar. The project distributes source, patches, and build recipes, not Mesen binaries.

### Changed
- Mesen pause and breakpoint control now remain in a native debugger halt serviced by bounded `codeBreakIdle` callbacks. Frozen sessions make zero guest progress and remain available through same-process reconnects and callback errors. Builds that lack the required host API are rejected as `mesen-patch-required`.

### Fixed
- Mesen numeric command-line settings are applied before first-run and single-instance decisions, so the script timeout, script permissions, and per-session instance override take effect without modifying the user's normal settings.
- GBA launch detects `.gba` content even through a wrapper Lua, stages an exactly 16 KiB BIOS into the isolated runtime, and fails before launch when firmware is missing or invalid instead of opening an interactive firmware prompt.

## 0.7.0

### Added
- Runtime continuity is now observable through `status.continuity` and `status.runtime_instance`. The Control MCP keeps bounded last-good and failure evidence across transport loss, exposes it through `get_failure_context`, and refuses duplicate or ambiguous launches unless an owned live generation is explicitly replaced.
- Flycast captures the exact SH-4 state and a fixed-size pre-failure PC ring before a blocked fatal exception mutates CPU state. It publishes bounded durable evidence, keeps read-only diagnostics available in a quarantine window, and exposes `dismiss_failure` as an explicit end to that window.

### Changed
- Adapters and bridges reconnect to a replacement Control MCP session without restarting their emulator or debugger backend. Request IDs and unfinished work remain scoped to the disconnected session; execution state, breakpoints, and explicit `set_input` holds remain emulator state.
- Timed NDS button/touch and PC-98 button operations acknowledge only after the requested frame effect and input release. A breakpoint or stop returns `status: "interrupted"` after releasing transient input. NDS synchronous timed input is capped at 120 frames.
- An empty `set_input` explicitly returns input ownership to native keyboard/controller handling on Flycast and Mednafen. PPSSPP timed input accepts one button only; unsupported atomic multi-button pulses are rejected before mutation, while persistent combinations remain available through `set_input`.

### Fixed
- Mesen2, Mednafen, and Flycast socket writes are bounded and preserve partial NDJSON writes, so a slow or disconnected peer cannot block the emulator thread indefinitely or corrupt the next response.
- Mesen2 `pause` and breakpoint freezes no longer auto-resume after 30 seconds of inactivity or 10 minutes without an MCP connection by default. Both escape timers are explicit opt-ins and their effective values, together with the unavoidable Lua watchdog instruction drift, are exposed in `status.freeze_policy`.
- Mednafen transient button presses release their override on completion, interruption, reset, and disconnect, preventing a zero-mask override from continuing to suppress native input.
- Mesen2 GBA launches create the portable-data marker needed for the staged BIOS to be discovered without modifying the user's normal settings; other Mesen systems continue to inherit native key mappings.
- Flycast frame-running and input handoff report their actual terminal state, and NDS/PC-98 timed-input stop races no longer report success before release or leave stale input active.

## 0.6.0

### Added
- Mesen Game Gear / Game Boy(GBC): `call_stack` frames, `get_trace` entries, and breakpoint-hit events now carry the ROM `bank` alongside the pc. On these systems the CPU bus is 16-bit and ROM is paged, so a bare address is ambiguous (the same address is different banks at different times); the bank is captured when the code ran and disambiguates it. `call_stack` frames are now `{pc, bank}` objects uniformly across all Mesen systems and `get_trace` entries gain `bank` (`bank` is `null`/omitted where the bank is already in the pc, as on SNES, or the system does not page ROM). `get_trace` tracks bank switches via a shadow refreshed only when the mapper is written, so it adds no per-instruction cost. `status.bank_tagging` advertises tagging only when the loaded cart actually exposes the bank fields, and a `null` bank means the bank is undetermined for that address (e.g. an MBC1 mode-1 / MBC1M low region, which Mesen does not resolve, is reported `null` rather than a wrong bank 0) — so a bank is trustworthy only where non-null.

### Fixed
- PSP: an execution breakpoint at a raw PC (e.g. straight from `get_state`'s `cpu.pc`, as the adapter README documents) now arms at that address. In 0.5.0 the address was mis-read as a `main` offset and rejected as out of range; `memory_type` is now ignored for an exec breakpoint, which takes an absolute address like `disassemble`. Read/write breakpoints still resolve their `memory_type` offset the way `read_memory`/`write_memory` do.
- Mesen: `call_stack` no longer collapses to an empty stack while tracing. The shadow call stack popped on return opcodes, which over-popped on Z80/SM83 conditional returns that were not taken and on interrupt returns (pushed with no matching call opcode), pinning the depth at 0 — most visibly on Game Gear and Game Boy/GBC. Returns are now recognized by the stack pointer unwinding instead (popping at the return site), and `call_stack` reads the freeze-point register snapshot so a delayed read cannot skew the depth. This also fixes the same interrupt-return over-pop on SNES and NES.

## 0.5.0

### Added
- **WonderSwan / WonderSwan Color** (`wswan`), a fifth system on the Mednafen fork (NEC V30MZ, x86 little-endian): memory, registers, screenshot, buttons (including the independent vertical/horizontal cursor pads), save/load state, disassemble (built-in x86), execution and read/write breakpoints, value-conditioned breakpoints, `find_pattern`, `dump_memory`, instruction stepping, `set_layer_enable`, `watch_register`, and `call_stack`. Load a `.ws`/`.wsc` ROM. Not yet supported for WonderSwan: `break_on_reset` and `get_video_state`/`resolve_tile` (video, tile, and palette data are reachable through `ram`/`physical` but have no dedicated decoder).

### Fixed
- A breakpoint that would be accepted but never fire is now rejected with a message naming the correct address form, across the adapters this release touched:
  - PC Engine: an exec breakpoint above the 16-bit logical space (which the core drops) is rejected, and an over-wide `start..end` span no longer makes the core iterate billions of addresses.
  - PSP: `set_breakpoint` with a `memory_type` resolves the offset the same way `read_memory`/`write_memory` do (`main` → the absolute RAM address) instead of arming at a raw low address.
  - Game Boy: a breakpoint on a banked region (VRAM / WRAM / cart RAM) at an offset outside the CPU-visible window is rejected instead of aliasing onto unrelated memory; NES PPU / palette / OAM (off the CPU bus) are rejected instead of never firing.
  - PC-98: a breakpoint offset past the end of its memory region is rejected before arming.
  - Flycast: an exec breakpoint given in any SH-4 cached/uncached mirror form matches the running PC.
  - WonderSwan: exec breakpoints use the 16-bit IP; read/write breakpoints accept only `physical`/`ram`; write value-conditions accept only `value_len=1`.
- WonderSwan `call_stack` classifies calls and returns that sit behind V30MZ instruction prefixes.

## 0.4.1

Robustness and correctness hardening across every adapter and the shared control/session layer.

### Fixed
- NDS: `find_pattern` and `dump_memory` no longer crash the emulator — a large scan overflowed a fixed debugger buffer; memory reads and writes are now clamped to the buffer and chunked.
- NDS: agent `touch` lands at the requested coordinate instead of at 1/16 of it.
- NDS: `write_memory` caps and chunks a large write instead of silently dropping one that exceeds the debugger packet buffer.
- NDS: a shared-RAM (`main`) write leaves both cores in their prior running/frozen state, and a stray interrupt echo no longer re-freezes a resumed core.
- NDS: `poll_events` validates its filter before draining, and preserves one core's events when the other core's drain errors.
- PC-98: `save_state` writes atomically — a mid-save failure no longer destroys a previously valid state at the same path.
- PC-98: `run_frames` and frame-step drain a pending stop first, so a stale breakpoint hit is not mis-reported as the frames result.
- PC-98: the pause/interrupt echo no longer surfaces as a phantom breakpoint event.
- PSP: `poll_events` validates its filter before draining, so a malformed filter no longer discards buffered hits.
- PSP: `reset` reports accurately for a `display: true` (GUI) session instead of claiming completion while the reboot is still in flight.
- Mesen: a multi-byte write value-breakpoint on a system-register address ($2000–$7FFF) fires on both bank mirrors and compares the bytes actually written; `auto_savestate` is rejected rather than silently ignored.
- Mednafen: the Saturn `physical` address space is rejected in `probe` and `find_pattern` (matching `read_memory`/`write_memory`) instead of returning silent zeros.
- `dump_memory` publishes atomically for every adapter — a failed dump never destroys a prior one — and refuses a destination that is a file or symlink.
- Session identity is keyed per session, so a second session in the same working directory can no longer adopt another's running emulator.
- Broker mode fences responses by session, so a stale reply after a session hand-off is rejected.
- Launch: `caffeinate` (the HITL display keep-awake) is reaped across all display adapters instead of leaking one zombie per relaunch, and a failed launch no longer leaves a staging temp directory behind.

### Changed
- PSP `adapters/ppsspp/build.sh`: `PPSSPPHeadless` is the guaranteed build; the GUI build (`PPSSPPSDL`, for `display: true`) is best-effort and no longer required at configure time, so a host without SDL3 still builds the headless debugger.
- The atomic directory swap behind `dump_memory` uses a single-syscall exchange where the platform provides one.

## 0.4.0

### Added
- **PSP (PlayStation Portable)** adapter, via a headless PPSSPP fork with a WebSocket debugger bridge: memory, registers, screenshot, buttons, save/load state, disassemble, instruction stepping, execution and read/write breakpoints, and reset. Build with `adapters/ppsspp/build.sh`.
- PSP `display: true` HITL mode — the adapter opens a real PPSSPP window a human watches and plays (keyboard/gamepad) while the agent reads and injects over the debugger WebSocket, mirroring the NDS display mode; the GUI runs under an isolated per-session profile, so it never touches the operator's real PPSSPP config or saves.
- PSP `dump_memory` — bulk-export a memory region to `<dir>/<region>.bin` + `regions.json` for large regions, instead of inline hex.
- NDS `dump_memory` and `find_pattern` — bulk-export Main RAM to region files, and scan a region for a byte pattern, mirroring the other adapters.

### Fixed
- PSP: the debugger WebSocket listens on loopback only, so it is not reachable from other hosts.
- PSP: `reset` performs a real reboot and reports failure when the reboot fails, instead of acknowledging a no-op.
- PSP: `main` reads and writes outside PSP user RAM are rejected instead of aliasing onto other memory.
- PSP: duplicate memory and execution breakpoints on one address are ref-counted, so clearing one no longer disarms another.
- Mesen: the GBA BIOS is resolved from the documented firmware directory, and an already-staged BIOS is used when its source is gone.
- NDS: a memory read or register read before `poll_events` no longer resumes past a pending breakpoint stop.

### Changed
- `EMUCAP_PPSSPP_SRC` is read-only input: the build clones it into the owned work tree and never patches or builds in place.

## 0.3.1

### Fixed
- NDS: memory reads, writes, and breakpoints out of the selected region are rejected instead of reaching unrelated bus space.
- NDS: a breakpoint hit is no longer lost when a memory read or screenshot runs before `poll_events`.
- NDS: an emulator or bridge that crashes at startup fails the launch instead of reporting success and leaving a stray process.
- Mesen: GBA launches stage the BIOS (from `EMUCAP_GBA_BIOS`, else the mesen2 firmware directory) instead of hanging on a firmware prompt, and fail with a clear message when it is missing.

### Changed
- NDS `read_memory` is capped at 128 KB per call — read a larger region in chunks.
- Adapter READMEs are English only; `README.ko.md` at the repo root remains the Korean entry point.

## 0.3.0

### Added
- **Nintendo DS** adapter, via a headless DeSmuME fork with an ARM9/ARM7 GDB bridge: memory, registers, screenshot, buttons, touchscreen (`touch`), save/load state, disassemble, call_stack, reset, execution breakpoints, and an optional window for human-in-the-loop play (`display: true`). Build with `adapters/desmume-nds/build.sh`.
- **Game Boy, Game Boy Color, Game Boy Advance, and NES** on the Mesen2 adapter, with a GBA ARM7 disassembler.
- Game Gear / Master System VRAM write breakpoints.
- `owned_instance` in `status` — the pids and pidfiles this session started, for scoped cleanup.
- Optional `cpu` argument on `get_state` / `resume` / `pause` / `step` for multi-core backends (NDS ARM9/ARM7).
- `EMUCAP_DEADMAN_MS<=0` holds a freeze indefinitely.

### Changed
- Tool argument descriptions defer per-system specifics to `status` and each adapter's README.
- The DeSmuME fork build is pinned to a known-good commit.

### Fixed
- Mesen2 `get_state` after a freeze reports the frozen instant, not a drifted one.
- NDS screenshots and memory reads taken while the game runs are no longer stale or corrupted.
- NDS `reset` leaves the game paused; a failed launch no longer leaves a stray emulator process.
- NDS `step_instructions` steps by instruction, and `poll_events` reports breakpoint hits without noise.
- NDS timed `press_buttons` / `touch` require a running game.
- NDS screenshot and disassemble no longer fail intermittently, and parallel NDS sessions no longer collide on a GDB port.

## 0.2.0

### Added
- Game Gear / Master System on the Mesen2 adapter (Z80). Launch with `system: "gamegear"`; buttons and `sms*` memory types are documented in `adapters/mesen2/README.md`.
- PC-98 second floppy (`content_path2` → `-flop2`) for two-drive titles.
- `watch_register` accepts a capped `max_instructions` budget.

### Changed
- Mesen2 adapter split into a shared `emucap-core.lua` plus per-system entries (`emucap-snes.lua`, `emucap-sms.lua`).
- `read_memory` over the size cap now returns an error instead of truncating.
- Frame counts and input-hold durations are capped to fit the link deadline.

### Fixed
- Mesen2 work-RAM read/write breakpoints now fire (RAM offset → CPU-bus address); multi-byte value filters read the correct bytes.
- Mesen2 / Mednafen hot breakpoints no longer flood the emulator thread and drop the connection.
- PC-98 GDB-RSP stream no longer desyncs when a `run_frames` frame target coincides with a breakpoint hit while tracing; the frozen-idle loop no longer fork-storms.
- Mednafen Saturn rejects the unimplemented `physical` address space instead of silent 0-reads / no-op writes.
- TCP and broker links: poison on partial write, deferred deadline against endless `working` keepalives, and split-reply demux.
- `track` observe rejects truncated reads (a hashed prefix could give a false pass/fail).
- Flycast: Dreamcast addresses at or above `0x80000000` no longer truncate on a 32-bit `long` (Windows) — JSON numbers parse via `strtoull` ([#1](https://github.com/mcpads/emucap/pull/1), thanks @UzuCore). Build-hook injection is idempotent and CRLF-normalized.

## 0.1.0

Initial public snapshot.
