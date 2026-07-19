# emu-monitor-hitl-adaptor (emucap)

Emulator **capture** infrastructure for debugging retro-game patches (emucap = emulator capture).
"Capture" here is not a narrow snapshot but a *broad surface for capturing a running emulator's
observable and reachable state* — memory, registers, screen, VDP, events; reach (writes, input,
breakpoints, freeze-step, save-states); case bundles; and an experiment ledger. It helps an AI analyze
a problem a human described, and supports several emulators through a common Core plus adapters.

Install and build → `README.md` (agent-driven install: prerequisites, Core build, MCP registration,
bootstrap handoff). Per-emulator adapters → each `adapters/*/README.md`.

## Binaries

emucap is split into **two MCP servers — a Control MCP and a Tracking MCP**. Register both; the agent
composes them (they do not call each other).

- `emucap` — case-bundle CLI (`finalize`/`inspect`) + tracking-ledger CLI
  (`track ls|show|compare|summarize|reindex|import`).
- `emucap-mcp` — the **Control MCP**. Reads and controls a running emulator (stdio): memory, state,
  screen, input, breakpoints, save-states, atomic frame-boundary probes, plus analysis verbs
  (`regression_run`/`verify_determinism`, which only *return* results — they do not write to the
  ledger). Port `EMUCAP_PORT` (default 47800).
- `emucap-track-mcp` — the **Tracking MCP**. An emulator-less server (stdio) that is the single writer
  of the experiment ledger (`.emucap/`): `run_start`/`run_finish`/`log_metric`/`log_gate`/
  `log_finding`/`log_artifact`/`set_reproduction`/`log_intervention`/`query_runs`/`get_run`/
  `compare_runs`/`summarize_runs` (+ `bootstrap`). Ledger location `EMUCAP_TRACK_ROOT` (default
  `.emucap` at the working repo's git root).
- `emucap-broker` — multi-session connection sharing (for the Control MCP).
- `emucap-mame-pc98-bridge` — PC-98 launch helper used by the Rust MAME launcher.
- `emucap-desmume-nds-bridge` — NDS launch helper used by the Rust DeSmuME launcher.
- `emucap-ppsspp-bridge` — PSP launch helper used by the Rust PPSSPP launcher.
- `emucap-pcsx2-bridge` — PS2 launch helper used by the Rust PCSX2 launcher.

## MCP operating notes

Start every emucap task with the MCP `bootstrap`. **Each MCP has its own `bootstrap`** — the Control
MCP's returns `listening_port`, `runtime_paths`, supported systems, and questions to ask (emulator
entry); the Tracking MCP's returns `ledger_path`, the active run, and orphaned running runs (ledger
entry). Do not hunt for an emucap directory locally or guess a launch from the `status` command
template alone.

Tracking tools (run/log/query) live **only on the Tracking MCP** — do not call them from the Control
MCP. Read `rom_sha1` from the Control MCP's `get_rom_info` and pass it to the Tracking MCP's
`run_start`; record analysis-verb results with `log_gate` and state-changing interventions with
`log_intervention`.

Treat transport, execution, and evidence as separate states. A timeout or disconnected socket does
not prove that the emulator exited: inspect `status.continuity`, `status.runtime_instance`, and
`get_failure_context` before launching again. Reattach to a live owned generation; use
`launch(..., replace: true)` only for an intentional identity-verified replacement. Flycast can hold
an exact fatal snapshot in read-only quarantine; inspect it first, then call `dismiss_failure` only
when the connected adapter advertises that method.

If tool discovery lacks the Control MCP's `bootstrap`/`launch_plan`, or `status` has no
`runtime_paths` (or the Tracking MCP's `run_start` is missing), the running release is stale. Rebuild
with `cargo build --release --bin emucap --bin emucap-mcp --bin emucap-track-mcp --bin emucap-broker --bin emucap-mame-pc98-bridge --bin emucap-desmume-nds-bridge --bin emucap-ppsspp-bridge --bin emucap-pcsx2-bridge`
and restart both MCPs.
