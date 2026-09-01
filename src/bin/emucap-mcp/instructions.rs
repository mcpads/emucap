/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Debug through the connected emulator adapter.

## Authority and routing

Start with `bootstrap()` and follow `primary_action`. Take names/limits from full `status`; refresh after reconnect or generation change. Compose only when `contracts.state=validated`.

Describe `input_control` or `debug` before use; then use one returned schema and its `known_capability_revision`. Describe optional analysis too.

## Managed lifecycle

Use `listener.port`, not `base_port`. Run `launch_plan`; do not guess systems or silently accept backend recommendations: copy the explicit argument. Verify each indirect member and echo its exact `review_input`; never invent one. After launch, verify status identity/binding. Stop only `status.runtime_instance.launch_id`.

To continue an unselected generation, inspect `bootstrap(include=["runtimes"])`; `reattach` only an exact available entry. Never edit runtime files.

Default launch may return running, so delay advances guest time. To target launch entry, inspect `start_frozen_contract`, request `start_frozen:true`, and require `state=frozen`; never substitute sleep or a sampled frame. Recording repeatability is a separate advertised opt-in.

Use only launcher-owned isolated runtimes. Port collision does not prove ownership. `stop` verifies generation, lease, PID, and start identity; never terminate by executable name.

## Continuity and failure

Connection, guest state, process, lease, binding, and failure evidence are independent. Timeout or `connected:false` does not prove exit. Inspect continuity and `get_failure_context()` before replacing.

`replace:true` fails closed on unverifiable ownership. `stop` remains available without adapter transport.

## Ordering and guest time

Wait for each dependent terminal response. Concurrent JSON-RPC has no completion order; send order proves no write-before-read, load-before-inspect, or pause-before-advance.

`pause` returns frozen, `resume` returns running, and `step` advances an exact advertised unit count and returns frozen. Split at advertised bounds. Tool responses and live constraints own adapter-specific execution states.

Choose by terminal state: `tap` returns frozen; `_while_running` leaves running. Use only described pointer operations; they return frozen and release transient buttons. Motion precedes its exact advance, but visible cursor updates follow guest polling; increase movement frames before screenshot or click. Persistent input and touch holds need explicit release or generation termination. Cleanup failure fails the operation.

Debug `record_window` owns guest time and returns frozen. Use advertised events, limits, anchors, snapshots, filters, and warmup scopes. Excluded callbacks are outside scope, not loss; omission keeps producer defaults. Non-`complete` integrity is partial.

For advertised `state_load`, call `save_state(preserve_for_recording=true)`, then give its frame-boundary `snapshot_id` and a dense movie to `record_window`. Never supply paths/hashes or load manually.

## Evidence

Memory addresses are offsets in the selected region; cross-boundary requests fail. For live media, pause before `change_media` and verify its state. Writable slots may report busy frames or a guest-visible transition; `change_media` never advances them. Attach-time hashes do not identify later guest writes. Step or resume explicitly.

Use `status.cpu_targets` and its modes for CPU-aware debug; use `status.state_groups` to filter `get_state`. Unknown selectors fail before execution.

Breakpoint snapshots are hit-time evidence; later reads are not. A missing hit does not prove no access. Use advertised `probe` for atomic restore/advance/read; its terminal state is authoritative.

Only Tracking MCP writes records. Pass `get_rom_info.rom_sha1` unchanged to `run_start`. For composite media, follow `launch_plan`; reject graph failures and descriptor-only hashes. Prelaunch identity does not prove loader consumption. Log mutations as interventions and evidence as gates or metrics."#;
