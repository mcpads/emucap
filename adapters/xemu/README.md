# xemu original Xbox adapter

This adapter builds xemu from the source commit pinned in `upstream.lock` and applies the checked
patch stack needed for managed input, exact frame stops, hidden rendering, screenshots, and
Xbox-aware disc changes. Build products remain under the ignored `work/` directory.

```sh
./adapters/xemu/build.sh
```

The macOS build needs Xcode command-line tools and `dylibbundler`. xemu's dependency extraction
currently needs Python 3.8 through 3.13; set `EMUCAP_XEMU_PYTHON` if no compatible `python3` is first
on `PATH`. Linux uses xemu's normal source-build prerequisites. This repository does not publish
platform binaries.

Managed launch requires an operator-owned firmware inventory. Set `EMUCAP_XEMU_FIRMWARE` to an
absolute directory containing `mcpx_1.0.bin`, `Complex_4627.bin`, and `xbox_hdd.qcow2`. The launcher
verifies regular-file shape and records the exact digest of every input. It copies MCPX, flash, and
the HDD template into the generation directory; the first two copies are read-only and the HDD copy
is writable. If an optional 256-byte `xemu_eeprom.bin` is present it is copied too, otherwise xemu
creates a generation-local EEPROM. The source inventory and the user's normal xemu profile are never
modified.

The canonical system id is `xbox`. Because `.iso` is shared by many systems, managed launch requires
an explicit system id. A raw XISO is accepted; a loose `default.xbe` and later Xbox platforms are
outside this adapter.

Display and host audio are separate launch choices. Both default to disabled. Use `display:true`
for the isolated native window and `sound:true` for audible output; either may be enabled without
the other. The generation settings bind the requested audio policy explicitly, so hidden launch
does not inherit xemu's audible default.

The generation-local settings also bind xemu's keyboard controller to physical port 1, which is API
port 0. This creates the guest-visible XID device without depending on a user's profile and keeps
native keyboard input available whenever emucap does not own the input override. The fork rejects
an engaged override if that controller is not bound instead of reporting a false success.
`input_control(operation="describe")` reports the callable analog axes and their exact integer
ranges. `set_input` replaces the complete port-0 state: omitted axes are neutral, an explicitly
neutral axis still owns the controller, and empty buttons plus axes return ownership to native
input. The `l` and `r` button names remain full-trigger aliases; combining one with an explicit
value for the same trigger fails before mutation.

The live bridge exposes the common debugger baseline only when both the pinned QMP extension and the
GDB endpoint are ready. The patch stack preserves an already-held QEMU big lock when a TCG data
watchpoint re-enters its interrupt path. A watchpoint hit during x86 interrupt injection completes
the matched guest-memory access and schedules the debugger stop after injection instead of
invalidating a translation block under that lock. Ordinary MTTCG watchpoints likewise retranslate
one instruction without invalidating the translation block that is still executing. Without those
guards, a valid stack watchpoint can abort or stall the host instead of reporting a debugger stop.
xemu always starts with `-S`;
`start_frozen: true` preserves that
pre-first-instruction boundary, while an ordinary launch resumes only after both endpoints pass the
readiness check. The live methods include state, memory, pause/resume, exact frame and instruction stepping, port-0
button and analog input with native handback, screenshot, reset, Xbox-aware disc replacement, CPU
breakpoints, disassembly, best-effort call stack, and frozen save/load. State paths hold an
emucap-owned container rather than a raw xemu snapshot. It binds the same managed generation's
internal VM/HDD snapshot to EEPROM bytes, the exact disc, machine and host build identities, and
controller topology. It is deliberately same-generation-only: relaunching xemu invalidates the
container, and a different or modified disc is rejected before mutation. Load remains frozen,
reconciles owned breakpoints and input, and proves both QMP and GDB serviceability; a failed load is
rolled back to the prior frozen state or marked unresolved if recovery cannot be proved.
The negotiated debug `probe` operation validates its bounded frame and memory request before
mutation, then loads one such container, advances by exact NV2A boundaries, and reads the resulting
frozen memory without a public-request gap. Breakpoint preemption returns the interrupted boundary
and retains its event instead of being flattened into completion.

The `main` memory type is the complete 64 MiB physical RAM region at offset zero. The `cpu` type is
the current i386 virtual address space. The bridge bounds xemu's GDB physical-memory mode to one
`main` request and restores virtual mode on every terminal path, so an unmapped CPU page cannot
make a full RAM dump fail or silently change the meaning of the next debugger request.

The dashboard boot PoC verifies isolated configuration, explicit audio policy, hidden OpenGL
rendering, a three-frame frozen-to-frozen step, GDB register and memory access, input
acquire/release, a 640x480 PNG, and fail-loud missing-disc handling. A separate representative game
XISO smoke reached game-visible menus and consumed confirm and directional input; audible output
was observed with `sound:true`. That smoke is evidence for the managed path, not a broad xemu game
compatibility claim. Narrow debugger validation also completed a full 64 MiB physical-RAM dump,
returned both write-watchpoint and exec-breakpoint interruptions without losing the bridge, replaced
the DVD while frozen, and decoded the controlled reset vector at `0xfffffff0`. The state-container
path has separate fake-transport coverage for save, intervening state change, load, frozen
serviceability, exact EEPROM restoration, failed-load rollback, and atomic probe completion and
interruption. Release runtime validation is a separate gate.

Only `status.methods` and the negotiated MCP drawers describe callable operations. The adapter does
not expose unimplemented operations as a future-method list.
