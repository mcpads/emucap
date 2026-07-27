# MAME Neo Geo adapter

This experimental adapter accepts three explicit profiles: `neogeo_mvs`, `neogeo_aes`, and
`neogeo_cd`. Hyper Neo Geo 64 is not an alias. Neo Geo Pocket/Color is the separate Mednafen
`ngp` profile.

Build the dedicated pinned MAME subset and the Rust bridge:

```sh
adapters/mame-neogeo/build.sh
cargo build --release --bin emucap-mame-neogeo-bridge
```

The Neo Geo build has its own `adapters/mame-neogeo/work` tree. It does not reuse or replace the
PC-98 subset under `adapters/mame-pc98/work`. BIOS files, game sets, and disc media are user-supplied
and are never copied into this repository. For MVS, run MAME's `-verifyroms <driver>` against the
game and `neogeo.zip`; an old or decrypted set with different CRCs is not compatible merely because
its ZIP name matches. AES uses the `aes` driver, an official `aes.zip`, and the pinned
`neogeo.xml` software list from the MAME build. The cartridge ZIP stem selects an AES-compatible
software-list entry; set `EMUCAP_NEOGEO_HASH_PATH` only when that build-owned hash directory is
stored elsewhere. CD uses the `neocdz` driver, an official `neocdz.zip` BIOS (`neocd.bin` plus
`000-lo.lo`), and a CUE entry file. The launcher explicitly selects official BIOS variants and
rejects a missing CUE reference before starting. MAME's whole-set audit may still report optional
Universe BIOS variants that are not selected by these profiles.

The MCP launch path uses an emucap-owned MAME home and a dedicated 68000 bridge. `display:true`
explicitly authorizes the repository's fail-closed MAME wrapper to open a window; the default
remains headless. The supported surface includes:

- machine-global pause/resume selected through the M68000 main CPU;
- bounded profile-specific RAM reads and writes, 68000 state, instruction step, frame step, and
  run-frames;
- frozen-frame PNG capture with frame and SHA-256 provenance;
- player-one A/B/C/D and directions with explicit ownership release; MVS adds coin/start/service,
  while AES and CD add start/select;
- native MAME save/load while frozen for MVS and AES. Save returns only after the pre-save notifier
  has fired and a non-empty staged file is complete; load returns only after the post-load
  notifier and freezes the restored machine. Step one frozen frame before judging the restored
  screen. CD omits save/load because MAME 0.288 marks CDZ save states unsupported.

CD content identity uses the CUE entry file plus every unique referenced file. It includes the
declared filename, size, and bytes of each file, and reports per-file SHA-1 values so changing an
audio or data track changes the disc identity.

A maintained representative-game smoke uses Metal Slug 2 with a matching MAME 0.288 set. It
verifies RAM mutation and native-state restore, `CREDIT 0` to `CREDIT 1` after coin input, and the
transition to the `HOW TO PLAY` screen after start input. Runtime media is not stored in the
repository:

```sh
cargo run --release --example mame_neogeo_smoke -- \
  /path/to/mslug2.zip /path/to/neogeo.zip --display
```

The same smoke validates AES software-list launch, the 64 KiB RAM boundary, native state restore,
frozen screenshots, select ownership release, and the start transition:

```sh
cargo run --release --example mame_neogeo_smoke -- \
  /path/to/mslug2.zip /path/to/aes.zip --aes
```

The maintained CD smoke uses a representative multi-track disc and verifies the complete CUE
graph, the 2 MiB RAM boundary, exact frame step, frozen screenshots, and start/select input
cleanup:

```sh
cargo run --release --example mame_neogeo_cd_smoke -- \
  /path/to/disc.cue /path/to/neocdz.zip
```

One synchronous frame advance is capped by `status.execution_limits.frame.max_count`; longer
travel must be split into terminally acknowledged calls. The current adapter does not expose
breakpoints, disassembly, trace, or the Z80 state. Results for MVS, AES, CD, and Pocket/Color
remain profile-specific and do not establish Hyper Neo Geo 64 support.
