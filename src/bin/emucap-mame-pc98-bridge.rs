use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::live::protocol::{ProtocolError, Request, Response};
use emucap::pc98_bridge::{Bridge, BridgeEnv, GdbRspClient};

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
    let mut bridge = Bridge::new(gdb, BridgeEnv::from_process_env());

    let sock = TcpStream::connect(("127.0.0.1", emucap_port))
        .with_context(|| format!("connect emucap MCP listener at 127.0.0.1:{emucap_port}"))?;
    sock.set_nodelay(true).ok();
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut writer = sock;
    eprintln!("[mame-pc98-rust] connected");

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

fn parse_endpoint(raw: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("expected GDB endpoint as HOST:PORT"))?;
    if host.is_empty() {
        return Err(anyhow!("GDB host is empty"));
    }
    Ok((host.into(), parse_port(port)?))
}
