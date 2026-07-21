# Mupen64Plus N64 adapter

This experimental adapter pins the upstream 2.6.0 source bundle, builds the core
with debugger support, and keeps build products under `work/`. Standard N64 cartridge images do
not require a BIOS. N64DD is outside this adapter's current scope.

Build the pinned bundle and verify its structural PoC:

```sh
./adapters/mupen64plus/build.sh
./adapters/mupen64plus/poc/inspect.sh
```

The bundled `m64p_test_rom.v64` is the upstream project's GPL test ROM. Its digest is pinned in
`upstream.lock`; the ROM and all generated binaries remain in the ignored work tree.

The MCP `launch` path validates the build metadata against `upstream.lock` and gives each port an
emucap-owned configuration directory. Headless mode loads only the RSP plugin; `display: true` also
loads the pinned Rice video plugin.

The current advertised methods are status and ROM identity, pause/resume, R4300 state and exact
instruction stepping, and bounded RDRAM reads/writes while frozen. The upstream test ROM has passed
the official MCP launch path, pause stability, one-instruction step, write/read/restore, and
cross-boundary rejection. Input, screenshots, save states, frame stepping, breakpoints, and RSP
state are intentionally not advertised yet.
