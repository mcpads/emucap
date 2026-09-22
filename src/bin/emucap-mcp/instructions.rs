/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Debug through the connected emulator adapter.

## Authority and routing

Start with `bootstrap()` and follow `primary_action`. Use full `status` for names, limits and capabilities; refresh after reconnect or generation change. Compose operations when `contracts.state=validated`.

Use `tap` for button/key input. For mouse controls describe `pointer`; for specialized controls describe `debug`. Execute a returned schema with its `known_capability_revision`. Discover optional analysis through `analysis(operation=describe)`.

## Managed lifecycle

Use launcher-owned isolated runtimes and the bound `listener.port`. Follow `launch_plan`'s system/backend selection requirements. Review each indirect media member and echo the exact returned `review_input`. After launch, verify status identity and binding.

For launch-entry evidence, inspect `start_frozen_contract`, request `start_frozen:true`, and require `state=frozen`. Treat frozen launch and advertised repeatability as separate guarantees. A running guest advances between calls.

For an unselected generation, inspect `bootstrap(include=["runtimes"])` and `reattach` its exact available entry. Use `stop(status.runtime_instance.launch_id)` to end a generation. Managed lifecycle tools own runtime files and verify generation, lease, PID and process-start identity; replacement requires verified ownership.

## Continuity and execution

Read transport, guest execution, process state, lease, binding and failure evidence separately. On timeout or disconnect, inspect continuity and `get_failure_context()` before replacement. Determine exit from process evidence. Managed `stop` also works while adapter transport is unavailable.

Start each dependent call after the preceding terminal response. Choose operations by their advertised terminal state: `pause`, `step` and `tap` return frozen; `resume` and `_while_running` leave running. Split advances at live bounds.

Pointer operations return frozen and release transient buttons. Choose movement frames to allow guest cursor polling. Release persistent input and touch holds explicitly or end their generation. Treat cleanup failure as operation failure.

Debug `record_window` owns guest time and returns frozen. Use its advertised events, limits, anchors, snapshots, filters and warmup scopes; omitted options keep producer defaults. Interpret evidence within the included callback scope. `integrity=complete` supports complete-window claims; other integrity states support partial evidence.

For a recording `state_load` origin, call `save_state(preserve_for_recording=true)` and pass its frame-boundary `snapshot_id` plus a dense movie to `record_window`; that operation owns restoration.

## Evidence

Use offsets and bounds of the selected memory region, `status.cpu_targets` and modes for CPU-aware debug, and `status.state_groups` for `get_state` selection.

Pause before `change_media` and verify the resulting media state. Advance guest-visible media transitions explicitly with `step` or `resume`. Identify media at the observation time, accounting for guest writes after attachment.

Read screenshot raster provenance separately from the capture-time guest state. Interpret breakpoint snapshots at their reported hit boundary and later reads at their own observation time. Interpret missing hits within validated monitoring coverage and integrity. Use advertised `probe` when restore/advance/read must be atomic; its terminal state is authoritative.

Use Tracking MCP for experiment records. Pass `get_rom_info.rom_sha1` unchanged to `run_start`; resolve composite identity through `launch_plan`. Establish loader consumption from runtime evidence. Record relevant mutations as interventions and observations as gates or metrics."#;
