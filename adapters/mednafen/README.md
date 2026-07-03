# emucap — Mednafen (Sega Saturn · Sony PlayStation · PC Engine · Mega Drive) adapter

> 한국어: [README.ko.md](README.ko.md)

Mednafen has no Lua like Mesen, so **we patch Mednafen and inject a socket
client (`emucap.cpp`)** — a C++ port of Mesen's `emucap-core.lua`. It connects to
emucap-mcp and serves the same NDJSON protocol, so the Rust side (TcpLink · tools
· MCP) stays unchanged.

**One binary handles Saturn (ss) · PlayStation (psx) · PC Engine (pce) · Mega
Drive (md), all of them.** Mednafen auto-detects the system from the loaded disc/
ROM, and at runtime emucap branches on `CurGame->shortname` ("ss"/"psx"/"pce"/
"md") for system-specific behavior (address-space mapping · button table ·
endianness). The common debugger interface works on each system unmodified. PCE
analysis defaults to the accuracy/debugger-first `pce` core. `pce_fast` is also
built, but since Mednafen provides no Debugger pointer there, the memory/register/
breakpoint family of tools is demoted to `no_debugger`.

Mednafen is GPL, so we do not vendor/redistribute it wholesale. Only our
additions (`emucap.cpp`/`.h`) live in this repository, and `build.sh` fetches
upstream Mednafen locally to patch and build it.

## Prerequisites

### Build dependencies (the agent installs these — the user does nothing here)
- brew: `flac libsndfile lzo musepack sdl2-compat zstd gettext`, `pkg-config`, clang.
- `build.sh` fetches and builds Mednafen itself, so there is no separate emulator install for the user.

### BIOS files — the USER must supply these (agent: relay this as a checklist)

BIOS files are copyrighted console firmware. **emucap cannot and will not include them** — the user
provides them from their own console or dumps. **Never commit BIOS files to the repository.**

**Where they go.** On macOS/Linux the folder is `~/.mednafen/firmware/`. Create it first if it is
missing (`mkdir -p ~/.mednafen/firmware`), then drop the file in with its exact name (below).
On **Windows** the Mednafen adapter is BETA and building it from source is non-trivial — do not assume a
firmware path there; see the **Platforms** note in the top-level `README.md` for the agent-driven fallback
(install the emulator from upstream and point emucap at it via env overrides) instead of guessing a path.

**Per system — give the user the exact filename and the exact folder:**

| System | BIOS needed? | Exact file → where |
|--------|--------------|--------------------|
| **PlayStation** (psx) | **Required** — cannot boot without it | `scph5500.bin` (JP) · `scph5501.bin` (NA) · `scph5502.bin` (EU), matching the disc's region → `~/.mednafen/firmware/` |
| **PC Engine CD** (pce, CD titles) | **Required** for CD titles | `syscard3.pce` → `~/.mednafen/firmware/` (default; to use another location set `pce.cdbios`/`pce_fast.cdbios`) |
| **Saturn** (ss) | Recommended | `sega_101.bin` (JP), etc. → `~/.mednafen/firmware/`; point at it with `ss.bios_jp` in `~/.mednafen/mednafen.cfg` |
| **PC Engine HuCard** (`.pce`) | **None** | boots without a BIOS |
| **Mega Drive / Genesis** | **None** | cartridge ROMs (`.md`/`.gen`/`.smd`, or a `.bin` with a header) boot without a BIOS |

If the user already runs RetroArch they can copy and reuse the same BIOS files, but the RetroArch system
directory is a host-specific setting, so do not hardcode a local path into docs or scripts.

**How to verify it worked.** After placing the file, launch the disc: a good boot reaches the BIOS/game
screen. The classic failure is a missing or misnamed BIOS — e.g. PSX boot fails with
`Error opening scph5500.bin`. If you see that, the file is absent, in the wrong folder, or spelled wrong
(check the exact name and the region suffix). Unlike Saturn, PSX cannot boot at all without a BIOS.

### Game ROM / disc — the USER must supply this
emucap does not include games. The user provides the ROM or disc image (a `.cue` plus its track files for
CD systems; a single cartridge ROM for Mega Drive/HuCard). Ask the user for the file, confirm its exact
path, and pass that path to `launch.sh` (see **Usage**).

## Build
```
./build.sh
```
- Download Mednafen 1.32.1 → inject `emucap.cpp` → re-inject all emucap hooks into the fresh source with perl →
  `./configure --enable-ss --enable-psx --enable-pce --enable-pce-fast --enable-md --enable-debugger` → make.
  Output: `work/mednafen/src/mednafen`.
- **`--enable-ss` required**: configure's Saturn auto-detection only turns it on when `host_cpu` is `aarch64*`/`arm64*`,
  but Apple Silicon reports as `arm` and gets dropped. psx is on by default, but `--enable-psx` pins the intent.
- **Hooks injected by build.sh (no reliance on hand-edits · reproducible)**: ① the main.cpp frame loop (`emucap_service`/
  `emucap_capture`, common driver path), ② input injection `emucap_apply_input(PortData[0])` —
  in the core-agnostic `mednafen.cpp`, at both the pre-Emulate and MidSync phases (common to ss · psx · pce · md), ③ value-conditioned BP recording
  `emucap_bp_record` — for Saturn `ss/debug.inc` (2 read/write functions), for PSX `CheckCPUBPCallB`
  (a single callback) in `psx/debug.cpp`, for PCE the HuC6280 logical read/write match in `pce/debug.cpp`,
  for MD the 68000 read/write match in `md/debug.cpp`, ④ input diagnostics `emucap_game_data_store` —
  the gamepad update path of `ss` · `psx` · `pce` · `pce_fast` · `md`.
  Each injection is verified at build time against a fixed string.
- automake not needed: added directly to OBJECTS in the generated Makefile.
- Set `EMUCAP_MEDNAFEN_WORK=/path/to/build-dir` only to an empty directory or a
  directory previously created by `build.sh`.  Non-empty custom work
  directories are rejected unless they carry the script's ownership marker.

## Usage
Launch the built binary with `launch.sh`. The actual port is authoritatively the `listening_port` from MCP `status`
(the example is 47800). Calling `status` sets up the MCP listener, so do not skip it. `launch.sh` refuses to
launch the emulator if there is no MCP listener on the port, does not kill anything if an emulator is already
connected, and only cleans up orphan Mednafen processes from its own pidfile. It does not return success
until the emucap connection is confirmed. Even right after connecting, it verifies for a default 3 seconds
(`EMUCAP_POST_CONNECT_GRACE`) that the PID and TCP connection stay up, so the "connected" output appears only
after passing the immediate-death case.  By default the per-port binary copy, pidfile, and log live under the
OS-specific emucap data root (`EMUCAP_EMU_HOME` override, otherwise macOS `~/Library/Application Support/emucap`,
Linux `${XDG_DATA_HOME:-~/.local/share}/emucap`, Windows `%LOCALAPPDATA%\emucap`).
So that a parent-shell exit does not propagate SIGHUP in a transient PTY like Codex, the launcher, if `python3`
is available, launches Mednafen into a new session with `start_new_session=True` and detaches stdio to the log/
`/dev/null` (falling back to `nohup` only when `python3` is absent).
In environments like Codex where you cannot make an MCP tool call while a shell command is running, `status`
before launch prepares the background accept/hello ahead of time. Skipping this procedure means only the TCP
connects while the protocol round-trip lags, and Mednafen may disconnect shortly.
```
# Saturn
./launch.sh "/path/to/saturn.cue" 47800
# PlayStation
./launch.sh "/path/to/psx.cue" 47800
# PC Engine CD-ROM2 / HuCard
MEDNAFEN_FORCE_MODULE=pce ./launch.sh "/path/to/pce.cue" 47800
# Mega Drive / Genesis
MEDNAFEN_FORCE_MODULE=md ./launch.sh "/path/to/game.md" 47800
```
(`launch.sh` uses `SDL_VIDEODRIVER=dummy` · `-sound 0` by default. If you need a screen, use
`EMUCAP_HEADLESS=0`; if you need sound, use `MEDNAFEN_SOUND=1`, adjusting via environment variables. Other environment variables:
`MEDNAFEN_BIN` (fork binary path, default `work/mednafen/src/mednafen`), `EMUCAP_LAUNCH_WAIT` (connection wait
seconds, default 20), `EMUCAP_EMU_HOME` (emucap data root), `EMUCAP_LOG` (log path), `EMUCAP_SESSION_TOKEN` (when unspecified, auto-loaded from
the per-port token file reported by `runtime_paths.token_file`). Override the build version with `MEDNAFEN_VER`.)
So if you used `launch.sh`, a separate `SDL_VIDEODRIVER=dummy` retry is not a new measure.
If the log ends near `Initializing video...` followed by `Signal has been caught ... SIGTERM`, it is usually
not a video crash but `launch.sh` cleaning up its own process after a connection timeout. First re-query
`status` right before launch and check whether the port is stale.
For diagnosis by connection symptom — such as the PID disappearing after `Mednafen 연결됨`, `Broken pipe`, or
`CLOSE_WAIT` — check the Mednafen log tail and re-query `status`/`listening_port` right before launch.

PCE analysis uses the exact `pce` core that has a Debugger. If auto-detection falls to `pce_fast` or the CUE is
ambiguous, pin it with `-force_module pce`. For PCE-CD the CUE track layout, not the filename, is authoritative.
An abbreviated/download CUE and the real original CUE can coexist, so first check the DATA track and track count.
Unsupported `CATALOG` and missing `.sbi` in the Mednafen log are usually non-fatal warnings; judge success/
failure by `Using module: pce`, the TOC output, and whether MCP `status` is connected.

MD `.bin` shares its extension with images of other systems. If there is no header or the filename is ambiguous,
pin it with `MEDNAFEN_FORCE_MODULE=md` or the 4th launch argument `md`. For the `md` module the launcher adds
`-md.input.auto 0 -md.input.port1 gamepad6` to pin the 6-button input buffer.

## memory_type = address space (exposed by the debugger)
- **Saturn (14 kinds)**: `workraml` (1MB) · `workramh` (1MB) · `vdp1vram` (512KB) · `vdp2vram` (512KB) ·
  `vdp1fb0`/`vdp1fb1` · `scspram` (512KB) · `cram` (4KB, VDP2 palette — stored raw; interpret index/color
  format via CRAM_Mode) · `backup` (32KB) · `physical` (SH-2 external bus), etc.
- **PSX (4 kinds)**: `cpu` (32-bit CPU bus — auto-decodes KUSEG/KSEG0/KSEG1 mirrors · scratchpad · BIOS · HW;
  exec BP and value reads happen here) · `ram` (main RAM 2MB direct) · `spu` (SPU RAM 512KB) · `gpu` (VRAM 1MB).
  MIPS is little-endian, so multi-byte value assembly is LE.
- **PCE exact (`pce`)**: `cpu` (HuC6280 16-bit logical — reflects the current MPR mapping, exec/read/write BP and value reads happen here) ·
  `physical` (21-bit physical) · `ram` (8KB, 32KB on SGX) · `vram0` (VDC VRAM, byte address) · `vram1` (SGX VDC-B VRAM, read/write BP) · `sat0` (VDC SAT) ·
  `pram` (VCE palette) · `adpcm` (CD ADPCM RAM, CD titles) · `acram` (Arcade Card) · `bram` · `psgram0..5`.
  HuC6280 is little-endian. `pce_fast` has no Debugger and does not expose these address spaces.
- **MD/Mega Drive**: `cpu` (68000 24-bit CPU physical — exec/read/write BP · value reads · disassemble happen here) ·
  `ram` (Work RAM 64KB, referenced at public offset 0x0000; read/write BP is internally mapped to CPU addresses 0xFF0000~0xFFFFFF) ·
  `zram` (Z80 RAM 8KB) · `vram` (VDP VRAM 64KB) · `cram` (VDP CRAM 128B, unpacked bus color word) ·
  `vsram` (VDP VSRAM 128B) · `vdpreg` (VDP register 32B). 68000 is big-endian. In Mednafen MD a
  `cpu` address-space write is a no-op, so `write_memory("cpu", ...)` is rejected by the adapter. A `vdpreg` write
  can change the screen mode · IRQ · scroll table reference, so use it only when needed during analysis, and keep
  smoke tests read-centric.

Button names: Saturn `a/b/c/x/y/z/l/r/start/directions` (`l`=`ls` · `r`=`rs` aliases), PSX `cross/circle/triangle/square/l1/l2/r1/r2/
start/select/directions` (SNES-style `l`=`l1` · `r`=`r1` aliases, plus DualShock `l3/r3`), PCE `i/ii/run/select/directions` (convenience aliases `a/b/start`,
6-button `iii/iv/v/vi`), MD `a/b/c/x/y/z/mode/start/directions`. All are active-high. The third argument of Mednafen's `IDIIS_Button*` is
not a BitOffset but a ConfigOrder; the actual raw bit is determined by the core's IDII declaration order and padding.

## Implementation scope
- **Working · proven**: `hello`/`status`/`read_memory`/`write_memory`/`get_state` (registers)/
  `save_state` · `load_state`/`run_frames`/`pause` · `step` · `resume` (freeze state machine: while frozen,
  `emucap_service` spins to block frame advance)/`probe` (atomic load → advance → read, deterministic)/
  `set_breakpoint` · `clear_breakpoint` · `clear_all_breakpoints` · `list_breakpoints` · `poll_events`
  (exec/read/write BP — via `AddBreakPoint`+`SetCPUCallback` the core auto-switches to DebugMode, and on a hit
  spins inside the callback for instruction-granularity freeze; read/write support value conditions (`value`/`value_mask`/`value_len`)
  and `pc_min`/`pc_max` filters. **The write value-condition works across all systems by injecting the *value being written***
  (SS=decoder-cloned 21 opcodes [including RMW] · PSX=GPR[rt] callback threading · PCE=WriteHandler V · MD=cloned bus; width-masked).
  The read value-condition falls back since the value read = current memory. Auxiliary (VDP/video memory) address-space value-BPs
  have no value injection in that write path yet, so they *honestly reject rather than silently ignore* (the proper fix is follow-up). **SS write/read BPs
  auto-convert the memory_type (`workraml`/`workramh`/`scspram`/`vdp1vram`/`vdp2vram`/`cram`) to SH-2 external-bus addresses**
  before firing (a non-convertible type is `unsupported` — no accept-but-never-fire; accesses that go only through the cache-through 0x2x
  mirror are uncovered). PCE supports `cpu` logical,
  `physical` 21-bit physical, and `vram0/vram1` VDC AUX BPs. MD access BPs, in addition to `cpu`/`ram`/`zram`,
  support `vram`/`cram`/`vsram`/`vdpreg` write BPs via a VDP write hook. VDP read BPs are not yet supported.
  MD `ram` BP is mapped to CPU address 0xFF0000)/
  `disassemble` (SH-2/MIPS/HuC6280/68000)/
  `find_pattern` (pattern search in 128KB units inside the debugger address space)/
  `reset`/`set_input` · `press_buttons` (controller injection — overwrites `mednafen.cpp`'s PortData[0] with the button
  mask, active-high; tap/tap_sequence/hold_until are assembled in Rust from set_input+step)/
  `screenshot` (right after MDFNI_Emulate, PNG-encode espec.surface via `PNGWrite` → base64)/
  `dump_memory` (bulk export the debugger AddressSpace as `.bin`+`regions.json` — a synthetic full-bus [PSX cpu 4GB ·
  SS physical 128MB] is skipped at a 64MB cap [reported in `reply.skipped`]; dedicated RAM/VRAM is exported)/ **Saturn only**:
  `get_video_state` (VDP2 per-NBG decode — effective charno bit width · cell size · bpp · derived cellbytes · plane (name table)
  base absolute address · scroll · BGON, per-field raw+reg_offset; the game's char-base correction constant is not applied = the agent's RE job) ·
  `resolve_tile(nbg,x,y)` (screen coordinate → char data address, resolved per-tile, with intermediate charno · nt_addr · raw PND included)/
  `set_layer_enable` (layer toggle — exposes Mednafen's built-in `MDFNI_SetLayerEnableMask`. Enable only `layers` [name array ·
  case-insensitive] and disable the rest, or `mask` [raw]; when omitted, query. Per-system exposure via `MDFNGameInfo->LayerNames`:
  SS `NBG0/NBG1/NBG2/NBG3/RBG0/RBG1/Sprite`, MD/PCE each their own; PSX has no LayerNames, so
  `unsupported`. Unknown name → `bad_params`, `layers:[]` = query (disable all is `mask:0`). The mask persists until
  changed — restore to enable-all after analysis. For VDP1/VDP2 routing · clean-plate determination).
- **Supported (added from previously unsupported)**: `dump_memory` (dedicated RAM/VRAM with no cap, synthetic full-bus with a 64MB cap-skip),
  `get_rom_info` (name/path/size/media_type from EMUCAP_CONTENT + **content_md5** (`MDFNGameInfo->MD5` — disc
  layout-aware · path-independent, recommended as rom_sha1) + sha1 (file)), `step_instructions` (instruction-granularity advance via
  the fork's per-instruction CPU callback; SS is 1 instruction of the active CPU).
- **Unsupported (Mesen-only — not in this adapter, error kind `unknown_method`)**:
  `watch_register` · `set_trace`/`get_trace`/`call_stack` · `break_on_reset`, and set_breakpoint's kind `nmi`/`irq`/`dma`.
  **Behavior differences**: `get_state`'s `groups` filter is ignored (always returns everything), input `port` is ignored (always port0), `save_state`/
  `load_state` work both frozen and running (Mesen only running), `poll_events` returns at minimum `{pc}` and access
  BPs also carry `{kind,address,length,value}` where possible. MD VDP write BP events also carry `memory_type`,
  `source` (`data_port`/`dma_vbus`/`dma_fill`/`dma_copy`/`control_port`), and for the DMA family a `source_address`
  (no Mesen-style type/channels/dma snapshot, etc.).
- **PCE status**: added the `pce` core build/branch/button/value-conditioned BP recording paths. The synthetic HuCard smoke is
  verified with `cargo run --example mednafen_pce_smoke`. Real-game input is checked with
  `cargo run --example mednafen_pce_input_visual -- <game.cue|game.pce>`. Default verification order:
  `status.system=="pce"` → HuC6280/VDC groups exposed in `get_state` → `read_memory("cpu", 0xE060, ...)`
  → `disassemble(0xE060)` → `tap(["run"])` or `tap(["start"])`.
- **MD status**: added the `md` core build/branch/button/value-conditioned BP recording paths. Default verification order:
  `status.system=="md"` → check the SEGA header with `read_memory("cpu", 0x100, ...)` →
  read the reset vector and `disassemble(reset_pc)` → `write_memory("ram", ...)` round-trip →
  check the runtime surface with `read_memory("vram"/"cram"/"vsram"/"vdpreg"/"zram", ...)` →
  `write_memory("zram"/"vram"/"cram"/"vsram", ...)` round-trip · restore →
  check VDP port/DMA write events with `set_breakpoint(kind="write", memory_type="vram"/"vdpreg", ...)` →
  after `tap(["start"])`/`press_buttons(["start"])`, check `status.last_game_buttons` and screen changes.
- **PSX proof (Waku Puyo Dungeon, JP)**: verified everything — status · get_state (CPU/SPU/timer) · disassemble (MIPS, cross-checked
  against raw bytes) · read_memory (cpu/ram, KSEG mirror folding · LE) · write_memory · save/load_state round-trip ·
  screenshot · input injection (running the menu to completion from title → start → DATA SELECT → new game → character select) · exec/write BP
  freeze.
- **Input injection point**: injected not at the driver's `Input_Update` but in the core-agnostic `mednafen.cpp`, right
  before Emulate (same phase as movie/netplay) and at MidSync. An Input_Update injection can be out of phase with when
  the game reads the input snapshot (the Saturn SMPC INTBACK path). PSX has no SMPC, so the game reads PortData directly
  every frame, meaning this injection drives the menu as-is.
- **Input visibility**: the injection state (engaged · mask) is written by the `emucap_service` thread and read by the apply
  hook (the main thread), so it must be `std::atomic` — a plain variable has no visibility and input oscillates between
  no-input ↔ input. status's `last_game_input` exposes the active-high bits that reached the pad latch.
  For Saturn the next stage is the SMPC, so also look at `last_smpc_read_addr`/`last_smpc_read_value`/
  `smpc_read_mask`/`last_smpc_oreg`. Distinguish OREG `0x10..0x2f` reads from direct-port `0x3a/0x3b`
  reads to separate "pad latch" from "game-visible read".
- **ROM reload**: for a rebuilt disc, **restart the fork** (kill, then re-run with the new disc →
  auto-reconnects to emucap-mcp). In-process reload is a non-goal for driver-threading reasons.
- **Follow-up**: a proper JSON parser (currently minimal extraction).

## broker multi-instance note

To attach several copies of the same fork to one broker (multi-session isolation), Mednafen blocks concurrent
execution from the same base directory with a lockfile, so `MEDNAFEN_ALLOWMULTI=1` is required (or separate the
base dir per instance). `launch.sh` already sets `MEDNAFEN_ALLOWMULTI=1` by default, so it is automatic on the
launch.sh path; the below is for running `<fork>` directly. Distinguish sessions with `EMUCAP_NAME`. Example:
```
MEDNAFEN_ALLOWMULTI=1 EMUCAP_PORT=47800 EMUCAP_NAME=g1 <fork> -sound 0 <game>
MEDNAFEN_ALLOWMULTI=1 EMUCAP_PORT=47800 EMUCAP_NAME=g2 <fork> -sound 0 <game>
```
