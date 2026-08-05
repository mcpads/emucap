# emucap

emucap gives agents a common observation and control surface over supported emulators. Installation,
supported platforms, prerequisites, and adapter-specific build instructions are in `README.md` and
`adapters/*/README.md`.

## MCP entrypoints

Register both servers:

- `emucap-mcp` controls a live emulator.
- `emucap-track-mcp` stores experiment records and is the sole `.emucap/` writer.

Start each new Control task with compact `bootstrap()`. Use `include=["systems"]` for the full
routing catalog or `include=["installation"]` for build/runtime paths. With known content, call
`launch_plan`, then the managed `launch`, and verify a fresh `status`.

Pass `get_rom_info.rom_sha1` unchanged to Tracking `run_start`. Record relevant writes, loads,
resets, and input with `log_intervention`; record analysis evidence with `log_gate` or `log_metric`.
Control-side analysis tools do not write the ledger.

## Runtime safety

The full live `status` is the capability authority. Use only advertised methods, memory regions,
breakpoint domains, inputs, and contract limits. A later status may pass
`known_capability_revision`; an `unchanged` response omits the cached catalog but retains live
execution and continuity state.

The static Control tool list is a basic remote. Memory writes, disassembly, call stacks,
breakpoints, event polling, and live media changes are direct tools. Call `input_control(operation="describe")` for
persistent or device-specific input, `debug(operation="describe")` for composite or device-specific
debugger operations, and `analysis(operation="describe")` for optional analysis. Execute a returned
operation through the same drawer with its capability revision.

Treat connection, guest execution, process state, lease ownership, runtime binding, and preserved
failure evidence as separate facts. A timeout or disconnected socket does not prove emulator exit.
Inspect continuity, `runtime_instance` or `stale_runtime_instance`, and `get_failure_context` before
replacement.

Never edit runtime ownership files, build detached launch commands, or terminate by executable
name. Use `replace:true` only for intentional replacement. End a managed generation with
`stop(status.runtime_instance.launch_id)`; it verifies generation, lease, PID, and process-start
identity and fails closed on uncertainty.

Wait for each dependent MCP call to finish before issuing the next one. Network request order does
not prove write/read, load/inspect, or pause/advance order. Read adapter contracts before composing
time-sensitive primitives.

## Build and reconnect

Use release binaries for registered MCP servers. After changing MCP instructions, schemas, tools,
or runtime responses, rebuild the affected release binaries and reconnect both servers. A debug
build does not update a running release process.
