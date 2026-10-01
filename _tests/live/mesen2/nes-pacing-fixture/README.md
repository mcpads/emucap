# NES pacing and raster fixture

Builds an independently authored NROM cartridge with the pinned typed RP2A03
assembler. Every executable instruction uses checked typed encoding. No original
game or BIOS bytes are needed. The iNES header and interrupt vectors are data.

```sh
CARGO_TARGET_DIR=out/nes-pacing-fixture/build cargo +1.98.0 run --offline --locked \
  --manifest-path _tests/live/mesen2/nes-pacing-fixture/Cargo.toml \
  -- out/nes-pacing-fixture/pacing.nes
python3 _tests/live/mesen2/pacing-counters.py \
  --profile PROFILE.json --output out/nes-pacing-fixture/counters
python3 _tests/live/pacing_images.py \
  --profile PROFILE.json --output out/nes-pacing-fixture/images
```

Use `launch_plan.system: "nes"`, generated cartridge path, frozen launch and
300 warmup frames. NMI increments a u16 field counter at CPU RAM `0x100` and a
u16 held-A counter at `0x102`, then changes the backdrop every two fields.
The main loop continuously executes. NMI preserves A and returns normally;
maskable IRQs, APU channels and tile/sprite rendering are disabled.

For a tap, expected deltas are `(press_frames + 1 + after_frames, press_frames)`
modulo 65536. No-input control leaves the input counter unchanged. Polling PPU
status is used only for initial stabilization; per-frame polling can suppress
a VBlank edge and is unsuitable for an exact field oracle.

The image witness compares immediate restore and six continued images with no
postload warmup. This fixture covers backdrop raster history and field/input
units; it does not qualify every mapper, HD pack, VS composition or video filter.
