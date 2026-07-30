/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Use this Control MCP to inspect and control an emulator while debugging retro software. The connected adapter defines the available surface.

## Live authority

After connection, use only methods, memory identifiers, breakpoint domains, input names, and limits returned by a full `status`. Do not infer support from another session or these instructions. Reuse `capability_revision` as `known_capability_revision`; an `unchanged` response omits only the cached catalog, not live state. Request a full status after reconnect, generation change, or revision mismatch.

Compose network-separated operations only when `status.contracts.state` is `validated`. Read active exceptions and constraints first. Primitive availability alone does not prove that a temporal sequence is safe.

## Session lifecycle

1. Call `bootstrap()` at the start of a new task. The compact response establishes the listener. Use `include=["systems"]` for routing details or `include=["installation"]` for build and runtime paths.
2. With known content, call `launch_plan(content_path, system?)`. Ambiguous media must produce a question instead of a guessed system.
3. Recheck `status` immediately before `launch`; never cache or hard-code a listener port.
4. Call `launch` with the planned arguments, then verify connection, system identity, contract state, and runtime binding.
5. End a managed emulator with `stop(status.runtime_instance.launch_id)`.

The launcher owns isolated runtime directories. Do not construct detached launch commands. A listener collision is diagnostic, not ownership. `stop` verifies generation, lease, PID, and process-start identity; never terminate by executable name.

## Continuity and failure

Connection, guest execution, host process, lease, runtime binding, and failure evidence are independent. A timeout or `connected:false` does not prove exit or crash. Inspect continuity, runtime instance, and `get_failure_context()` before replacement.

Never edit session, generation, lease, or link files. Use `replace:true` only for an intentional replacement. It fails closed when ownership cannot be verified. `stop` remains the termination path when adapter transport is unavailable.

## Ordering and time

Wait for the terminal response of every dependent call before sending the next one. Concurrent JSON-RPC requests have no completion ordering guarantee. Never infer write-before-read, load-before-inspect, or pause-before-step from send order.

`pause` produces `frozen`; `resume` produces `running`. `step` advances from frozen and returns to frozen. `run_frames` leaves the guest running. Split work at advertised bounds. Save/load safety, screenshot freshness, host Pause behavior, and execution units are adapter-specific.

## Data, input, and evidence

Numbers accept non-negative JSON integers or strings prefixed with `0x` or `$`. Memory addresses are offsets within the selected advertised region. Cross-boundary access must fail.

`write_memory` accepts exactly one source: inline `hex`, or `input_file{path,offset?,length,sha256?}`. File input is snapshotted and hashed before adapter contact. Preserve returned size and hash when the mutation matters.

`set_input(buttons)` holds an override; `set_input([])` returns ownership to native input. Bounded input tools must release ownership on every terminal path. A cleanup failure is an operation failure.

Breakpoint snapshots are hit-time evidence; a later read is not equivalent. A missing hit alone does not prove that execution or access did not occur. Use adapter-side `probe` when atomic temporal observation matters.

Only Tracking MCP writes experiment records. Pass `get_rom_info.rom_sha1` unchanged to Tracking `run_start`. Log mutations as interventions and analysis evidence as gates or metrics. Load optional schemas with `analysis(operation="describe")`; analysis never writes the ledger.

Compare original and patched content at equivalent execution anchors. A remaining divergence is evidence to investigate, not proof of causality."#;
