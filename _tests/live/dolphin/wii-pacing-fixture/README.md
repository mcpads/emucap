# Wii WPAD observation guest

Build an independently authored C guest with the pinned devkitPro ARM64 Linux
container (Docker required):

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-input-guest
```

The output directory must be new. It retains source, ELF, symbols, disassembly,
compiler/package inventory and hashes. The script's image digest pins devkitPPC
and libogc; no game media is needed. The fixture uses ELF because the pinned
Dolphin DOL loader rejects a non-32-byte-sized data section emitted by this
SDK's DOL converter. No padding or opcode patch is applied to the executable.

The guest selects NTSC 480-progressive video and alternates one CPU-written XFB
pixel pair after each VSync by default. Pass `2` as the second build argument to
retain each image for two loop iterations:

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-duplicate-guest 2
```

`presentation-period.txt` records the selected period. At an odd loop count in
this variant, the observation halt follows an unchanged XFB write. Save at that
halt and compare uninterrupted input with reloads of the same snapshot: the next
VI must retain its duplicate classification. Select the origin before saving;
compare full records without post-load warmup. Run with native texture-cache
saving enabled and disabled to detect lost duplicate-image classification after restore.

Pass `gx` as the third argument to copy the EFB through actual GX commands:

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-gx-guest 2 gx
```

This variant initializes the GPU FIFO, changes EFB clear color, copies to the XFB
once per selected period, and waits for `GX_DrawDone` before the observation halt.
It exercises GPU copy/completion and native immediate-XFB presentation. Select an
even-loop origin for period 2 and verify the serialized native immediate flag when
that mode is under test. `renderer.txt` records the selection; `cpu` is the default.
Resolve addresses from each build's symbols. The record layout is shared, but the
GX fixture has different addresses and presentation behavior. It does not draw
textured primitives or establish live TMEM bindings.

`gx-split` copies two half-height EFB images into adjacent XFB halves. In ordinary
VI mode, an even-loop halt follows creation of the next pair; an odd-loop halt
retains the assembled container and its two live copy references. Use both origins
with optional cache saving disabled to check future-copy and reference preservation:

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-gx-split-guest 2 gx-split
```

Record the tested producer identity and rendering profile with each result;
passing one profile does not qualify another.

`gx-texture` copies a 32-by-32 EFB region into a texture, binds it, draws a triangle,
and copies the resulting EFB to the display. Reusing the same texture registers
allows native TMEM to retain an older cached image even as later copies overwrite
its backing RAM:

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-tmem-guest 2 gx-texture
```

Verify the actual saved binding, then compare both guest observations and displayed
image bytes after continuation. Restored CPU/input equality can coexist with
incorrect TMEM pixels.

`gx-texture-ram` draws with a CPU-written RGB565 texture instead of an EFB copy.
It overwrites and flushes the backing RAM while retaining the same native texture
registers. Check that the serialized binding is neither an EFB nor an XFB copy,
then compare native image bytes as well as guest observations:

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-tmem-ram-guest 2 gx-texture-ram
```

`gx-texture-ram-retained` keeps the EFB after each display copy. The next triangle
draw consumes the retained multisample edge values. Compare ordered native images
from the same checkpoint at one sample and at a supported MSAA setting; CPU equality
alone misses resolve-and-replicate damage; compare the continued image sequence.

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-msaa-guest 2 gx-texture-ram-retained
```

`gx-depth-retained` draws shifted near/far triangles with depth testing and writes
enabled, changes their texture colors, and retains the EFB. Verify that the native
snapshot contains differing depth samples before using it as a depth-continuation
witness. Its orthographic camera uses negative view-space Z for visible geometry.

```sh
bash _tests/live/dolphin/wii-pacing-fixture/build.sh out/wii-depth-guest 2 gx-depth-retained
```

For state-owner regressions, run `instruction_breakpoints.py` with the built guest
and its `fixture_observe` address from `symbols.txt`. Use
`--display --video-backend OGL --save-texture-cache` to exercise GUI OpenGL readback.
The witness verifies the actual backend, unchanged CPU/frame observations during
save, full CPU restoration, and subsequent stepping.

The guest samples the real WPAD/IOS/Bluetooth path and
records fourteen big-endian words in `fixture_record`:

| Word | Meaning |
| --- | --- |
| 0–1 | WPAD signature and layout revision |
| 2 | Loop iterations |
| 3 | Valid WPAD samples |
| 4 | Valid samples with A held |
| 5–6 | A-down and A-up transitions on valid samples |
| 7–8 | Raw buttons and signed WPAD probe result |
| 9–10 | Guest timebase high/low words |
| 11 | libogc retrace callback count |
| 12 | WPAD data-present flags |
| 13 | Consecutive valid samples |

Read the record at a native instruction halt. Connection readiness requires
valid WPAD probes, not merely a nonzero counter. The timebase is sampled after
WPAD processing; it is not an exact VI-edge timestamp. Invalid probes never count
as input consumption.

`fixture_observe` is a deliberately empty, non-inlined function after the record
and XFB update. Its compiled return instruction provides a regression site for
native call/return optimization and instruction breakpoints. Use `symbols.txt`
and `disassembly.txt` to select the return, its call and an ordinary loop
instruction. With the default CPU build their addresses are respectively
0x800042f4, 0x800041d8 and 0x8000419c:

```sh
python3 _tests/live/dolphin/instruction_breakpoints.py \
  --content out/wii-input-guest/fixture.elf --system wii --cpu 4 \
  --address 0x800042f4 --address 0x800041d8 --address 0x8000419c \
  --output out/wii-instruction-halts
```

CPU selectors are 0 (Interpreter64), 4 (JITARM64) and 5 (Cached Interpreter).
The witness warms translated code before arming, tests clear/re-arm and restored
PC, resumes without stale hits, and stops its exact managed generation.
`--display` selects the GUI producer; sound is disabled and CPUThread is false.

Pacing admission additionally requires same-origin, same-input repeatability
before comparing different policies. The initial guest exposed an alternating
same-policy restore/input result.
The source is retained for that investigation and halt regression; it is not yet
a passing Wii S8 oracle. Do not equate presented completions, guest loop iterations
and VI fields or discard their differences after a failure.

## Ordered presentation continuation

After building the maintained Dolphin adapter and `emucap-mcp`, use
`../presentation_continuation.py` with a newly built `gx-depth-retained` fixture.
Resolve `fixture_observe` and `fixture_record` from that build's symbols and pass
their numeric addresses as `--address` and `--record-address`:

```text
python3 _tests/live/dolphin/presentation_continuation.py \
  --content <fixture.elf> --output <new-output-directory> \
  --address <fixture_observe> --record-address <fixture_record> \
  --backend Metal --dual-core --samples 4
```

Add `--display --backend OGL --layers 2` for GUI OpenGL stereo. `--lle`, `--sound`,
`--immediate-xfb` and `--save-texture-cache` select additional native profiles.
An immediate-XFB run requires the chosen halt to contain an active immediate field;
the witness verifies it in the saved state. The backend must actually support the
requested layers and samples. CPU choices use Dolphin enums via `--cpu`.

The macOS/Linux witness owns an isolated managed launch and stops its exact launch
ID. It compares six ordered images, full returned CPU observations and guest
records at 100, 50, 400, unlimited and 1 percent with uninterrupted 100-percent
execution. It checks real step completion and progress, without post-load warmup.
Input/config/producer hashes and per-policy evidence remain in the new output
directory. Sound initialization uses managed launch validation; audible device
delivery remains a separate check.

`presentation_state.py` is a fixture-specific revision-5 reader for unscaled
640-pixel XFB/EFB data. It requires the native build's LZ4 shared library and
rejects unsupported layouts. It is not a general-purpose state-file API.
