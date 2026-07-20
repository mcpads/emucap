use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::gdb_rsp::{GdbBridgeEnv, GdbRspClient};
use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply, ProcessDependency};
use emucap::pc98_bridge::Bridge;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: emucap-mame-pc98-bridge <EMUCAP_PORT> [GDB_HOST:PORT]");
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let (gdb_host, gdb_port) = if let Some(raw) = args.get(2) {
        parse_endpoint(raw)?
    } else {
        ("127.0.0.1".into(), 3264)
    };

    eprintln!(
        "[mame-pc98-rust] connecting gdb={gdb_host}:{gdb_port} emucap=127.0.0.1:{emucap_port}"
    );
    let gdb = GdbRspClient::connect(
        &gdb_host,
        gdb_port,
        Duration::from_secs(5),
        Duration::from_secs(30),
    )
    .with_context(|| format!("connect GDB stub at {gdb_host}:{gdb_port}"))?;
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::from_process_env());
    let dependency =
        ProcessDependency::from_process_env().context("load emulator process dependency")?;

    serve_reconnecting_controlled(
        emucap_port,
        "mame-pc98-rust",
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

fn parse_endpoint(raw: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("expected GDB endpoint as HOST:PORT"))?;
    if host.is_empty() {
        return Err(anyhow!("GDB host is empty"));
    }
    Ok((host.into(), parse_port(port)?))
}
