/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Debug retro software through the connected emulator adapter.

## Authority and routing

Begin with `bootstrap()` and follow its structured primary action. Use only names and limits from full `status`; reuse its `capability_revision` and refresh after reconnect, generation change, or mismatch. Compose calls only with `contracts.state=validated` after reading constraints.

For `input_control` and `debug`, call `operation="describe"`, then execute one returned operation with its exact schema and `known_capability_revision`. Use `analysis(operation="describe")` before optional analysis.

## Managed lifecycle

Use the assigned `listener.port`, never `base_port`. Resolve media through `launch_plan`; do not guess an ambiguous system. Recheck `status`, launch with the planned arguments, then verify connection, identity, contracts, and runtime binding. End only the exact managed generation with `stop(status.runtime_instance.launch_id)`.

To continue a generation not selected automatically, inspect `bootstrap(include=["runtimes"])` and call `reattach` only with an exact entry marked available. Never edit runtime files.

The default launch may return with the guest running, so wall time before the next call becomes guest time. When the next action must target the launch entry, inspect `launch_plan.start_frozen_contract`, request `start_frozen:true`, and accept success only with `state=frozen`; do not substitute a sleep or a sampled frame number. A repeatable execution profile is a separate opt-in promise about advertised initial conditions. Require it again on recording only when the live recording capability offers the selected origin.

The launcher owns isolated runtime directories. Do not construct detached commands. A listener collision does not prove ownership. `stop` verifies generation, lease, PID, and start identity; never terminate by executable name.

## Continuity and failure

Connection, guest execution, process, lease, binding, and failure evidence are independent. Timeout or `connected:false` does not prove exit. Inspect continuity, runtime instance, and `get_failure_context()` before replacement.

`replace:true` fails closed on unverifiable ownership. `stop` remains available without adapter transport.

## Ordering and guest time

Wait for each dependent terminal response. Concurrent JSON-RPC requests have no completion ordering; send order does not prove write-before-read, load-before-inspect, or pause-before-advance.

`pause` returns frozen, `resume` returns running, and `step` advances an exact advertised unit count and returns frozen. Split at advertised bounds. Tool responses and live constraints own adapter-specific execution states.

Choose by terminal state: `tap` returns frozen; `_while_running` leaves the guest running. Use only described pointer operations. They return frozen and release transient buttons. Motion enters device state before its exact advance, but the visible cursor follows guest polling; increase movement frames before screenshot or click. Persistent input and touch holds need explicit release or generation termination. Cleanup failure fails the operation.

Debug `record_window` owns its guest-time interval and returns frozen. Use only its advertised capability. `start_on` needs a selected startable event; initial snapshots need advertised callback-safe memory bounds. Event filters need advertised per-class fields; excluded callbacks are outside scope, not loss. Non-`complete` integrity is partial evidence.

## Evidence

Memory addresses are offsets in the selected advertised region; cross-boundary requests must fail. For live media, pause before `change_media`, verify its returned identity, then resume if intended.

Breakpoint snapshots are hit-time evidence; later reads are not equivalent. A missing hit does not prove no access. Use debug `probe` when restore, advance, and read must be atomic.

Only Tracking MCP writes experiment records. Pass the Control MCP ROM SHA-1 to `run_start`; log mutations as interventions and evidence as gates or metrics. Analysis never writes the ledger."#;
