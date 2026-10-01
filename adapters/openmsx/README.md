# openMSX adapter

This directory pins openMSX 21.0 for the experimental `msx`, `msx1`, `msx2`, and `msx2p`
system profiles.
`emucap-openmsx-bridge` remains a separate Rust process that owns the emulator's XML stdio
control channel. Four pinned host patches add readback-checked joystick ownership, a renderer-independent
VDP frame-boundary probe, completed-raster retention with native frame provenance, and transactional disk-state relocation.

Build the pinned source and the bridge:

```sh
./adapters/openmsx/build.sh
cargo build --locked --release --bin emucap-openmsx-bridge --bin emucap-mcp
```

The script downloads the exact release archive, applies the pinned upstream SDL2 compatibility
backport and all five emucap host patches, builds only inside the ignored `work/` tree, verifies the
generated executable with `-testconfig`, and writes every patch hash and host API 6 next to
the binary. Linux builds install runtime resources inside the selected work directory.
macOS builds include them in the app bundle and use existing Homebrew development libraries. No generated source,
firmware, ROM, or binary is committed.

Pacing waits share the event queue's mutex and predicate. A queued command returns from the
wait immediately; a command arriving during the wait wakes it. The wait reports a completed
delay only if its deadline expires with an empty queue. Dispatch remains on the native main
thread, and an early wake preserves the remaining guest-time lead for the next pacing sync.
Run `bash _tests/adapters/openmsx/event-wait-test.sh adapters/openmsx/work/openmsx-21.0`
to exercise the actual native event queue and wait with isolated dispatcher dependencies.

Launch through the MCP tool:

```text
launch(content_path="/absolute/path/game.rom", system="msx", display=false)
```

`sound:true` enables host audio independently of `display`; the default is muted. The bridge
checks native `mute` readback before launch readiness, and `status.diagnostics.audio_muted`
reports the current setting. Pacing changes host timing while preserving this audio choice.

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
- visible `SDLGL-PP` mode produces a 320x240 PNG from the latest completed native VDP field,
  with separate capture-time PC/frame/cycle and raster-boundary identities.

The Homebrew bottle returned `Failed to take screenshot: TODO`, but the canonical source build did
not. This is why the launcher validates the pinned sidecar instead of accepting an arbitrary host
install. The `none` renderer still rejects screenshots, so a headless session omits that method.

The same release bridge passed the maintained cartridge smoke on C-BIOS MSX2+, Philips VG-8020
MSX1, Philips NMS-8250 MSX2, and Panasonic FS-A1WSX MSX2+. Disk and cassette content are accepted
only on their explicit profile media matrix and are mounted from generation-owned copies. MSX2 disk
checks now include Madoushi Lulba's natural boot/menu/cursor and Madou Monogatari 2's text-consumer
breakpoint with two memory snapshots. Other disk profiles and cassette runtime remain unproven.

The current adapter exposes Z80 state and instruction step, exact frame step, bounded
`memory`/`ram`/`vram` reads and writes while frozen, save/load, reset, pause/resume, standard
MSX keyboard-matrix input, two independent joystick ports, pausing exec/read/write breakpoints,
hit-time evidence, event polling, and Z80 disassembly. Read-watch events intentionally omit an
access value because openMSX does not provide an authoritative one at that callback. It does not
expose headless screenshots or R800/turboR state. The bounded MSX2 disk witnesses do not qualify
other titles, whole-game behavior or cassette boot paths.

The contract exception registry covers the four supported system IDs individually; button
metadata reports the selected system. `msxtr` remains rejected because the bridge has no proven
Z80/R800 selection contract. A breakpoint can capture up to 16 ordered memory ranges. The Tcl
producer quotes its snapshot delimiter; the Rust parser verifies count, region, address, length
and bytes before publishing a hit. Debugger integrity failures still terminate that generation,
but managed launches retain the original reason in their failure artifact before shutdown. This
is an adapter failure, not evidence of a guest crash.

The producer/parser integration test requires Tcl 8.6+ (`TCLSH` can select the executable):

```sh
cargo test --locked --lib openmsx_bridge::tests::debugger_tcl -- --ignored
```

It executes the installed Tcl script with synthetic emulator primitives, then drains through the
real Rust parser for 0, 1, 2 and 16 ranges. Real-machine breakpoint and display checks remain separate.

Run the maintained runtime smoke:

```sh
cargo run --locked --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>"
cargo run --locked --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>" --display
EMUCAP_OPENMSX_FIRMWARE=/absolute/firmware/root \
  cargo run --locked --release --example openmsx_adapter_smoke -- "<path-to-msx-cartridge>" --system=msx2
```

The smoke owns and terminates only the exact bridge and emulator processes it launched. The older
`openmsx_control_smoke` remains a lower-level XML-control diagnostic.


## Completed-raster capture

Visible managed sessions render every VDP field even with throttling disabled. Window repaint
keeps the upstream wall-clock scheduling. A screenshot reads the retained completed raw raster;
it does not render current VRAM retroactively or advance the guest. `freshness` is
`latest_completed_frame`, with `capture_boundary` (Z80 PC and native VDP frame/cycle) and
`raster_boundary` (epoch, machine, launch, native frame, completion boundary and PNG digest).
The top-level `frame` uses the separately named bridge frame-boundary sequence domain.

The native producer publishes a frame identity only after frame-end rendering and the buffer
rotation into the screenshot source. Reset, restore and renderer reinitialization invalidate it.
A missing or stale completed raster returns `bad_state`; callers choose whether to advance.
Capture never inserts a hidden step. Managed capture disables deinterlace and deflicker so each
image represents one completed field; a changed capture policy is rejected. Headless capture
remains unsupported. Host API 6 and the pinned patch digests are required; rebuild the native host,
Control and bridge together.

The opt-in native witness uses a generated black/white border-toggle cartridge and checks pixels
against the independently stored VDP colour write, including long/short steps, exec halts,
frozen repetition and restore. It requires Pillow and writes evidence under the chosen directory:

```sh
python3 _tests/live/openmsx/raster_capture.py --output /absolute/private/evidence-directory
```

PNG bytes may differ because upstream embeds wall-clock creation metadata; repeated captures
must preserve raster identity, decoded pixels and guest state. A bounded MSX2 Madou Monogatari 2
consumer route is also qualified locally; other scene types and real non-macOS hosts are not
covered by that witness.

## Disk snapshot portability

Disk launches expose `diska` in `status.media_devices`. At a frozen boundary use
`change_media(device="diska", path=<absolute image>, expected_sha1=<optional SHA-1>)`
or `change_media(device="diska", eject=true)`. Each insertion copies the requested
bytes into the current generation; the source image is never mounted writable.
The operation returns frozen without advancing guest time. `status.mounted_media`
reports the current mount separately from the immutable launch source identity.

The response's `previous.path`, `previous.sha1` and `previous.size` describe a sector
export including guest writes. Reinsert that path to continue using the written data
disk. Inserting the original source path again starts from its original bytes. Copy
the exported file outside the generation before stopping if it must be retained.
`load_state` similarly returns `previous_media` for the outgoing disk. These exports
remain separate from later writable mounts and snapshot scratch files.

Only drive A is supported for managed disk changes. An empty drive can execute and
load an existing snapshot; `save_state` requires an inserted disk and rejects an empty
drive before native serialization. Cartridge and cassette launches do not advertise
this disk-change capability. Failed preconditions leave the mount untouched; native
command failures require unchanged mount/frozen readback, otherwise the generation
fails terminally rather than claiming recovery.

For disk launches, `save_state` atomically publishes a self-contained ZIP at the requested
path: native machine state, full drive-A sector image, and a manifest binding both hashes
to source media, machine, firmware and host ABI. `load_state` validates it, creates a fresh
working disk inside the current generation, and substitutes that disk during native device
deserialization. Saved controller state and disk-change flags are retained. A native disk
checksum mismatch is an error. Old generation paths and their continued existence are irrelevant.

The original frozen machine stays alive until the candidate passes runtime identity, debugger,
input and frame-monitor checks. Rejected candidates return an error with the original session
usable; an unverified rollback is a terminal integrity failure. Native watchpoints and probes
are copied into the candidate with their IDs. A successful restore starts a new capture epoch.

Legacy raw disk states can restore only when the unchanged admitted source bytes match the
saved native disk checksum. The adapter supplies a private verified copy; historical media
paths are never opened. Missing guest-written disk bytes cannot be reconstructed and cause
a recoverable rejection. New saves always embed snapshot-time disk bytes. Cartridge and
cassette formats retain their existing compatibility. Disk snapshots support a single unpatched
DSK in drive A, with native state and sector image each bounded to 64 MiB.

## Batched memory and execution speed

`read_memory_batch` reads ordered `ranges` at one globally frozen boundary. Each range has
`memory_type`, `address`, and `length`, using the same address domains as `read_memory`.
Limits and admitted windows come from `status.memory_batch_capability`: 1–64 ranges,
1–16 KiB per range, 64 KiB total, inside `memory`, `ram` or `vram`. Every range is validated
before one native command reads them all. Success contains `reads` indexed by request position,
exact hex bytes, `consistency: "frozen_boundary"`, and `boundary` with the runtime generation,
native `stop_epoch`/`memory_mapping_epoch` and the VDP frame clock. The epochs change after any
execution, load, reset, media change or debugger write, even at an unchanged frame. Invalid or
incomplete batches produce errors without partial data. Pause explicitly before reading.

Agents can adjust host pacing for automated collection or human interaction through `debug`.
Call `debug(operation="describe")`, then use its current capability revision:

- `operation="execution_speed", arguments={"mode":"limited","percent":50}`: half speed.
- `arguments={"mode":"limited","percent":100}`: normal speed; 200 is double, 400 quadruple.
- `arguments={"mode":"unlimited"}`: remove pacing, the existing launch default.
- `arguments={}`: query the effective policy. `status.execution_speed` reports the same object.

Limited mode admits 0.01–10000 percent in 0.01 steps (`status.execution_speed_capability`);
other values are rejected, never rounded. It sets a host pacing target, not a guaranteed
throughput. Policies report `mode`, `percent` (null unless `limited`), `policy_revision`, and
the raw native settings under `diagnostics`. Changes work while running or frozen without
pause/resume and keep a frozen stop identical. They do not change guest clocks, rendering, input
duration or breakpoint semantics. Native fast-forward and loading acceleration are disabled by
an explicit change; when a person enables them, or sets a speed off the 0.01 grid, the policy is
`custom`. A change is one native transaction: a rejected or clamped setting is restored and
reported as `failed_restored`; a newer external change is kept and reported as a conflict;
unverifiable restoration terminates the control capability with failure evidence. Reset and
state loads keep the current policy.

Control requests remain ordered: a speed change during another Control request returns `busy`
instead of waiting. A frame step that reaches its 10-second host budget at a slow speed stops at
the reached frame and returns `status: "interrupted", reason: "host_deadline"` with the actual
count, so split long advances at low percentages. Host patch 0005 lets a control command wake the
real-time pacing sleep, so commands stay responsive at any percentage without shortening pacing.

Rebuild Control and the openMSX bridge and reconnect to discover these operations. The native
host API remains unchanged. Other adapters advertise these capabilities only when implemented.
