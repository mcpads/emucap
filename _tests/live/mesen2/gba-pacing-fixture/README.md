# GBA pacing and raster fixture

This generator uses the pinned typed ARM7TDMI assembler. Every executable instruction
is encoded and decoded for verification. The ROM runs a busy VCOUNT polling loop,
increments one u32 field counter and one u32 held-A counter, and changes the backdrop
between red and white every two fields. Mode 0 has no enabled background/object
layers. It never invokes BIOS Halt, so visible-raster instruction snapshots are
reachable without changing the producer's safe-save rules.

| EWRAM address | Meaning |
| --- | --- |
| `0x02000000` | VBlank count, little-endian u32 |
| `0x02000004` | Fields with A held, little-endian u32 |

For `tap(a, press_frames=p, after_frames=q)`, counter deltas are `(p+1+q, p)`.
Without input the second delta is zero. Counters wrap modulo 2^32.

A user-supplied GBA ROM supplies the boot header only. The entry instruction, title,
game code, checksum and all program code are generated. The boot logo/header bytes
are not distributed here. Keep the supplied ROM, BIOS and generated cartridge in
private output; the generator does not modify the input file.

```sh
mkdir -p out/gba-pacing-fixture
CARGO_TARGET_DIR=out/gba-pacing-fixture/build cargo +1.98.0 run --offline --locked \
  --manifest-path _tests/live/mesen2/gba-pacing-fixture/Cargo.toml \
  -- HEADER_SOURCE.gba out/gba-pacing-fixture/pacing.gba
python3 _tests/live/mesen2/pacing-counters.py \
  --profile PROFILE.json --output out/gba-pacing-fixture/counters
python3 _tests/live/pacing_images.py \
  --profile PROFILE.json --output out/gba-pacing-fixture/images
```

Use the standard private witness profile with `launch_plan.system: "gba"`, generated
content path, `launch.start_frozen: true`, `warmup_frames: 300`, and the operator's `EMUCAP_GBA_BIOS` override.
For visible-raster images add
`origin_raster: {"scanline":"ppu.scanline","first":32,"last":120}`.
Use 301 warmup frames for the alternate buffer trial and inspect the saved buffer
index and recorded origin; reaching a requested window is not itself proof of the
final saved boundary. The image witness compares the immediate and six consecutive
images, disturbing the live buffer before each restore. LCD blending stays enabled.

This fixture covers exact field/input units and pixel/history preservation at its
selected origins. It does not exercise every GBA tile, affine, bitmap or DMA mode;
commercial-title, native writer and device tests remain complementary evidence.
