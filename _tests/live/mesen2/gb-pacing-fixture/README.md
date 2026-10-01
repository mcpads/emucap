# GB/GBC pacing and raster fixture

Builds a ROM-only cartridge with the pinned typed SM83 assembler. All executable
instructions use checked typed encoding. The operator supplies the boot logo from
a local cartridge; no logo or generated ROM is distributed here. The output
extension selects DMG (`.gb`) or CGB-compatible (`.gbc`) header and palette writes.

The cartridge clears VRAM with LCD disabled, enables a blank tile background,
and busy-polls LY. At each VBlank it increments the u16 field counter at `0xc100`
and, when A is held, the u16 input counter at `0xc102`. It alternates the background
color every two fields. Interrupts stay disabled and the loop never uses HALT.
The expected tap deltas are `(press_frames + 1 + after_frames, press_frames)`,
modulo 65536. No-input control leaves the input counter unchanged.

```sh
CARGO_TARGET_DIR=out/gb-pacing-fixture/build cargo +1.98.0 run --offline --locked \
  --manifest-path _tests/live/mesen2/gb-pacing-fixture/Cargo.toml \
  -- HEADER_SOURCE.gb out/gb-pacing-fixture/pacing.gb
python3 _tests/live/mesen2/pacing-counters.py \
  --profile PROFILE.json --output out/gb-pacing-fixture/counters
python3 _tests/live/pacing_images.py \
  --profile PROFILE.json --output out/gb-pacing-fixture/images
```

Use a private profile with `launch_plan.system` set to `gb` or `gbc`, generated
content path, `launch.start_frozen: true`, and `warmup_frames: 300`. Counter identity
reads start at cartridge address `0x100`, outside the boot-ROM overlay. The image
witness requires visible changes and includes the first image after restore.
This fixture covers palette/background history and exact field/input counts;
it does not cover every tile, sprite, link-cable or SGB rendering mode.
