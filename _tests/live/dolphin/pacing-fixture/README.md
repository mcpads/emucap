# Dolphin controlled pacing fixture

Generate a synthetic GameCube DOL with the pinned typed Gekko assembler:

```sh
cargo +1.98.0 run --locked --offline \
  --manifest-path _tests/live/dolphin/pacing-fixture/Cargo.toml -- \
  out/dolphin-pacing.dol
python3 _tests/live/dolphin/pacing_counters.py \
  --content out/dolphin-pacing.dol --cpu 4 --output out/dolphin-counter-check
```

The generator writes DOL bytes and a sibling JSON layout. The witness uses the
existing managed Dolphin launcher and local `target/release/emucap-mcp`; the
normal native-binary override remains available. CPU selections are 0
(Interpreter64), 4 (JITARM64) and 5 (Cached Interpreter). The default uses ordinary dual-core execution; `--single-core` selects the
native single-core path. Each witness verifies the actual threading mode and
owns its HOME, launch generation and stop. The default HLE run needs no native firmware, game discs or SDK libraries.
The fixture does not contain game-derived code or data.

Add `--display` and/or `--sound` to select native output. Use `--dsp lle
--dsp-rom-dir PATH` for LLE, with `dsp_rom.bin` and `dsp_coef.bin` in that
explicit directory. The witness copies them into its owned user directory and
binds their hashes. It verifies the actual CPU/DSP selection and initialized
audio backend; this CPU/VI/SI fixture does not measure DSP program execution
or audible sample delivery.

## Guest work and clocks

The CPU configures a 320x240 XFB in main RAM and changes its first pixel pair once
per complete VI beam-counter scan. It issues no GPU FIFO commands or PE completion
tokens. The actual native GPU-thread determinism flag must remain false in the
ordinary dual-core profile. Host renderer presentation and native VI scheduling
remain active; this is not a deterministic-GPU-mode substitute.

A full beam-counter scan contains two 525-half-line fields in this configuration.
Each half-line has 429 VI samples. The guest explicitly selects the 27 MHz VI clock. With GC CPU
486 MHz and timebase divisor 12, a sample accounts for three timebase ticks:

```text
TB_per_scan = 2 * 525 * 429 * (2 * 486 MHz / 27 MHz / 12) = 1,351,350
```

A scan changes the CPU-backed XFB once, so one non-duplicate presented-frame
completion corresponds to two VI fields here. The witness separately checks
presented work, the guest scan counter and the native VI count.

Each scan records six big-endian words at the metadata's record address:

1. Complete VI scans observed by the guest.
2. Scans where SI channel zero reports A pressed without a transfer error.
3. Timebase sample before the previous VI-beam read (lower edge bound).
4. Raw SI channel-zero high word, including native error bits.
5. Newly observed beam position.
6. Timebase sample after the current VI-beam read (upper edge bound).

The SI error bit matters: native handback can mean a disconnected controller,
whose stale button bits are not a consumed press. Counting requires valid data.
The guest reads its real SI device; the witness uses the public `tap` operation.

## Oracles

From one retained instruction-boundary snapshot, `tap(A,2,3)` asks for six
presented completions, including the release edge. Required outcomes are six
guest scans, two consumed-input scans and twelve native VI fields. A no-input
six-frame control must consume zero presses. Repeated 100%, 50%, 400%, 10000%,
unlimited and 1% cases must preserve the entire guest record and CPU register
observation for the same input. All six record words and the full returned CPU register map participate.

A polling timestamp is not the exact VI edge. For the origin bracket `[L0,U0]`
and endpoint bracket `[L1,U1]`, with unsigned 32-bit wrap handled explicitly:

```text
L1 - U0 <= 6 * TB_per_scan <= U1 - L0
0 <= Ui - Li < TB_per_half_line
same_origin && same_input && different_pacing => identical_record_and_registers
```

The bracket comes from guest instructions surrounding the actual beam reads.
It is not a tolerance chosen from observed error. The earlier point-timestamp
candidate failed in Interpreter by one tick because input-dependent code shifted
the sampling instant; that failed witness remains in the journal evidence.

This fixture checks controlled guest timing/input behavior. Title-specific cold
restoration and off-thread GPU completion histories retain their own regressions.
Wii/IOS input, DSP execution, audio delivery and other hosts require separate
evidence. Output/backend combinations are recorded in the dated runtime journal.
