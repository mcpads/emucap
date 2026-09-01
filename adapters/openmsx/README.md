# openMSX adapter

This directory pins openMSX 21.0 for the experimental `msx`, `msx1`, `msx2`, and `msx2p`
system profiles.
`emucap-openmsx-bridge` remains a separate Rust process that owns the emulator's XML stdio
control channel. Two small pinned host patches add a two-byte control-plane debuggable for
readback-checked joystick ownership and a renderer-independent VDP frame-boundary probe.

Build the pinned source and the bridge:

```sh
./adapters/openmsx/build.sh
cargo build --release --bin emucap-openmsx-bridge --bin emucap-mcp
```

The script downloads the exact release archive, applies the pinned upstream SDL2 compatibility
backport and both emucap host patches, builds only inside the ignored `work/` tree, verifies the
generated executable with `-testconfig`, and writes every patch hash and host API 3 next to
the binary. On macOS it uses existing Homebrew development libraries. No generated source,
firmware, ROM, or binary is committed.

Launch through the MCP tool:

```text
launch(content_path="/absolute/path/game.rom", system="msx", display=false)
```

`.mx1` and `.mx2` are inferred as MSX cartridges. Generic `.rom` files require an explicit MSX
system because that extension is shared by unrelated platforms. `msx` uses a `C-BIOS_MSX2+`
cartridge. `msx1`, `msx2`, and `msx2p` select pinned real-machine profiles and resolve
user-provided firmware by accepted SHA-1, never by filename alone. Set
`EMUCAP_OPENMSX_FIRMWARE` to an absolute firmware inventory root. The repository does not
distribute those system ROMs.

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
- independent active-low joystick holds on ports 1 and 2, bounded-pulse restoration, explicit
  native release, and persistent-owner reapplication after savestate machine replacement;
- exact Z80 exec and logical-memory read/write breakpoints, bounded atomic register/memory
  snapshots, one-shot event delivery, and disassembly;
- public and native guest breakpoint identity preservation across savestate machine replacement,
  followed by a second live hit from the transferred breakpoint;
- exact headless frame progress from the VDP VSYNC probe even when `renderer=none`; the private
  frame-monitor breakpoint is rebound only after a restored machine reports an empty inventory;
- breakpoint interruption of a pending frame operation without losing the event or transient-input
  cleanup;
- out-of-range and cross-boundary memory requests fail loudly;
- frozen save/load restores a mutated RAM byte;
- public atomic `probe` composes frozen load, exact VDP frame step, and bounded memory read under
  one Control link lock;
- visible `SDLGL-PP` mode produces a 320x240 PNG with stable frozen-frame provenance.

The Homebrew bottle returned `Failed to take screenshot: TODO`, but the canonical source build did
not. This is why the launcher validates the pinned sidecar instead of accepting an arbitrary host
install. The `none` renderer still rejects screenshots, so a headless session omits that method.

The same release bridge passed the maintained cartridge smoke on C-BIOS MSX2+, Philips VG-8020
MSX1, Philips NMS-8250 MSX2, and Panasonic FS-A1WSX MSX2+. Disk and cassette content are accepted
only on their explicit profile media matrix and are mounted from generation-owned copies, but no
representative disk or cassette runtime witness is recorded yet.

The current adapter exposes Z80 state and instruction step, exact frame step, bounded
`memory`/`ram`/`vram` reads and writes while frozen, save/load, reset, pause/resume, standard
MSX keyboard-matrix input, two independent joystick ports, pausing exec/read/write breakpoints,
hit-time evidence, event polling, and Z80 disassembly. Read-watch events intentionally omit an
access value because openMSX does not provide an authoritative one at that callback. It does not
expose headless screenshots or R800/turboR state. Disk and cassette launch support remains
runtime-unproven until representative media reaches a declared boot anchor.

Run the maintained runtime smoke:

```sh
cargo run --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>"
cargo run --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>" --display
EMUCAP_OPENMSX_FIRMWARE=/absolute/firmware/root \
  cargo run --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>" --system=msx2
```

The smoke owns and terminates only the exact bridge and emulator processes it launched. The older
`openmsx_control_smoke` remains a lower-level XML-control diagnostic.
