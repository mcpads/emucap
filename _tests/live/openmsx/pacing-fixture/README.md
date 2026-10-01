# openMSX pacing cartridge

This synthetic 16 KiB cartridge uses the pinned typed Z80 assembler and its checked label
resolution/encode-decode verification. It contains no game or firmware bytes. It polls the
hardware VBlank latch, samples keyboard A once per field, alternates the border, and configures
a fixed PSG tone. BIOS initializes the machine before entering the cartridge.

| Guest memory | Meaning |
| --- | --- |
| `0xC100..0xC101` | Field counter, little-endian u16 |
| `0xC102..0xC103` | Fields with A held, little-endian u16 |
| `0xC104` | Last border colour, alternates 1 and 15 |

Counters wrap modulo 65536. From one saved origin, `tap(a, press_frames=p, after_frames=q)`
must increase them by `(p + 1 + q, p)`; advancing the same frames without input increases
only the field counter. Pacing policy changes must preserve these guest deltas.

From the repository root:

```sh
mkdir -p out/openmsx-pacing
CARGO_TARGET_DIR=out/openmsx-pacing/build cargo run --locked \
  --manifest-path _tests/live/openmsx/pacing-fixture/Cargo.toml \
  -- out/openmsx-pacing/pacing.rom
python3 _tests/live/openmsx/pacing_counters.py \
  --profile PROFILE.json --output out/openmsx-pacing/counters
```

Use Rust 1.98 or the project's selected toolchain. Visible capture checks require Pillow. The JSON profile has the same shape as
`_tests/live/input_pacing.py`: `launch_plan` points to the generated cartridge with one of
`msx`, `msx1`, `msx2`, `msx2p`; `launch` selects `display` and `sound`; `env` supplies the
operator's firmware inventory when required. The counter witness admits the ROM at execution address `0x4010`, verifies the mapped
cartridge bytes, and advances 60 fields before saving its origin. This avoids mistaking BIOS
input activity for fixture execution. For the generic witnesses, select a warmup beyond the
measured ROM entry (400 frames covers the qualified machines).

Run the general input, observation/lifecycle and slow-control witnesses separately for RAM/VRAM
coverage, invalid-request behavior and repeated minimum-speed response latency. Those checks
complement the exact counter oracle. Results bind one machine and host profile; they do not
establish commercial-title compatibility or acoustic fidelity.
