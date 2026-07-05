use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::live::protocol::{ProtocolError, Request, Response};
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
    // error event. A 5s socket read timeout here would race that — verified live: the client's
    // own read can time out (`bridge_error`/IO) a few ms ahead of PPSSPP's reply, which then
    // arrives unread on the socket and gets misattributed as an error to whatever unrelated
    // request comes next (this transport demuxes by event name only, no per-request id). Comfortably
    // outlasting PPSSPP's known worst-case wait avoids that race in the common case.
    //
    // 8s does NOT cover savestate: the fork's `SaveStateSubscriber.cpp` waits up to 15s for the save/
    // load to complete before replying, so `save_state`/`load_state` get a dedicated per-call read
    // budget above 15s (`SAVESTATE_READ_TIMEOUT`, threaded via `call_with_timeout`) instead of raising
    // this default — every other read keeps the fast 8s so ordinary failures still surface quickly.
    let ws = TungsteniteWs::connect(ppsspp_port, Duration::from_secs(8))
        .with_context(|| format!("connect PPSSPP debugger websocket at 127.0.0.1:{ppsspp_port}"))?;
    let mut bridge = PpssppBridge::new(ws);

    let sock = TcpStream::connect(("127.0.0.1", emucap_port))
        .with_context(|| format!("connect emucap MCP listener at 127.0.0.1:{emucap_port}"))?;
    sock.set_nodelay(true).ok();
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut writer = sock;
    eprintln!("[ppsspp-rust] connected");

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => bridge.handle_request(request),
            Err(err) => Response {
                id: 0,
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    kind: "protocol_error".into(),
                    message: err.to_string(),
                }),
            },
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
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
