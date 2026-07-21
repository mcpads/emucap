# MAME Neo Geo adapter

This experimental adapter currently accepts only `neogeo_mvs`. `neogeo_aes`, Neo Geo CD,
Pocket/Color, and Hyper Neo Geo 64 require separate media and runtime validation and are not aliases.

The structural PoC inspects MAME's live machine without editing user configuration:

```sh
mame -rompath /path/to/romsets neogeo \
  -noreadconfig -window -video none -sound none \
  -debug -debugger none -autoboot_script adapters/mame-neogeo/poc/inspect.lua \
  -seconds_to_run 3
```

The BIOS and game sets are user-supplied and are never copied into this repository. A successful
BIOS-only structural probe does not establish game input, screenshot, or save-state support.

The MCP launch path uses an emucap-owned MAME home and a dedicated 68000 bridge. BIOS-only runtime
validation covers launch, global pause, MVS work-RAM read/write/restore, exact frame and 68000
instruction stepping, screenshot capture, input cleanup, reset, and region-boundary rejection.
A representative game-set run and native save/load completion proof are still required; those
features are not claimed by the BIOS-only result.
