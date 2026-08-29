# emucap — emulator monitor + HITL adaptor

> Korean guide: [README.ko.md](README.ko.md)

MCP infrastructure for debugging retro-game patches. An AI agent reads and
controls a running emulator's memory, state, and screen so it can analyze a
problem a human described in plain language. A common Core plus per-emulator
adapters supports several emulators — Mesen2 (SNES · Game Gear · Game Boy · GBC ·
GBA · NES), a Mednafen fork
(Saturn · PlayStation · PC Engine · PC-FX · Mega Drive/Genesis · WonderSwan/WSC ·
Neo Geo Pocket/Color), Flycast
(Dreamcast), a DeSmuME fork (Nintendo DS), a PPSSPP fork (PSP), a PCSX2 fork
(PlayStation 2), a Dolphin fork (GameCube · Wii), MAME plus an optional direct
NP2kai compatibility backend (PC-98), MAME (experimental Neo Geo
MVS/AES/CD), and an experimental Mupen64Plus frontend (Nintendo 64).
Stock openMSX 21.0 provides experimental C-BIOS MSX2+ and real-firmware
MSX1/MSX2/MSX2+ cartridge profiles through a separate Rust XML-control bridge.

**v0.15.0 — beta.** This repository remains under active development; interfaces and
behavior may change in later releases. Adapter availability is host-dependent and is
reported by `status`.

Licensed under GPL-2.0-or-later. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Platforms

The Rust core (the two MCPs) and the Rust `launch` tool are cross-platform (macOS
on Apple Silicon + Intel, Linux, Windows). Per-emulator build/launch requirements
vary by OS — where automation falls short the agent installs the emulator from
upstream instructions and points emucap at it, and `status` reports which tools
are actually available on the host. On Windows, prefer the Rust `launch` tool and
documented env overrides over the Unix shell launchers.

## Let the agent do the install

This repository is built so that **an agent (Claude Code, Codex, …) performs the
install itself**. A non-developer can hand the agent the repo and say:

> "Follow this repo's README 'Agent install steps' to build emucap and register
> it as MCP servers."

The agent runs the steps below in order. The Core build is light; per-emulator
adapters are built only when needed.

**You (the agent) are the user's interface.** Assume the user is not a developer, may not be
comfortable with a terminal or even with installing desktop programs, and will not read this file — you
read it and do the work. Run the terminal steps yourself. When a step needs the user to click something
in a GUI (for example the Mesen2 setup), walk them through it one action at a time: name the menu and
where it is ("the menu bar along the top"), quote the exact button/checkbox label, and confirm they did
it before moving on. Adapt to the user's OS — this guide's shell commands are Unix-style; on Windows
use the equivalents (and see the Platforms note above).

### 1. Prerequisites (the agent checks, and installs if missing)

- **Rust 1.88 or newer** — check with `command -v cargo` and `rustc --version`. If missing:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"`
- **C compiler** (to build the bundled SQLite) — macOS: `xcode-select -p || xcode-select --install`.
  Linux: `cc --version || sudo apt-get install -y build-essential`. Windows: install the MSVC C++
  build tools (the Rust installer may prompt for this); then build from a normal PowerShell.
- **git**.

### 2. Build the Core

From the repo root:

```sh
cargo build --release \
  --bin emucap --bin emucap-mcp --bin emucap-track-mcp --bin emucap-broker \
  --bin emucap-mame-pc98-bridge --bin emucap-mame-neogeo-bridge \
  --bin emucap-mupen64plus --bin emucap-desmume-nds-bridge \
  --bin emucap-ppsspp-bridge --bin emucap-pcsx2-bridge \
  --bin emucap-openmsx-bridge --bin emucap-np2kai
```

Outputs: `target/release/emucap-mcp` (**Control MCP** — drives the emulator),
`emucap-track-mcp` (**Tracking MCP** — experiment ledger, emulator-less),
`emucap` (case-bundle CLI), `emucap-broker` (multi-session broker),
`emucap-mame-pc98-bridge` (PC-98 launch helper),
`emucap-mame-neogeo-bridge` (Neo Geo MVS/AES/CD launch helper),
`emucap-mupen64plus` (N64 frontend and adapter),
`emucap-desmume-nds-bridge` (NDS launch helper),
`emucap-ppsspp-bridge` (PSP launch helper),
`emucap-pcsx2-bridge` (PS2 launch helper), and
`emucap-openmsx-bridge` (stock openMSX XML-control helper), and
`emucap-np2kai` (direct PC-98 compatibility frontend). All dependencies come from
crates.io and SQLite is bundled, so **nothing beyond Rust and a C compiler is
required** for a source build. The first build is slower while dependencies
download; later builds are fast.

### 3. Register the MCP servers (two of them)

emucap is split into **two MCPs, and you register both** — the agent composes
them (see §2b).

- **Control MCP** (`emucap-mcp`) — the emulator-driving engine. Reads memory,
  state, and screen; controls input, save-states, and breakpoints; and returns
  results from optional analysis operations. Its static tool list is a compact
  basic remote. Open persistent/device input with
  `input_control(operation="describe")`, composite or device-specific debugger operations with
  `debug(operation="describe")`, and reproducibility analysis with
  `analysis(operation="describe")`. Each drawer returns only the current
  runtime's operations and schemas; execute through that same tool. Core
  debugger primitives—memory write, disassembly, call stacks, breakpoints, and event
  polling—remain direct tools. Live media changes are also direct and require a
  frozen guest plus a device ID from `status.media_devices`. Exact guest-time
  advance uses `step`, which returns the guest frozen; adapter free-running frame waits are
  compatibility wire operations and are not exposed to MCP agents.
  Exact bounded button input uses direct `tap`, which releases input and returns frozen. A
  real-time pulse that leaves the guest running is available only as the explicitly named
  `pulse_while_running` operation in the input drawer when the runtime supports it.
- **Tracking MCP** (`emucap-track-mcp`) — the experiment ledger (`.emucap/`).
  Starts (`run_start`), records (`log_*`), and queries (`query_runs` /
  `compare_runs` / `summarize_runs`) runs. It **knows nothing about emulators**
  (emulator-less). It is an add-on layered on the Control MCP, so the Control MCP
  works fine without it.

**Claude Code:**

```sh
claude mcp add emucap-control -- "$(pwd)/target/release/emucap-mcp"
claude mcp add emucap-track   -- "$(pwd)/target/release/emucap-track-mcp"
```

**Codex**:

```sh
tools/register-codex-mcp.sh
```

On Windows, run `tools/register-codex-mcp.ps1` in PowerShell. The scripts use the
source-build binaries from `target/release/` and register `emucap` plus
`emucap-track`.

Tune with environment variables as needed: `EMUCAP_PORT` (Control MCP, default
47800, auto-advances to the next port if taken), `EMUCAP_TRACK_ROOT` (the
Tracking MCP's ledger location, default `.emucap` at the working repo's git root).

After registering, reconnect the agent session (`/mcp`). Since **each MCP exposes
its own `bootstrap`**, success means both the Control MCP's `bootstrap` (emulator
entry) and the Tracking MCP's `bootstrap` (ledger entry) appear in the tool list.
If they don't, rebuild the release and reconnect — the MCP servers run the
release binary, so debug builds are not picked up.

### 3b. Three tiers, composed by the agent

Three tiers work together but stay independent (analogy: ② Tracking MCP is
MLflow, ① Control MCP is TensorFlow):

1. **Emulator control** (Control MCP) — a domain-agnostic live-control engine.
   Complete on its own (you can debug without any tracking).
2. **Experiment management** (Tracking MCP) — an add-on. It *need not know* about
   ①; it layers on top to record and query experiments.
3. **Application / methodology** (e.g. a localization-patch skill) — the top tier
   that *composes* ① and ②. **This slot is replaceable** (localization, fan
   games, AI TAS — whatever sits here reuses the two tiers below unchanged).

The two MCPs never call each other — **the agent composes them**:

- **Pass rom_sha1**: read the opaque Tracking identifier via the Control MCP's
  `get_rom_info` (`.rom_sha1`) and pass it unchanged to the Tracking MCP's
  `run_start(rom_sha1=…)`. Managed descriptor media bind a pre-launch `content_identity` to the
  entry and every loader-declared member before execution; the compatibility `rom_sha1` slot carries
  that SHA-256 identity. Never identify composite media by hashing only its descriptor. Passing
  `connection_ref` (the Control MCP `status` connection name, or `"port:N"`)
  auto-finalizes the previous unfinished run on that connection.
- **Keep returned identities opaque**: pass `rom_sha1`, `run_id`, and `finding_id`
  back unchanged. Managed identifiers contain only ASCII letters and digits joined by
  single hyphens; path syntax, dots, underscores, and platform-reserved device names are
  rejected before filesystem access.
- **Analysis verbs only return**: call `analysis(operation="describe")` when
  `regression_run` / `verify_determinism` are needed, then execute the selected
  operation through that same tool. Analysis drives the emulator and *returns*
  a result without writing to the ledger. To record it, log the result via the
  Tracking MCP's `log_gate` / `log_metric`.
- **Frame-boundary search composes the debug `probe` operation**: binary-search
  the frame range with repeated atomic probes. Each call restores the same base state,
  advances, and reads the predicate without an externally visible gap.
- **Interventions are logged explicitly**: state changes like `write_memory` /
  `load_state` / `reset` / input are not recorded automatically, so log them via
  the Tracking MCP's `log_intervention` to preserve reproduction fidelity.

### 4. First run (the agent starts with bootstrap)

Every emucap task starts with `bootstrap`. Ask the agent to "call emucap
`bootstrap`", and its compact response returns `listener.port`, system IDs, a
catalog revision, and questions about what to bring up. The full routing catalog
is available through `bootstrap(include=["systems"])`; build and runtime paths
through `bootstrap(include=["installation"])`. The compact response includes a live managed-runtime
count; request `bootstrap(include=["runtimes"])` only when continuing an existing generation. An
available entry supplies the exact arguments for `reattach`, so a client without a stable host
session ID can resume a returned lease without editing runtime files. Then
`launch_plan(content_path, system?)` returns the validated MCP `launch` tool
arguments. The agent calls `launch`, which waits for adapter readiness, and
verifies the resulting live and runtime identities with `status`. Launcher
scripts are developer entry points, not an alternative managed lifecycle.

Managed CUE, GDI, CCD, TOC, and M3U entries are closed media graphs. Selecting a descriptor authorizes
only that entry. `launch_plan` returns `review_input` with the exact relative member names before it
reads their metadata, content, or hashes; review each name and echo only the server-produced
`indirect_media_approval`. Nested M3U descriptors may require another review frontier. References
must remain portable and below the entry directory and resolve to real regular files without
symlinks. Planning and launch fail before emulator startup when review or graph validation fails.
`content_identity_binding: "prelaunch"` describes the resulting identity; it is not a claim that a
mutable source path was snapshotted or that the emulator consumed every byte.

An ordinary launch can return while the guest is already running. If the first follow-up action
must not inherit network or agent delay as guest time, request `start_frozen: true`; a supported
launcher succeeds only after the adapter is connected at a frozen guest boundary. This controls
post-launch guest time, not power-on RAM, RTC, saves, or other initial conditions. A live-advertised
`execution_profile: "repeatable"` is the separate opt-in for those producer-owned conditions, and
`record_window(require_repeatable: true)` fails before reset, input, or guest advance unless the
selected recording origin is eligible.
For Mesen SNES, refreshing the isolated runtime preserves the ordinary profile's battery saves and
other portable data. Its repeatable profile uses a separate disposable portable root, so obtaining
fresh initial conditions never deletes the ordinary profile's data or the user's standard Mesen
files. Mednafen Mega Drive provides the same opt-in negotiation with a separate disposable home. It
captures a bounded guest state before the first instruction and restores it before each eligible
`reset_release` recording, so earlier execution or debugger writes in that process do not become
undeclared recording inputs. Ordinary Mednafen profiles and saves are not changed.

`listener.base_port` is only where direct-mode port search begins. It may already
belong to another live MCP session. Launchers use the assigned `listener.port`
or full-status `listening_port`. Stopping an emulator generation does not close
its owning MCP listener; that listener is released when the MCP session exits.

A full connected `status` returns `capability_revision`. Pass it back as
`known_capability_revision` on repeated checks to omit an unchanged capability
catalog while retaining current execution, continuity, generation, and
ownership state.

When that full status includes `recording_capability`, the debug `record_window`
operation can own a
finite guest-frame interval without making agent or network latency part of guest
time. Give it an existing absolute `output_root` in the current workspace and a
frame count; it returns with the emulator frozen plus a validated bundle path and
manifest hash. The live capability is the authority for event classes and limits.
Optional origin, input movie, and event-stop arguments are valid only when that exact capability
advertises them; omitting them retains the bounded next-frame behavior.
When `recording_capability.state_load` is advertised, `save_state` with
`preserve_for_recording=true` and an absolute path also carries a producer-managed
`snapshot_receipt`. A later `record_window(origin="state_load")` accepts only that
receipt's opaque `snapshot_id`, not a caller path, digest, or boundary assertion. Only a receipt
created at a proven frame boundary is eligible. Core reopens the managed bytes, revalidates their
runtime generation and hash, copies them into the bundle, and dispatches one load-and-window
transaction. The adapter restores that exact frame coordinate, owns the dense input movie, hooks,
and sink before releasing guest execution, and returns frozen. An instruction-boundary receipt
still describes a successful save but is deliberately ineligible for a frame window. Omitting
`preserve_for_recording` leaves ordinary save behavior unchanged when omitted or false, and omitting
`state_load` leaves ordinary load and recording behavior unchanged.
High-rate event classes may additionally advertise filterable integer payload fields. Optional
per-class half-open ranges narrow only the declared observation scope; excluded callbacks are not
reported as drops, while matching events retain the ordinary sequence, limit, and integrity rules.
Unadvertised fields and invalid ranges are rejected before guest mutation.
When `recording_capability.warmup` exists, `warmup_frames` keeps the producer's default transaction
classes active while delaying observation event emission until the exact guest boundary. Classes
listed in `warmup.selectable_event_scopes` may instead use an advertised scope through
`event_arming_overrides`; omission preserves the producer default. The selected scope controls the
stream and its event, byte, and drop accounting, not whether a native emulator hook remains
installed. The input movie covers both intervals in one request.
When a selected event is marked `startable`, optional `start_on` can align the observation interval
to its first occurrence. `initial_snapshots` additionally require the capability's callback-safe
memory type and bounds; Core receives them through a separate authenticated binary sink and binds
their hashes to the exact anchor event in the manifest.
Optional terminal snapshots additionally require bounds in `recording_capability.terminal_snapshots`
and an exact finite entry in `status.memory_regions`. Core validates every range before arming,
reads it only after the terminal frame is frozen, and publishes it as a hashed bundle member.
Omitting the field performs no extra memory read.
An advertised `recording_capability.terminal_state` profile may likewise be selected to preserve
one producer-defined, canonical JSON state member at the same frozen terminal boundary. Consumers
project that representation on their side; their schemas are not part of emucap.
Longer windows are explicit and capability-bounded. A short request keeps the established failure
deadline, while Core derives proportionally more host cleanup time for a longer requested window;
host time never changes the movie's guest-frame schedule.
Unsupported adapters reject the call without installing hooks or advancing the
guest. `status.recording_capture` retains a bounded active or terminal capsule
across a Control MCP restart; event bytes and private staging paths stay out of
status.

A timeout or `connected: false` reports transport state, not proof that the
emulator exited. Inspect `status.continuity.runtime_binding`,
`status.runtime_instance` or `status.stale_runtime_instance`, and
`get_failure_context` before relaunching. Automatic reattach is limited to the same stable control
identity; otherwise select an available exact generation from bootstrap runtime discovery and call
`reattach(launch_id=...)`;
use `launch(..., replace: true)` only for an intentional, identity-verified
replacement. On a Flycast fatal quarantine, read the preserved context first and
call debug operation `dismiss_failure` only when `status.methods` advertises it.

To end a managed emulator, read `status.runtime_instance.launch_id` and call
`stop(launch_id=...)`. The Control MCP verifies the current generation, control
lease, and process-start identities, then waits for the emulator and its recorded
bridge to exit. It preserves failure evidence and refuses stale, broker-owned, or
unmanaged processes instead of asking the agent to kill by process name.

## Per-emulator adapters (the agent installs when needed)

Pick one to start. MesenCE uses a local source build because live control requires its native
debugger halt to service requests without advancing the guest.

- **Mesen2 (SNES · Game Gear · Game Boy · GBC · GBA · NES)** — run
  `adapters/mesen2/build.sh` (Windows: `build.ps1`). It fetches pinned MesenCE 2.2.1 into a
  local directory excluded from version control, applies the GPLv3 patch stack, and builds locally;
  no emulator binary is distributed. Per-system Lua entries cover them (65816 for SNES,
  Z80 for Game Gear / Master System, SM83 for Game Boy / GBC, ARM7 for GBA, 6502 for
  NES). An unmodified Mesen build is rejected for live control because it lacks the patched native
  halt service and safe savestate event.
  GBA needs a real BIOS (`gba_bios.bin`, not committed); SNES / Game Gear / GB /
  GBC / NES need none. → `adapters/mesen2/README.md`
- **Mednafen (Saturn · PSX · PCE · PC-FX · MD · WonderSwan/WSC · Neo Geo Pocket/Color)** — build the fork with
  `adapters/mednafen/build.sh` (needs SDL: macOS `brew install sdl2`, Linux
  `libsdl2-dev`). Its source archive and checksum are pinned. One binary handles all seven system families.
  PSX, PCE-CD, and PC-FX need BIOS files (not committed to the repo). PC-FX requires an explicit
  version 1.00 BIOS and runs with an emucap-owned Mednafen profile. Neo Geo Pocket and
  Pocket Color share the patched `ngp` module. Its deliberately narrow TLCS-900/H debugger
  exposes side-effect-free RAM/ROM/BIOS views, RAM writes, exact instruction stepping, safe
  disassembly, and exec-only breakpoints. Sound-Z80 state, read/write breakpoints, trace, and
  call-stack classification remain outside this profile.
  → `adapters/mednafen/README.md`
- **Flycast (Dreamcast)** — build with `adapters/flycast/build.sh`; it builds in an
  emucap-owned work tree, pins the commit and recursive submodule graph, and treats any
  `FLYCAST_SRC` checkout as a read-only Git object source.
  → `adapters/flycast/README.md`
- **DeSmuME (Nintendo DS)** — build the headless fork with
  `adapters/desmume-nds/build.sh` (needs meson/ninja/SDL2/glib). No NDS BIOS is
  needed (HLE direct-boot). The dual CPUs (ARM9/ARM7) each get a GDB stub, like the
  PC-98 adapter. → `adapters/desmume-nds/README.md`
- **PPSSPP (PSP)** — build the headless fork with `adapters/ppsspp/build.sh` (needs
  CMake and a C++ toolchain). No PSP firmware is needed. The adapter is a pure
  WebSocket client against PPSSPP's own debugger protocol, so it's a single
  headless process plus the bridge — no GDB stub. → `adapters/ppsspp/README.md`
- **PCSX2 (PlayStation 2)** — build the pinned fork with
  `adapters/pcsx2/build.sh`, then set `EMUCAP_PCSX2_BIOS` to an absolute path to
  an operator-supplied BIOS dump. The isolated headless path supports EE memory
  and registers, pattern search and dumps, frame stepping, disassembly, frozen
  savestates, screenshots, controller input, pausing EE breakpoints with register
  snapshots, best-effort call stacks, and synchronous reset through a bounded PINE bridge.
  → `adapters/pcsx2/README.md`
- **Dolphin (GameCube · Wii)** — build the pinned native fork with
  `adapters/dolphin/build.sh` (Windows: `build.ps1`). The default launch is
  headless; `display: true` uses DolphinQt when the GUI build is available. The
  adapter provides PowerPC memory and registers, exact instruction stepping,
  disassembly, best-effort call stacks, exec breakpoints with register snapshots,
  bounded screenshots, and synchronous savestates. GameCube supports port-0
  controller input; Wii supports emulated Wii Remote 1 core buttons without
  claiming IR, motion, or extensions. → `adapters/dolphin/README.md`
- **MAME (PC-98)** — build MAME from source with `adapters/mame-pc98/build.sh`
  (slow, uses a lot of disk). The pinned build provides keyboard input plus
  relative pointer movement, frozen click, and drag operations without taking
  persistent ownership from a visible native mouse. → `adapters/mame-pc98/README.md`
- **NP2kai (optional PC-98 HDI compatibility backend)** — run
  `adapters/np2kai/build.sh`, build `emucap-np2kai`, and provide legally obtained
  firmware through `EMUCAP_NP2KAI_FIRMWARE`. Select it explicitly with
  `pc98_backend: "np2kai"`; omission keeps MAME. Its patched core and direct
  host expose the complete MAME PC-98 Control/Debug method set: bounded memory
  access and dumps, breakpoints and events, register state, instruction stepping,
  disassembly, trace and best-effort call stacks, exact frames, input, screenshots,
  native states, and verified `hdd0` replacement. Device semantics still differ:
  NP2kai is headless, has no host-audio launch path, cannot eject `hdd0`, limits
  disassembly to the current CPU mode, and captures breakpoint memory snapshots
  only at pausing hits. This backend accepts `.hdi` only; read breakpoints do not
  claim an authoritative access value, while write breakpoints support value
  filters.
  → `adapters/np2kai/README.md`
- **MAME (Neo Geo MVS/AES/CD, experimental)** — build the dedicated pinned MAME subset with
  `adapters/mame-neogeo/build.sh`, then build `emucap-mame-neogeo-bridge`. Launch
  requires an explicit system ID. MVS uses a user-supplied `neogeo.zip` plus a matching
  game ROM set. AES uses `aes.zip` and a cartridge set whose ZIP stem names an
  AES-compatible entry in MAME's pinned Neo Geo software list. CD uses an official
  `neocdz.zip` BIOS plus a CUE entry file whose referenced tracks all exist; its content
  identity covers the complete CUE graph. All three profiles expose bounded RAM, 68000
  state and stepping, exec/read/write breakpoints with hit-time evidence, disassembly,
  frame control, frozen-frame screenshots, and port-0 input. Native save/load is
  advertised for MVS and AES; MAME 0.288 marks CDZ save states unsupported.
  → `adapters/mame-neogeo/README.md`
- **Mupen64Plus (Nintendo 64, experimental; Unix)** — run
  `adapters/mupen64plus/build.sh`, then build `emucap-mupen64plus`. Standard cartridge
  ROMs need no BIOS. The current pure-interpreter adapter supports isolated headless or
  visible launch, pause/resume, R4300 instruction stepping, CPU state, and bounded frozen
  RDRAM access. Both modes expose port-0 input holds with explicit native-ownership release.
  Both modes also expose synchronous reset, R4300 exec/read/write breakpoints with hit-time
  evidence, event polling, and disassembly. Visible launch additionally exposes exact
  rendered-frame advance, bounded input pulses, current PNG capture, and
  completion-checked native save/load. Headless launch remains instruction-only and omits those
  rendered-frame operations. RSP state remains outside this profile.
  → `adapters/mupen64plus/README.md`
- **openMSX (MSX cartridge profiles, experimental)** — run
  `adapters/openmsx/build.sh`, then build `emucap-openmsx-bridge`. The official
  launcher accepts a pinned stock openMSX 21.0 sidecar and runs it with an
  emucap-owned per-port `HOME`; it does not patch openMSX or read the user's
  emulator profile. `msx` is C-BIOS MSX2+; `msx1`, `msx2`, and `msx2p` select
  explicit user-supplied real-firmware profiles. The cartridge surface includes Z80
  state and instruction step, exact headless or visible frame step, bounded CPU
  memory/main RAM/VRAM access, frozen save/load, keyboard-matrix and two-port
  joystick input, exec/read/write breakpoints, event polling, and disassembly.
  Screenshots require `display: true`. Disk/tape staging lacks representative runtime
  proof, and turboR/R800 is not implemented. Generic `.rom` files require an explicit
  MSX system ID.
  → `adapters/openmsx/README.md`

## Learn more

- What is built and why, and the binaries → `CLAUDE.md`
- Per-emulator memory types, button names, breakpoints, and launch
  troubleshooting → each `adapters/*/README.md`

Binaries: `emucap` (case bundles: `finalize` / `inspect`), `emucap-mcp` (Control
MCP — live emulator control, stdio), `emucap-track-mcp` (Tracking MCP —
experiment ledger, emulator-less, stdio), `emucap-broker` (multi-session
connection sharing), the N64 frontend, and the PC-98/Neo Geo/NDS/PSP/PS2/MSX launch bridges
listed in the build section.
