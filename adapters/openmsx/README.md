# openMSX adapter

This directory pins stock openMSX 21.0 for the first experimental `msx` system profile. openMSX
is not patched: `emucap-openmsx-bridge` remains a separate Rust process that owns the emulator's
XML stdio control channel.

Build the pinned source and the bridge:

```sh
./adapters/openmsx/build.sh
cargo build --release --bin emucap-openmsx-bridge --bin emucap-mcp
```

The script downloads the exact release archive, applies the pinned upstream SDL2 compatibility
backport, builds only inside the ignored `work/` tree, verifies the generated executable with
`-testconfig`, and writes build metadata next to the binary. On macOS it uses existing Homebrew
development libraries. No generated source, firmware, ROM, or binary is committed.

Launch through the MCP tool:

```text
launch(content_path="/absolute/path/game.rom", system="msx", display=false)
```

`.mx1` and `.mx2` are inferred as MSX cartridges. Generic `.rom` files require `system=msx`
because that extension is shared by unrelated platforms. The initial runtime scope is a
`C-BIOS_MSX2+` cartridge. C-BIOS avoids proprietary system ROMs, but it does not prove disk,
cassette, real-machine firmware, or turboR support. The repository does not distribute those
system ROMs.

The official launcher validates `emucap-openmsx-build.json`, starts the bridge, captures both
process identities, and waits for authenticated adapter readiness before publishing the runtime
generation. openMSX receives an emucap-owned per-port `HOME`, so it does not read or change the
operator's normal openMSX profile.

The canonical pinned build and representative cartridge have passed:

- an emucap-owned `HOME` isolates user settings and state files;
- authenticated MCP launch with matching live and capsule `launch_id`;
- machine identity, exact bounded frame step, Z80 instruction step, and runtime-enumerated CPU
  memory, main RAM, and VRAM sizes;
- keyboard-matrix hold, bounded pulse, and `set_input([])` release, observed at the matrix byte;
- out-of-range and cross-boundary memory requests fail loudly;
- frozen save/load restores a mutated RAM byte;
- visible `SDLGL-PP` mode produces a 320x240 PNG with stable frozen-frame provenance.

The Homebrew bottle returned `Failed to take screenshot: TODO`, but the canonical source build did
not. This is why the launcher validates the pinned sidecar instead of accepting an arbitrary host
install. The `none` renderer still rejects screenshots, so a headless session omits that method.

The current adapter exposes Z80 state and instruction step, exact frame step, bounded
`memory`/`ram`/`vram` reads and writes while frozen, save/load, reset, pause/resume, and standard
MSX keyboard-matrix input. It does not expose breakpoints, joystick delivery, headless
screenshots, disks, tapes, real-machine firmware, or R800/turboR state.

Run the maintained runtime smoke:

```sh
cargo run --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>"
cargo run --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>" --display
```

The smoke owns and terminates only the exact bridge and emulator processes it launched. The older
`openmsx_control_smoke` remains a lower-level stock-control diagnostic.
