# emucap — Mesen2 adapter

## For the agent — you are the user's interface

The user is likely not a developer and will not read this file — you read it and walk them through each
step. Do the terminal/technical work yourself. For any GUI step, tell the user exactly which menu to
click and where it is (e.g. "the menu bar along the very top of the window"), the exact button/checkbox
label in quotes, and confirm they did it before moving on. The steps below assume Windows; only mention
the macOS/Linux difference if that is the user's system.

## 1. Install Mesen2

- **Windows**: open the maintained MesenCE releases page,
  <https://github.com/nesdev-org/MesenCE/releases>, and download `Mesen_<version>_Windows.zip`.
  Extract it — inside is a single file, `Mesen.exe` (there is no installer to run). Double-click
  `Mesen.exe`. On the first launch it shows a "Select Data Storage Folder" dialog; either option is
  fine — accept it.
- **macOS/Linux**: download the matching build from the same Releases page.

## 2. One-time setup — let the adapter script save and connect

The adapter is a Lua script that Mesen runs. Mesen blocks scripts from disk and network by default, so
two checkboxes must be turned on once (Mesen remembers them — only needed the first time):

1. Load any SNES ROM: top menu bar → **File → Open**, pick the ROM file. (The next step stays greyed
   out until a game is loaded.)
2. Top menu bar → **Debug → Script Window** (shortcut Ctrl+N). A new window opens.
3. In that Script Window's own menu bar (inside the window) → **Script → Settings**. A "Debugger
   Settings" window opens on the "Script Window" tab.
4. Under the **Restrictions** heading: first check **"Allow access to I/O and OS functions"** — that
   makes **"Allow network access"** become clickable, so check it too.
5. Click **OK**.

## 3. Load and run the adapter

In the Script Window → **File → Open** (Ctrl+O) → choose `adapters/mesen2/emucap-snes.lua` (live agent
control for SNES; `emucap-sms.lua` for Game Gear / Master System, `emucap-gb.lua` for Game Boy / GBC,
`emucap-gba.lua` for GBA, `emucap-nes.lua` for NES) or `emucap.lua` (retrospective-bundle capture). The
live entries share `adapters/mesen2/emucap-core.lua`; `emucap-snes.lua` adds the 65816 decoder,
`emucap-sms.lua` the Z80 one, `emucap-gb.lua` the SM83 one, `emucap-gba.lua` the ARM7 one, `emucap-nes.lua`
the 6502 one, and the entry you load selects the system. It runs automatically;
the log pane at the bottom should print `emucap: ROM 경로 = …` with no I/O warning. (To re-run manually:
**Script → Run Script**, F5.)

The ROM path is auto-inferred via `getRomInfo`; if inference is off, fix the `ROM_PATH` fallback at the
top of `emucap.lua`, or override it when finalizing with `emucap finalize --rom`.

## Launch internals & macOS caveats

### Launching — crash-cascade caveat

If a crashed or stuck Mesen is left around, the next Mesen can crash-cascade
(Avalonia RenderTimer -6661). Also, if the macOS "quit unexpectedly" dialog is up
after a crash, new launches are blocked until it is dismissed.

- ⚠ **Do not broadly kill** leftover instances (`pkill -i mesen` / `killall Mesen`
  kill the Mesen of other sessions). Leave cleanup to `launch.sh` — it cleans up
  only the orphan instance on that port, and refuses if a connected instance exists.

Recommended path: call the MCP `launch` tool. It copies Mesen into an emucap-owned portable directory,
applies required options without modifying the user's default settings, and leaves native key mappings
available. GBA alone creates a minimal portable `settings.json` so the staged BIOS is discoverable.
The legacy `adapters/mesen2/launch.sh <ROM> <EMUCAP_PORT> [EMUCAP_NAME]` helper follows the same portable
copy rule and remains a fallback when the MCP tool is unavailable.

## Retrospective capture (emucap.lua)
- During play, pressing **Ctrl+Shift+C** drops a slice of roughly the last
  `DEPTH × INTERVAL` frames (save-states · screenshots) and the input movie into
  `bundles/<time>-retrospective/`. "EMUCAP CAPTURED" flashes briefly on screen.

## Finalize · analyze
```
emucap finalize bundles/<timestamp>-retrospective
emucap inspect  bundles/<timestamp>-retrospective
```
Give the finalized bundle directory and a short problem description to the analysis
agent or tool of your choice.

## Tuning
Adjust `INTERVAL` (sample interval), `DEPTH` (ring depth), and `TRIGGER_KEYS` (key
combo) at the top of the script.

## Live MCP mode — agent operation

A separate entry script `emucap-snes.lua` (SNES; `emucap-sms.lua` for Game Gear, `emucap-gb.lua` for Game Boy / GBC, `emucap-gba.lua` for GBA) lets the agent read and control the running game.
The MCP server `emucap-mcp` comes up over stdio, and the Lua connects to that server's
TCP port (default 47800).

After a disconnect, the adapter accepts a replacement same-session connection without restarting
Mesen. Unfinished request IDs and transient presses belong to the dead connection and are canceled;
execution state, breakpoints, and explicit `set_input` holds remain. A timeout alone is not proof
that Mesen exited, so reconnect and query `status` before launching another process.

- Read: `read_memory`/`find_pattern` (byte-pattern search — direct region scan,
  matching offsets only)/`screenshot`/`get_state`/`get_rom_info`/`status`.
- Active: `write_memory`/`set_input`/`press_buttons`/`tap`/`tap_sequence`/`hold_until`/`save_state`/`load_state`/
  `run_frames`/`pause`/`step`/`step_instructions`/`resume`/`reset`/`probe`.
  (⚠ save_state/load_state work **only in running** (frozen is rejected — an exec
  callback context is required). A set_input hold persists until explicitly released
  with an empty set_input (resume/step do not release it).)
- Breakpoints · tracing: `set_breakpoint` (kind **exec/read/write/nmi/irq/dma**;
  pc_min/pc_max conditions, **value/value_mask/value_len value-conditions**; a write BP
  includes the $2118/$2119→**vram_addr** · $2122→cgram_addr · $2104→oam_addr destination
  in the event) · `clear_breakpoint`/`list_breakpoints`/`clear_all_breakpoints`/`poll_events` ·
  `watch_register` · `set_trace`/`get_trace`/`call_stack` · `break_on_reset`.
- Disassembly: `disassemble(address, count)` → `[{addr,text,bytes}]`. Mesen2 Lua has no
  disassembly API, so a 65816 decoder is implemented directly in the adapter (M/X flags
  start from `cpu.ps` and track REP/SEP).
- Analysis: `dump_memory`/`bisect`/`regression_run`.
- `verify_determinism` — measures reproducibility by replaying a reproduction recipe N
  times and matching hashes (determinism_replay gate).
- **Note**: of the above, `tap`/`tap_sequence`/`hold_until`/`step_instructions`/`bisect`/`regression_run`/
  `verify_determinism` are not adapter-native — the MCP server (`emucap-mcp`) synthesizes
  them from primitive tools (set_input · step · read_memory, etc.). The native methods the
  adapter advertises directly are canonically listed in `hello.methods`.

### The agent launches Mesen

Get the port from `status`'s `listening_port` — never hardcode 47800. Prefer the MCP `launch` tool:

```json
{"content_path": "/path/to/game.sfc", "system": "snes", "name": "snes_session"}
```

For Game Gear (or Master System), launch a `.gg` / `.sms` file with `system: "gamegear"`:

```json
{"content_path": "/path/to/game.gg", "system": "gamegear", "name": "gg_session"}
```

For Game Boy / Game Boy Color, launch a `.gb` / `.gbc` file with `system: "gb"` (or `"gbc"`); for GBA,
launch a `.gba` file with `system: "gba"`; for NES, launch a `.nes` file with `system: "nes"`:

```json
{"content_path": "/path/to/game.gb", "system": "gb", "name": "gb_session"}
{"content_path": "/path/to/game.gba", "system": "gba", "name": "gba_session"}
{"content_path": "/path/to/game.nes", "system": "nes", "name": "nes_session"}
```

The launcher uses an emucap-owned portable Mesen copy under `EMUCAP_EMU_HOME` or the OS default emucap
data root. The Rust MCP launcher applies required options on the command line and creates a local
`settings.json` only for GBA firmware discovery; legacy fallback launchers write their settings beside
their own portable copy. Pidfiles and logs stay under the per-port directory unless `EMUCAP_LOG`
overrides the log path.

**macOS / Linux fallback** — use `launch.sh` only when the MCP `launch` tool is unavailable:

```bash
REPO=/path/to/emu-monitor-hitl-adaptor
"$REPO/adapters/mesen2/launch.sh" "/path/to/game.sfc" <listening_port> [name]
# launch.sh prints "연결됨" (connected) and returns only after it confirms the TCP
# connection (ESTABLISHED + post-connect grace) — no separate sleep is needed.
```

`launch.sh` checks `MESEN_BIN`, the macOS app default, then PATH (`Mesen`/`mesen`) and still runs an
emucap-owned portable copy rather than the source install in place.

**Windows fallback** — use **`launch.ps1`** only when the MCP `launch` tool is unavailable. It copies
`Mesen.exe` into `%LOCALAPPDATA%\emucap\mesen2\<port>\portable`, writes settings there, and launches
that copy. It refuses to start unless the MCP listener is already on `<listening_port>`, refuses a port
that already has an emulator connection, writes `mesen.pid`/`mesen.log` under the per-port directory,
and returns only after Mesen connects. It checks `MESEN_BIN`, common user/program-files install paths,
then PATH. Set `MESEN_BIN` to the path of `Mesen.exe` when needed.
The script reads the MCP session token from the OS temp directory when `EMUCAP_SESSION_TOKEN` is not
already set.

```powershell
$env:MESEN_BIN = "C:\path\to\Mesen.exe"
powershell -ExecutionPolicy Bypass -File "<repo>\adapters\mesen2\launch.ps1" "C:\path\to\game.sfc" <listening_port> [name]
```

- The agent knows the ROM path (the user tells it, or it is a build-output path).
- If `launch.sh` reports "no MCP listener", do not relaunch the emulator — call `status`
  again first. A log that looks like a shutdown right after renderer/video init may just
  be the launcher timeout cleaning up with SIGTERM.
- If no new Mesen window appears on macOS, or launch.sh fails right after "연결됨",
  first suspect a blocked macOS dialog or display-sleep renderer failure. The fallback launcher defaults
  to direct execution of the portable copy and uses `caffeinate` when available. If it still recurs,
  check the Mesen window/dialog directly and relaunch.
- To let a human freeze a transient moment (a sprite popup, etc.) on the spot, press the
  **freeze hotkey `Home`** in the Mesen window (change with `EMUCAP_FREEZE_KEY`; the same
  key toggles resume) — it is a codeBreak freeze, so emucap freezes indefinitely while
  keeping responses alive (`status.reason="hotkey"`). ⚠ **Do not use Mesen's GUI Pause** —
  it drops the connection to 'not connected' and does not recover until you resume from the GUI.
- Environment variables: `MESEN_BIN` (path to the source Mesen executable or macOS app bundle; when
  unset, the launcher checks common install paths and PATH), `EMUCAP_EMU_HOME` (portable copy root),
  `EMUCAP_LAUNCH_WAIT` (seconds to wait for connection, default 20),
  `EMUCAP_POST_CONNECT_GRACE` (grace seconds after connection, default 2), `EMUCAP_LOG`
  (log path), `EMUCAP_DEADMAN_MS` (operator opt-in idle auto-resume; default 0 = disabled),
  `EMUCAP_RECONNECT_GIVEUP_MS` (operator opt-in auto-resume after MCP disconnect; default 0 =
  wait indefinitely). `status.freeze_policy` reports the effective values. Mesen's Lua API still
  requires a one-instruction watchdog rearm roughly every 800 ms, so this is a treadmill freeze,
  not a zero-drift CPU halt; use breakpoint `snapshot` for exact hit-time evidence.
- `EMUCAP_PREARM` pre-arms a DMA write BP right after cold boot (form `dma` | `dma:<dest>` |
  `dma:<dest>:<vmin>-<vmax>`). When an agent round-trip cannot catch a DMA write that
  vanishes in an instant during boot (e.g. initialization before the attract), arm it ahead
  at launch time so it freezes on the first hit.

### Game Gear (and Master System) — Z80

Game Gear runs on the same adapter through the `emucap-sms.lua` entry (a Z80 core; the SMS core also
handles `.sms`). Launch with `system: "gamegear"` and a `.gg` / `.sms` content path. The tool set is
identical to SNES — only the ISA, memory types, and button names differ:

- **ISA**: Z80. `disassemble`, `call_stack`, and `get_state` are Z80 (SNES uses 65816).
- **Buttons** (`status.input_buttons`): `up` / `down` / `left` / `right` / `one` / `two` / `pause`.
  `one` = Button 1 (B), `two` = Button 2 (A), `pause` = Start. Aliases: `start→pause`, `a→two`,
  `b→one`, `1→one`, `2→two`.
- **memory_types**: `smsWorkRam` (8KB WRAM, CPU bus 0xC000+), `smsMemory` (full Z80 bus), `smsCartRam`,
  `smsPrgRom`, `smsVideoRam`, `smsPaletteRam`, `smsPort`, `smsBootRom`, `smsDebug`.
  `status.memory_types` is authoritative.
- **BP address conversion**: a read/write BP given `smsWorkRam` (an offset) fires on the CPU bus
  address (0xC000 + offset) after adapter translation; an exec BP takes `smsMemory` (a Z80 bus
  address). The Mesen-only `nmi` / `irq` / `dma` BP kinds apply here too.
- **VRAM write BP (`smsVideoRam`)**: VDP VRAM is not on the Z80 bus (it is written through the data
  port `OUT $BE`), so a plain memory-write callback never sees it. The adapter reconstructs it — a
  `write` BP on `smsVideoRam` runs a per-instruction exec callback that detects the VDP data-port write
  (`OUT $BE` and the `OTIR`/`OUTI`/`OUTD`/`OTDR` block forms, port in `C`) and reads the VDP
  `addressReg`/`codeReg` to recover the destination VRAM word address (the response carries
  `mechanism: "vdp_write_reconstruction"`). It is a hunting tool: per-instruction, with an instruction
  budget + auto-disarm, so pair it with `pause_on_hit` and clear it when done. Write BPs on other
  non-bus memtypes (`smsPaletteRam` / CRAM) return an `unsupported` error rather than silently
  never firing (CRAM reconstruction is not supported).
- **ROM bank tagging**: the Z80 bus is 16-bit and ROM is paged into three 16 KB slots by the Sega
  mapper, so a bare pc does not say which bank ran. `call_stack` frames (`{pc, bank}`), `get_trace`
  entries, and breakpoint-hit events carry the ROM `bank` — the slot bank from `get_state`'s
  `cart.prgBanks0/1/2` (slot = `addr >> 14`; the fixed first 1 KB is bank 0). The bank is captured when
  the code ran, and `status.bank_tagging` reports it per cart (true only when the cart exposes those
  fields). A `null`/absent `bank` means undetermined. Only the standard Sega mapper is covered —
  non-Sega mappers (Codemasters / Korean) and code executing from slot-2 cart RAM may report a wrong or
  absent bank, so trust `bank` on standard-mapper carts.

### Game Boy / Game Boy Color — SM83

Game Boy and Game Boy Color both run through the `emucap-gb.lua` entry (an SM83 core; Mesen handles GB
and GBC as one `gameboy` core, the way `emucap-sms.lua` covers both Master System and Game Gear).
Launch with `system: "gb"` (or `"gbc"`) and a `.gb` / `.gbc` content path. No BIOS is required. The
tool set is identical to SNES — only the ISA, memory types, and button names differ:

- **ISA**: SM83. `disassemble`, `call_stack`, and `get_state` are SM83 (SNES uses 65816).
- **Buttons** (`status.input_buttons`): `a` / `b` / `start` / `select` / `up` / `down` / `left` / `right`.
- **memory_types**: `gameboyMemory` (full SM83 bus), `gbWorkRam`, `gbVideoRam`, `gbCartRam`,
  `gbHighRam`, `gbPrgRom`, `gbSpriteRam`, `gbBootRom`. `status.memory_types` is authoritative.
- **ROM bank tagging**: the SM83 bus is 16-bit; 0x4000-0x7FFF is a switchable MBC bank
  (`get_state`'s `cart.prgBank`) and 0x0000-0x3FFF is bank 0. `call_stack` frames (`{pc, bank}`),
  `get_trace` entries, and breakpoint-hit events carry the `bank`, captured when the code ran;
  `status.bank_tagging` reports it per cart. MBC1 mode-1 / MBC1M remap the low region and Mesen exposes
  no resolved low bank, so those report `bank: null` (undetermined) rather than a wrong bank 0.

### Game Boy Advance — ARM7

GBA runs through the `emucap-gba.lua` entry (an ARM7TDMI core). Launch with `system: "gba"` and a
`.gba` content path. The tool set matches SNES for memory / state / input / breakpoints / save-states,
with these differences:

- **BIOS required**: Mesen needs a real GBA BIOS (`gba_bios.bin`, not committed to the repo). Without
  it Mesen shows a firmware prompt. The launcher provisions it headlessly from `EMUCAP_GBA_BIOS` (env)
  or the emucap firmware directory (`<emucap-data>/firmware/gba_bios.bin`) — the same pattern as the
  PSX BIOS. GB / GBC need no BIOS.
- **Buttons** (`status.input_buttons`): `a` / `b` / `l` / `r` / `start` / `select` / `up` / `down` /
  `left` / `right`.
- **memory_types**: `gbaMemory` (full ARM7 bus), `gbaIntWorkRam`, `gbaExtWorkRam`, `gbaVideoRam`,
  `gbaPaletteRam`, `gbaSpriteRam`, `gbaSaveRam`, `gbaPrgRom`, `gbaBootRom`. `status.memory_types` is
  authoritative.
- **`disassemble` supported; `call_stack` not implemented yet**: the ARM7 decoder handles ARM and
  Thumb instructions including `SUBS PC,LR,#4`, PUSH/POP, scaled-register `LDR`, and `MRS SPSR`.
  `call_stack` is not built yet — ARM's
  LR-based return does not fit the core's SP-based call-stack model, so `call_stack` is not advertised.
  Everything else (read/write_memory, get_state,
  step / step_instructions, breakpoints, screenshot, input, save/load_state) works as on SNES.
  `status.methods` is authoritative.

### NES (Nintendo Entertainment System / Famicom) — 6502

NES runs through the `emucap-nes.lua` entry (a 6502 / 2A03 core). Launch with `system: "nes"` (aliases
`nintendo` / `famicom` / `fc`) and a `.nes` content path. No BIOS is required. The tool set is identical
to SNES — only the ISA, memory types, and button names differ:

- **ISA**: 6502 (2A03). `disassemble`, `call_stack`, and `get_state` are 6502 (SNES uses 65816). The
  6502's `JSR` / `RTS` fit the core's SP-based call-stack model, so `call_stack` is supported.
- **Buttons** (`status.input_buttons`): `a` / `b` / `start` / `select` / `up` / `down` / `left` /
  `right` (standard NES controller; no X/Y/L/R). Aliases: `enter` / `return` → `start`.
- **memory_types**: `nesMemory` (full 6502 bus / default), `nesInternalRam` (2KB @ $0000), `nesWorkRam`,
  `nesSaveRam` (PRG-RAM @ $6000), `nesPrgRom`, `nesChrRom`, `nesChrRam`, `nesNametableRam`,
  `nesPaletteRam`, `nesSpriteRam` (OAM), `nesSecondarySpriteRam`, `nesMapperRam`.
  `status.memory_types` is authoritative.

### Verify the connection
Call the `status` tool → `{"connected":true,"frame":…,"state":"running"}` means it is
ready. The first call right after boot may return `emulator not connected`, so retry a few
seconds later. The MCP server binds lazily, so even before Mesen exists, tool calls respond
gracefully with "not connected".

### (Alternative) Load via the GUI
If Mesen is already up, you can also load `emucap-snes.lua` (or `emucap-sms.lua` for Game Gear, `emucap-gb.lua` for Game Boy / GBC, `emucap-gba.lua` for GBA, `emucap-nes.lua` for NES) from Debug → Script Window.

Server and client match ports via `EMUCAP_PORT`.

## Cross-ROM diff (original vs patched)

Find what the patch broke — drive both ROMs to the same logical moment and compare state.

1. Bring up two emucap-mcp sessions and launch two instances via launch.sh, each with its
   session's `status` `listening_port`:
   - `launch.sh "<JP.sfc>" <portA> emucap-a`
   - `launch.sh "<KR.sfc>" <portB> emucap-b`
   - Use the port each session's status reports (never hardcode). For a single-session
     sequential run, use broker mode.
2. **Align**: `set_breakpoint(..., pause_on_hit=true)` at the *same game-logic address* in
   both instances. Advancing both makes each freeze at that event (aligned by logic, not
   frame count — robust even if the patch changes timing). A text patch does not change
   logic addresses, so both hit the same BP.
3. **Dump**: while frozen, `dump_memory(dirA)` · `dump_memory(dirB)` (memory + state.json).
4. **Compare**: `emucap diff dirA dirB`.
   - Differences the patch changed intentionally (translated text · fonts) show up all over.
     To separate those out:
     - **Baseline subtraction**: at a good point,
       `emucap diff A_good B_good --json > baseline.json`; at the bug point,
       `emucap diff A_bug B_bug --baseline baseline.json` → only new differences.
     - **State diff**: registers/DMA/PPU should not be touched by a text patch → a difference
       there is a bug signal. Add noise keys to exclude with `--ignore-key`.

## Caveat
The call context of `createSavestate` and the return keys of `getInput` may differ by Mesen2
version. On first use, confirm the behavior empirically before relying on it.
