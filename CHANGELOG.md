# Changelog

Beta software — interfaces may still change.

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
