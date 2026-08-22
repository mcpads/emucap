/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Debug retro software through the connected emulator adapter.

## Authority and routing

Begin with `bootstrap()` and follow its primary action. Use names and limits from full `status`; refresh after reconnect, generation change, or mismatch. Compose calls only with `contracts.state=validated`.

For `input_control` and `debug`, call `operation="describe"`, then use one returned schema and `known_capability_revision`. Describe optional analysis before use.

## Managed lifecycle

Use `listener.port`, not `base_port`. Resolve media through `launch_plan`; never guess an ambiguous system. Follow `review_input`: verify each indirect member, then echo its exact approval. Never construct or bypass it. Recheck `status`, launch with the planned arguments, and verify connection, identity, contracts, and binding. Stop only `status.runtime_instance.launch_id`.

To continue a generation not selected automatically, inspect `bootstrap(include=["runtimes"])` and call `reattach` only with an exact entry marked available. Never edit runtime files.

Default launch may return running, so delay before the next call becomes guest time. To target launch entry, inspect `start_frozen_contract`, request `start_frozen:true`, and require `state=frozen`; never substitute sleep or a sampled frame. Repeatability is a separate advertised opt-in and must be required again for recording.

The launcher owns isolated runtime directories. Do not construct detached commands. A listener collision does not prove ownership. `stop` verifies generation, lease, PID, and start identity; never terminate by executable name.

## Continuity and failure

Connection, guest execution, process, lease, binding, and failure evidence are independent. Timeout or `connected:false` does not prove exit. Inspect continuity, runtime instance, and `get_failure_context()` before replacement.

`replace:true` fails closed on unverifiable ownership. `stop` remains available without adapter transport.

## Ordering and guest time

Wait for each dependent terminal response. Concurrent JSON-RPC requests have no completion ordering; send order does not prove write-before-read, load-before-inspect, or pause-before-advance.

`pause` returns frozen, `resume` returns running, and `step` advances an exact advertised unit count and returns frozen. Split at advertised bounds. Tool responses and live constraints own adapter-specific execution states.

Choose by terminal state: `tap` returns frozen; `_while_running` leaves the guest running. Use only described pointer operations. They return frozen and release transient buttons. Motion enters device state before its exact advance, but the visible cursor follows guest polling; increase movement frames before screenshot or click. Persistent input and touch holds need explicit release or generation termination. Cleanup failure fails the operation.

Debug `record_window` owns guest time and returns frozen. Use advertised event classes, limits, startable anchors, callback-safe snapshots, filter fields, and warmup scope pairs. Excluded callbacks are outside scope, not loss; omission keeps producer defaults. Non-`complete` integrity is partial evidence.

## Evidence

Memory addresses are offsets in the selected advertised region; cross-boundary requests must fail. For live media, pause before `change_media`, verify its returned identity, then resume if intended.

Breakpoint snapshots are hit-time evidence; later reads are not equivalent. A missing hit does not prove no access. Use debug `probe` when restore, advance, and read must be atomic.

Only Tracking MCP writes records. Pass `get_rom_info.rom_sha1` unchanged to `run_start`. For composite media, follow `launch_plan`; reject graph failures and descriptor-only hashes. A prelaunch content identity does not prove loader consumption. Log mutations as interventions and evidence as gates or metrics."#;
