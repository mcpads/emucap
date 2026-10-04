# emucap

**Let AI agents see, play, and debug games in emulators.**

[한국어](README.ko.md) · [Releases](https://github.com/mcpads/emucap/releases) · [Agent guide](AGENT_GUIDE.md)

emucap connects MCP-compatible agents such as Claude Code and Codex to retro-game
emulators. Ask your agent to play through a scene, reproduce a bug, or investigate
what the game is doing. It can inspect the screen, press buttons, advance frames,
and examine memory through one interface across supported emulators.

## What you can do

- **Play and observe:** capture screenshots, press or hold buttons, move the mouse,
  and adjust execution speed where supported.
- **Reproduce a problem:** save a checkpoint, replay inputs, and compare results.
- **Investigate the game:** read memory and registers, set breakpoints, and step
  through instructions.
- **Keep experiment records:** optionally record runs, findings, and comparisons
  with the separate Tracking MCP.

For example, give your agent a game path and ask:

> “Open this game and pause it before we start.”
>
> “Save a checkpoint, hold right for 60 frames, and show me the screen.”
>
> “Find what writes to this memory address when the player takes damage.”

Available operations depend on the emulator and system. The connected runtime
reports them through `status`, so the agent can choose the supported operations.

## Get started

Give your agent this repository and the game you want to use:

> “Install emucap using AGENT_GUIDE.md, register its MCP servers, and prepare the
> adapter for this game.”

The agent installs the core, prepares the selected emulator adapter, and checks
that it can connect. You provide the game and any required BIOS or firmware.

Core packages are available for **Windows x86-64, Linux x86-64, and macOS Apple
Silicon** on [GitHub Releases](https://github.com/mcpads/emucap/releases).
Source builds also support Intel macOS. Emulator adapters are prepared separately;
their build requirements and features vary by host OS.

For manual installation, see the [installation steps](AGENT_GUIDE.md#1-choose-the-core-installation).
The core exposes two MCP servers: **Control** drives the emulator, while optional
**Tracking** stores experiment records. The agent combines them as needed.

## Systems

| Emulator | Systems |
| --- | --- |
| Mesen2 | NES, SNES, Master System, Game Gear, Game Boy / Color, GBA |
| Mednafen | PlayStation, Saturn, PC Engine, PC-FX, Mega Drive / Genesis, WonderSwan / Color, Neo Geo Pocket / Color |
| Flycast | Dreamcast |
| DeSmuME | Nintendo DS |
| PPSSPP | PSP |
| PCSX2 | PlayStation 2 |
| Dolphin | GameCube, Wii |
| MAME / NP2kai | PC-98 |
| MAME | Neo Geo MVS / AES / CD — experimental |
| Mupen64Plus | Nintendo 64 — experimental |
| openMSX | MSX1 / MSX2 / MSX2+ — experimental |
| xemu | Original Xbox — experimental |

See the [adapter guide](AGENT_GUIDE.md#per-emulator-adapters-the-agent-installs-when-needed)
for setup and each profile’s supported scope. emucap maintains source patches where
native emulator changes are needed for control and observation.

## Project status

**1.0.0 is available.** [COMPATIBILITY.md](COMPATIBILITY.md) defines
the 1.x compatibility commitments; [CHANGELOG.md](CHANGELOG.md) records changes.

The core and otherwise unmarked source are **GPL-2.0-or-later**. Emulator patches
follow their upstream license boundaries. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
