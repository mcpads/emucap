# Windows x64 JIT exit regression

Run on Windows x64 and Windows ARM64 translating x64. This isolates the native
CPU exit ABI without ROMs, BIOS, graphics or the broker.

After applying the pinned xemu patch stack, compile the test with a Windows x64
C toolchain (or cross-compile with `zig cc -target x86_64-windows-gnu`):

```sh
zig cc -target x86_64-windows-gnu -O2 \
  _tests/live/xemu/windows_jit_exit.c _tests/live/xemu/windows_jit_exit.S \
  adapters/xemu/work/xemu/util/longjmp-win64.S -o windows_jit_exit.exe
```

Run `windows_jit_exit.exe`: require exit 0 and all 5,000 iterations passing.
The test dynamically resolves `_setjmp` from `msvcrt.dll`, matching the xemu
cross-build import. Generated code corrupts all eight nonvolatile integer
registers and XMM6 through XMM15 before returning through the patched Windows signal jump.
The caller verifies every sentinel and return values 0 (normalized to 1), 1, 2, -2 and -3. This catches partial
register restoration that a compiler builtin jump would not guarantee.

Run `windows_jit_exit.exe baseline` separately to select the CRT `longjmp`.
On the affected ARM Windows build it exits with `0xc00000ff`; record that negative
control without requiring an unaffected Windows x64 host to reproduce it.
Native emulator boot, state restore and pacing remain separate runtime checks.
