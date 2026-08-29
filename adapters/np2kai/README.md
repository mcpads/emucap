# NP2kai PC-98 compatibility backend

This adapter builds a pinned NP2kai libretro core for the optional PC-98
compatibility backend. The public system remains `pc98`; select this backend
with `pc98_backend: "np2kai"`. Omitting the field continues to select MAME.

Build the core and the direct emucap frontend:

```sh
./adapters/np2kai/build.sh
cargo build --release --bin emucap-np2kai
```

The supported build profile deliberately excludes FMGEN, GPL MAME sound,
DOSBox FPU, the older SoftFloat tree, and Trident TGUI sources. It retains the
BSD MAME sound path and SoftFloat 3. The build stops if the effective source or
define manifest violates that boundary, then writes
`work/np2kai/emucap-np2kai-build.json` beside the core.

The repository does not distribute the generated core or PC-98 firmware. Put
your legally obtained NP2kai firmware inventory in a directory and set
`EMUCAP_NP2KAI_FIRMWARE` to it. The launcher copies only recognized firmware
files into the per-port emucap runtime; it never reads or changes a user's
RetroArch or NP2kai profile.

The patch stack adds a small native debug ABI at CPU execution, memory access,
machine reset, and hard-disk transition boundaries. The direct host therefore
advertises the same 33 Control/Debug methods as the MAME PC-98 backend:

- bounded physical, RAM, text-VRAM, and planar graphics-VRAM reads, writes,
  searches, and dumps;
- register state, exec/read/write/access breakpoints, register watches,
  breakpoint events, exact instruction stepping, current-mode disassembly,
  bounded trace, and best-effort call stacks;
- exact frame execution, keyboard and relative-pointer input, frozen
  screenshots, identity-bound native states, reset observation, and verified
  `hdd0` replacement through an emucap-owned media copy.

Method parity does not erase device differences. `hdd0` must remain loaded,
only `.hdi` media and one hard disk are exposed, and a media change is verified
against the native core's reported path or rolled back with the observed state
reported. Read breakpoints expose address, size, and access-time registers but
not a value or value filter; the native hook cannot observe mapped-read results
without performing a second, potentially state-changing read. Write values are
authoritative and may be filtered. Breakpoint memory snapshots require
`pause_on_hit: true`; they describe the resulting frozen boundary. Disassembly
uses the active x86 CPU mode and code-segment mapping. Call stacks come from
bounded trace calls when tracing is active and otherwise use a best-effort
frame-pointer walk.

Keyboard input includes guest F12 because the pinned core disables NP2kai's F12
frontend-menu shortcut. Relative pointer input has the two PC-98 guest buttons,
`mouse_left` and `mouse_right`; NP2kai's middle-button frontend-menu shortcut is
not advertised as a guest control.

This frontend remains headless and deliberately provides no host-audio launch
path. Those are presentation capabilities rather than missing Control/Debug
methods. Use the default MAME backend when a visible window, host audio,
multiple PC-98 media devices, or MAME-specific machine configuration is
required.
