# emucap — emulator monitor + HITL adaptor

> 한국어 안내: [README.ko.md](README.ko.md)

MCP infrastructure for debugging retro-game patches. An AI agent reads and
controls a running emulator's memory, state, and screen so it can analyze a
problem a human described in plain language. A common Core plus per-emulator
adapters supports several emulators — Mesen2 (SNES · Game Gear · Game Boy · GBC ·
GBA · NES), a Mednafen fork
(Saturn · PlayStation · PC Engine · Mega Drive/Genesis · WonderSwan/WSC), Flycast
(Dreamcast), a DeSmuME fork (Nintendo DS), a PPSSPP fork (PSP), and MAME (PC-98).

**v0.9.0-alpha.5 — alpha.** This repository is under active, continuous development;
interfaces and behavior may change between prereleases.

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

- **Rust** — check with `command -v cargo`. If missing:
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
  --bin emucap-mame-pc98-bridge --bin emucap-desmume-nds-bridge \
  --bin emucap-ppsspp-bridge
```

Outputs: `target/release/emucap-mcp` (**Control MCP** — drives the emulator),
`emucap-track-mcp` (**Tracking MCP** — experiment ledger, emulator-less),
`emucap` (case-bundle CLI), `emucap-broker` (multi-session broker),
`emucap-mame-pc98-bridge` (PC-98 launch helper),
`emucap-desmume-nds-bridge` (NDS launch helper), and
`emucap-ppsspp-bridge` (PSP launch helper). All dependencies come from
crates.io and SQLite is bundled, so **nothing beyond Rust and a C compiler is
required** for a source build. The first build is slower while dependencies
download; later builds are fast.

### 3. Register the MCP servers (two of them)

emucap is split into **two MCPs, and you register both** — the agent composes
them (see §2b).

- **Control MCP** (`emucap-mcp`) — the emulator-driving engine. Reads memory,
  state, and screen; controls input, save-states, and breakpoints; and returns
  results from analysis verbs (`regression_run` / `verify_determinism`).
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

- **Pass rom_sha1**: read the ROM identifier via the Control MCP's `get_rom_info`
  (`.sha1`) and pass it to the Tracking MCP's `run_start(rom_sha1=…)` (if an
  adapter lacks `get_rom_info`, fall back to `shasum -a1 <content>`). Passing
  `connection_ref` (the Control MCP `status` connection name, or `"port:N"`)
  auto-finalizes the previous unfinished run on that connection.
- **Analysis verbs only return**: `regression_run` / `verify_determinism` have
  the Control MCP drive the emulator and *return* a
  result without writing to the ledger. To record it, log the result via the
  Tracking MCP's `log_gate` / `log_metric`.
- **Frame-boundary search composes `probe`**: binary-search the frame range with
  repeated atomic `probe` calls. Each call restores the same base state,
  advances, and reads the predicate without an externally visible gap.
- **Interventions are logged explicitly**: state changes like `write_memory` /
  `load_state` / `reset` / input are not recorded automatically, so log them via
  the Tracking MCP's `log_intervention` to preserve reproduction fidelity.

### 4. First run (the agent starts with bootstrap)

Every emucap task starts with `bootstrap`. Ask the agent to "call emucap
`bootstrap`", and it returns `listening_port`, `runtime_paths` (each adapter's
absolute build paths and legacy fallback launchers), the supported systems, and
questions about what to bring up. Then `launch_plan(content_path, system?)`
returns the preferred MCP `launch` tool arguments. The agent calls `launch` and
checks `status` a few seconds later. Because **bootstrap also reveals the adapter
install paths and fallbacks**, the agent never has to hunt around the filesystem.

A timeout or `connected: false` reports transport state, not proof that the
emulator exited. Inspect `status.continuity`, `status.runtime_instance`, and
`get_failure_context` before relaunching. Reattach to a live owned generation;
use `launch(..., replace: true)` only for an intentional, identity-verified
replacement. On a Flycast fatal quarantine, read the preserved context first and
call `dismiss_failure` only when `status.methods` advertises it.

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
- **Mednafen (Saturn · PSX · PCE · MD · WonderSwan/WSC)** — build the fork with
  `adapters/mednafen/build.sh` (needs SDL: macOS `brew install sdl2`, Linux
  `libsdl2-dev`). Its source archive and checksum are pinned. One binary handles all five systems. PSX and PCE-CD need BIOS
  files (not committed to the repo). → `adapters/mednafen/README.md`
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
- **MAME (PC-98)** — build MAME from source with `adapters/mame-pc98/build.sh`
  (slow, uses a lot of disk). → `adapters/mame-pc98/README.md`

## Learn more

- What is built and why, and the binaries → `CLAUDE.md`
- Per-emulator memory types, button names, breakpoints, and launch
  troubleshooting → each `adapters/*/README.md`

Binaries: `emucap` (case bundles: `finalize` / `inspect`), `emucap-mcp` (Control
MCP — live emulator control, stdio), `emucap-track-mcp` (Tracking MCP —
experiment ledger, emulator-less, stdio), `emucap-broker` (multi-session
connection sharing), and the PC-98/NDS/PSP launch bridges listed in the build
section.
