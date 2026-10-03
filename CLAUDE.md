# emucap

emucap lets agents observe and control supported emulators. Installation and core packages are
in `README.md`; adapter prerequisites and builds are in `adapters/*/README.md`.

Register both MCP servers with their release binaries:

- `emucap-mcp`: live emulator control.
- `emucap-track-mcp`: experiment records and the sole `.emucap/` writer.

## Start a task

Call `bootstrap()` and follow `primary_action`. Request `include=["systems"]` for routing details
or `include=["installation"]` for build paths. With known content, use `launch_plan`, managed
`launch`, then verify identity and binding with a fresh full `status`.

Full `status` is the authority for available methods, memory regions, CPU targets and limits.
Refresh after reconnect or generation change. Pass `known_capability_revision` on repeated status
requests to retain current execution state with a compact catalog response.

## Operate

Use `tap` for button/key input, including long holds and simultaneous buttons: `press_frames` sets
the hold duration and `after_frames` the advance after release. Describe `pointer` for mouse controls or `debug` for specialized
controls, then execute with the returned schema and `known_capability_revision`. Describe
`analysis` for optional analysis. Tool descriptions and live responses supply operational details.

For several memory ranges, pause a running guest and use advertised `read_memory_batch` within
its live windows and limits. For collection speed or human handoff, describe `debug`, query
`execution_speed`, set an advertised rate, and check the returned policy and state.

Wait for each dependent call's terminal response and check its execution state and cleanup
outcome. On timeout or disconnect, inspect continuity and `get_failure_context()` before choosing
recovery. Establish process exit from process evidence. Use managed lifecycle tools for attachment
and replacement; end a generation with `stop(status.runtime_instance.launch_id)`.

Pass `get_rom_info.rom_sha1` unchanged to Tracking `run_start`. Record relevant mutations with
`log_intervention` and observations with `log_gate` or `log_metric`.

## Develop

After changing MCP instructions, schemas, tools or responses, rebuild the affected release
binaries and reconnect the registered servers.
