/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Debug through the connected emulator adapter.

## Start and select

Start with `bootstrap()` and follow `primary_action`. Full `status` supplies names, limits and capabilities; refresh after reconnect or generation change. Compose operations when `contracts.state=validated`.

Use `tap` for button/key input. Describe `pointer` for mouse controls or `debug` for specialized controls, then execute with the returned schema and `known_capability_revision`. Describe `analysis` for optional analysis.

## Manage the runtime

Use `launch_plan` and managed `launch`. Review indirect media members and submit the returned `review_input`. Verify identity and binding after launch; use the bound `listener.port`.

For launch-entry evidence, check `start_frozen_contract`, request `start_frozen:true`, and verify `state=frozen`. Check repeatability separately.

Select an existing generation through `bootstrap(include=["runtimes"])` and `reattach` its exact entry. End a generation with `stop(status.runtime_instance.launch_id)`, including while adapter transport is unavailable.

On timeout or disconnect, inspect `status` continuity and `get_failure_context()` before choosing recovery. Distinguish guest execution, transport, process, lease and binding state; establish exit from process evidence.

## Control and observe

Wait for each dependent call's terminal response and verify its execution state and successful cleanup. Split advances at live bounds. A running guest advances between calls.

For collection speed or human handoff, describe `debug`, query `execution_speed`, set an advertised rate, and check the returned policy and state.

For several memory ranges, pause a running guest and use advertised `read_memory_batch` within its live windows and limits. Use region-relative offsets, `status.cpu_targets` for CPU/mode selection, and `status.state_groups` for `get_state`.

Pause before `change_media`, verify the resulting media state, then advance guest-visible transitions with `step` or `resume`. Account for guest writes when identifying attached media.

Use advertised `probe` for atomic restore/advance/read. Read screenshot raster provenance separately from capture-time guest state; breakpoint snapshots and later reads each describe their own boundary. Interpret absent hits within validated monitoring coverage and integrity.

## Record evidence

Use `debug(operation="record_window")` for bounded guest-time captures; `debug(operation="describe")` supplies setup and interpretation rules.

Use Tracking MCP for experiment records. Pass `get_rom_info.rom_sha1` unchanged to `run_start`; resolve composite identity through `launch_plan`. Record relevant mutations as interventions and observations as gates or metrics."#;
