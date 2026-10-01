# SMS pacing cartridge

Synthetic 32 KiB SMS cartridge with no game or firmware bytes. The pinned typed Z80
assembler checks instruction encoding and label resolution. The fixture initializes
the stack and VDP mode, polls the VBlank status latch, and samples controller button 2
once per field. IRQs are disabled. Initialized backdrop colours alternate red/white. A fixed PSG tone exercises the native audio path.

| CPU address | Meaning |
| --- | --- |
| `0xC100..0xC101` | Field counter, little-endian u16 |
| `0xC102..0xC103` | Fields with button 2 held, little-endian u16 |
| `0xC104` | Alternating border register value |

The field and held-input counters must advance by `(p + 1 + q, p)` for a frozen
`tap(a, press_frames=p, after_frames=q)`. Without input only the field counter
advances. Compare from one saved instruction boundary; counters wrap modulo 65536.
This is a scheduler/input fixture, not a commercial-game compatibility test.

```sh
mkdir -p out/sms-pacing
CARGO_TARGET_DIR=out/sms-pacing/build cargo +1.98.0 run --offline --locked \
  --manifest-path _tests/live/mesen2/pacing-fixture/Cargo.toml \
  -- out/sms-pacing/pacing.sms
```

Use the generic observation and input witnesses with system `sms`, the generated
file as `content_path`, `warmup_frames: 60` and `tap_buttons: ["a"]`. Keep profiles
and results outside the source tree. The fixture's RAM oracle does not establish
image preservation or physical audio delivery.

Run the exact counter oracle with the same profile:

```sh
python3 _tests/live/mesen2/pacing-counters.py \
  --profile PROFILE.json --output out/sms-pacing/counters
```

It checks 1%, 50%, 100%, 400%, 10000% and unlimited, a no-input control,
restored counter values, and exact final frame deltas.

`_tests/live/pacing_images.py` uses the same profile to compare the immediate saved
image and six consecutive images across policies. It retains mismatches, including
those occurring at fixed 100%; see the dated Mesen ordered-image investigation.

An output ending in `.gg` selects Game Gear CRAM encoding and holds each colour
for two fields, keeping motion observable with the default LCD blending filter.
The field/input counters keep the same meaning. `pacing_images.py` accepts an
optional `origin_raster` profile object (`scanline`, `first`, `last`) to select a
visible scanline before saving; it records the actual origin state.
