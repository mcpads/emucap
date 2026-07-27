/// Self-contained runtime guidance returned to MCP clients.
pub(crate) const SERVER_INSTRUCTIONS: &str = r#"Use this Control MCP to inspect and control a running emulator while debugging retro-game patches. It can expose memory, registers, video state, screenshots, input, save states, execution control, and debugger events. The connected adapter determines the actual surface.

**Live capability authority.** After connecting, use only methods listed in `status.methods`, memory identifiers listed in `status.memory_types`, breakpoint domains listed in `status.breakpoint_kinds`, and parameter limits listed in `status.contracts`. Do not infer support from this text, another adapter, or a platform family. `hello` is an adapter handshake and is not a user tool.

**Contract admission.** Composed operations are safe to sequence only when `status.contracts.state` is `validated`. Read `status.contracts.active_exceptions` and `constraints` before composing calls. When the state is `unreported` or `unvalidated`, the presence of primitive methods does not prove that a multi-call temporal sequence is safe.

**Ordering.** Wait for the terminal response from each dependent call before sending the next one. Concurrent JSON-RPC requests have no completion ordering guarantee. Never assume write then readback, load then inspect, or pause then step from send order alone.

[Starting and launching]
  0. Call `bootstrap()` first for every new Control MCP task. It creates the listener even when no emulator is connected and returns the current `listening_port`, `runtime_paths`, supported systems, and the next content question.
  1. If content is known, call `launch_plan(content_path, system?)`. Ambiguous media such as CUE, CHD, or BIN must produce a question instead of a guessed system.
  2. Recheck `status` immediately before launch and use that response's listener identity. Do not cache or hard-code port 47800.
  3. Prefer `launch(content_path, system?, name?, display?, sound?, replace?)`. It uses emucap-owned runtime directories on macOS, Linux, and Windows and returns only after readiness is checked. Use a fallback launcher from `runtime_paths` only when it is reported as available and the MCP launcher is unavailable on that host.
  4. Verify `status.connected`, system identity, contract state, and runtime binding before further control.

`display:true` opens a native HITL window only on adapters that support it. Mednafen audio is disabled by default and is enabled independently with `sound:true`; unsupported adapters reject that option. Never create an ad hoc `nohup` launch and never terminate by broad executable name. Stop only the identity-verified processes owned by the current port and launch generation.

Direct mode uses a distinct auto-selected port and per-port identity token for each session. Broker mode (`EMUCAP_BROKER=1`) keeps emulator connections alive across MCP sessions and can select a named emulator with `EMUCAP_NAME`. A missing listener means the Control MCP listener must be re-established; it is not a reason to relaunch an emulator immediately.

[Failure and continuity]
Connection state, guest execution state, process state, lease ownership, and preserved failure evidence are independent. A timeout or `connected:false` does not prove emulator exit or crash. Inspect `status.continuity`, `status.runtime_instance` or `status.stale_runtime_instance`, and `get_failure_context()` first. A `mismatched` or `unmanaged` runtime binding may be useful for observation, but an older runtime record is not authority to stop or replace the live process.

Do not edit runtime files to clear a lease or change generation identity. Use `launch(..., replace:true)` only for an intentional replacement; it fails closed unless PID and process-start identity can be verified. `get_failure_context` reads the last good state and host or adapter failure without issuing a new emulator request. When Flycast advertises `dismiss_failure`, inspect the quarantined state first and call it only when investigation is complete; it continues the existing termination path and does not repair the game.

[Numbers and memory writes]
Addresses, values, ranges, and lengths accept non-negative JSON integers or strings with `0x` or `$` hexadecimal prefixes. Prefer strings for hexadecimal addresses to avoid accidental decimal conversion. `write_memory` accepts exactly one source: inline `hex` for short byte strings, or `input_file{path,offset?,length,sha256?}` for an absolute raw-binary file slice. The Control MCP snapshots and hashes the file slice before contacting the adapter. Respect `status.contracts.constraints["memory.write"]`, and retain returned `input_bytes` and `input_sha256` in any intervention record.

[Execution and time]
`pause` produces `frozen`; `resume` produces `running`. `step(count, unit="frames"|"instructions", cpu?)` advances from frozen state and returns to frozen. Read allowed units and request bounds from `status.contracts.constraints`; split longer work into terminally acknowledged calls. `run_frames(n)` resumes when needed, advances at least the requested frames, and leaves the guest running. For an exact frozen frame boundary, use `pause` followed by frame `step`.

Save/load availability while frozen is adapter-specific. Mesen host API 2 allows save/load at an explicit main-CPU instruction-boundary pause and preserves the halt; frame/PPU steps and breakpoint halts may be rejected as `unsafe_halt`. A save requested while running is not an exact breakpoint-hit snapshot; use breakpoint `snapshot` fields for hit-time memory. PC-98 does not restore the screen bitmap and reports post-load screenshot freshness as unverified. Flycast rejects screenshots after load until a fresh frame exists. In either case, perform one frozen frame step before judging the restored screen.

GUI Pause behavior is adapter-specific. Prefer MCP `pause` or a documented host freeze hotkey unless the live adapter advertises an equivalent service path. Compatible Mesen builds advertising `paused_start_service` adopt a regular host Pause as `frozen`, continue servicing supported requests without further guest-time advance, and reconcile a host Resume back to `running`. `status.freeze_policy` reports the current behavior.

[Input]
Read canonical button names from `status.input_buttons` and available primitives from `status.methods`. `set_input(buttons)` holds an override until `set_input([])` returns ownership to native input. `press_buttons(buttons, frames)` owns a bounded real-time pulse and releases before its terminal response. A stopped adapter may resume automatically or reject the pulse according to its contract.

Use `tap` only when advertised for deterministic frozen one-shot input. It advances `press_frames`, releases, optionally advances `after_frames`, and returns frozen. Sequence multiple taps one at a time. `hold_until` holds buttons while advancing under a memory watch, releases at change or bound, and returns `{changed,frames,before,after}`. NDS touchscreen input uses `touch`; use the `cpu` argument for ARM9/ARM7 execution methods. Every transient-input terminal path must return ownership, so treat a cleanup failure as operation failure.

[Memory search]
`find_pattern` returns matching offsets within one named memory region and is preferable to transferring an entire large region. Omitted length searches to the region end subject to adapter bounds. Continue from a later start when `truncated:true`, and use `output_path` for large result sets. After locating a dynamic string, buffer, or table, a write breakpoint can identify its producer.

[Breakpoints and disassembly]
Use `set_breakpoint(kind, memory_type, start, end, pause_on_hit, value?, value_mask?, value_len?, pc_min?, pc_max?, snapshot?)`. The live `status.breakpoint_kinds` entries define valid kinds, address units, memory use, and snapshot support. Unsupported values fail before mutation. A pausing hit leaves the guest frozen; read queued hits with `poll_events` and manage ownership with list and clear methods.

Value filters match the accessed value, PC filters restrict the instruction causing the access, and `snapshot=["memory_type:address:length"]` captures memory in the hit callback. Absence of an exec hit alone does not prove code was not executed; WRAM execution or lost observation may require adjacent breakpoints and comparison evidence. `disassemble` decodes the connected CPU's ISA.

[Register watches and execution trace]
`watch_register` freezes on the instruction that leaves the inclusive register range and is intended for bounded hunting. When available, enable `set_trace(true)` before reproduction and then read `call_stack` and `get_trace`. Call stacks are outermost to innermost and guarantee only `.pc`; `bank` is optional and meaningful only when the adapter can identify a paged ROM bank. Disable tracing after use. `break_on_reset` detects entry into a reset handler. If these tools are absent, `status.capability_notes` may describe a bounded breakpoint, step, and disassembly substitute.

[Video and analysis]
Saturn `get_video_state`, `resolve_tile`, and `set_layer_enable` are adapter-specific and must appear in `status.methods`. Restore any temporary layer override after capture. `dump_memory(path)` writes `.bin`, `regions.json`, and `state.json` for `emucap diff`. Use `probe` for an atomic state restore, frame advance, and memory observation. `regression_run` and `verify_determinism` return analysis results but never write experiment records. Send large results to `output_path` when supported.

[Tracking MCP]
Only the separate `emucap-track-mcp` writes `.emucap/`. Call its own `bootstrap`, pass Control `get_rom_info.rom_sha1` to Tracking `run_start`, record analysis results with `log_gate` or `log_metric`, and record state-changing calls such as write, load, reset, or input with `log_intervention` when reproducibility matters. Use an external SHA-1 only when the adapter supplies no normalized content hash.

[Original and patched comparison]
Stop original and patched media at equivalent execution anchors, capture comparable dumps, and use `emucap diff` with baseline and ignore ranges for expected changes. A remaining state divergence is evidence to investigate, not automatic proof of patch causality. Broker mode can reuse one persistent connection sequentially; direct mode can use isolated sessions."#;
