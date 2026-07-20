use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply, ProcessDependency};
use emucap::ppsspp_bridge::{PpssppBridge, TungsteniteWs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: emucap-ppsspp-bridge <EMUCAP_PORT> <PPSSPP_DEBUGGER_PORT>");
        eprintln!(
            "  PPSSPP_DEBUGGER_PORT is the port PPSSPPHeadless/PPSSPP was launched with (--debugger=PORT),"
        );
        eprintln!("  which opens ws://127.0.0.1:<port>/debugger.");
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let ppsspp_port = parse_port(&args[2]).context("invalid PPSSPP_DEBUGGER_PORT")?;

    eprintln!(
        "[ppsspp-rust] connecting ppsspp=127.0.0.1:{ppsspp_port} emucap=127.0.0.1:{emucap_port}"
    );
    // 8s default, not 5s: the emucap fork's `emucap.screenshot` has its own internal 5.0s wait for
    // GE stepping (`WebSocketGPUBufferEmucapScreenshot`'s `timeoutSeconds`) before it replies with an
    // error event. A 5s socket read timeout here would race that; the client's
    // own read can time out (`bridge_error`/IO) a few ms ahead of PPSSPP's reply. Every bridge
    // request is ticket-correlated, so a late reply cannot be attributed to a later command;
    // comfortably outlasting PPSSPP's known worst-case wait still avoids a false timeout.
    //
    // 8s does NOT cover savestate: the fork's `SaveStateSubscriber.cpp` waits up to 15s for the save/
    // load to complete before replying, so `save_state`/`load_state` get a dedicated per-call read
    // budget above 15s (`SAVESTATE_READ_TIMEOUT`, threaded via `call_with_timeout`) instead of raising
    // this default — every other read keeps the fast 8s so ordinary failures still surface quickly.
    let ws = TungsteniteWs::connect(ppsspp_port, Duration::from_secs(8))
        .with_context(|| format!("connect PPSSPP debugger websocket at 127.0.0.1:{ppsspp_port}"))?;
    let mut bridge = PpssppBridge::new(ws);
    let dependency =
        ProcessDependency::from_process_env().context("load emulator process dependency")?;

    serve_reconnecting_controlled(
        emucap_port,
        "ppsspp-rust",
        move |request| {
            let response = bridge.handle_request(request);
            if bridge.backend_terminal() {
                BridgeReply::terminate_with(response)
            } else {
                BridgeReply::continue_with(response)
            }
        },
        move || {
            dependency
                .as_ref()
                .and_then(ProcessDependency::terminal_reason)
        },
    )
    .context("serve reconnecting emucap session")
}

fn parse_port(raw: &str) -> anyhow::Result<u16> {
    raw.parse::<u16>()
        .map_err(|_| anyhow!("expected decimal TCP port, got {raw:?}"))
        .and_then(|port| {
            if port == 0 {
                Err(anyhow!("port must be in 1..=65535"))
            } else {
                Ok(port)
            }
        })
}
