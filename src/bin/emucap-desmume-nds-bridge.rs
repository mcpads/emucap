use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::live::protocol::{ProtocolError, Request, Response};
use emucap::nds_bridge::NdsBridge;
use emucap::pc98_bridge::{BridgeEnv, GdbRspClient};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: emucap-desmume-nds-bridge <EMUCAP_PORT> <ARM9_HOST:PORT> [ARM7_HOST:PORT]"
        );
        eprintln!(
            "  ARM9 GDB endpoint (DeSmuME --arm9gdb) is required; ARM7 (--arm7gdb) is optional."
        );
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let (arm9_host, arm9_port) =
        parse_endpoint(&args[2]).context("invalid ARM9 GDB endpoint")?;
    let arm7_endpoint = match args.get(3) {
        Some(raw) => Some(parse_endpoint(raw).context("invalid ARM7 GDB endpoint")?),
        None => None,
    };

    eprintln!(
        "[desmume-nds-rust] connecting arm9={arm9_host}:{arm9_port} emucap=127.0.0.1:{emucap_port}"
    );
    let arm9 = GdbRspClient::connect(
        &arm9_host,
        arm9_port,
        Duration::from_secs(5),
        Duration::from_secs(30),
    )
    .with_context(|| format!("connect ARM9 GDB stub at {arm9_host}:{arm9_port}"))?;

    let arm7 = match &arm7_endpoint {
        Some((host, port)) => {
            eprintln!("[desmume-nds-rust] connecting arm7={host}:{port}");
            Some(
                GdbRspClient::connect(
                    host,
                    *port,
                    Duration::from_secs(5),
                    Duration::from_secs(30),
                )
                .with_context(|| format!("connect ARM7 GDB stub at {host}:{port}"))?,
            )
        }
        None => {
            eprintln!("[desmume-nds-rust] no ARM7 endpoint; arm7 memory/cpu routing disabled");
            None
        }
    };

    let mut bridge = NdsBridge::new(arm9, arm7, BridgeEnv::from_process_env());

    let sock = TcpStream::connect(("127.0.0.1", emucap_port))
        .with_context(|| format!("connect emucap MCP listener at 127.0.0.1:{emucap_port}"))?;
    sock.set_nodelay(true).ok();
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut writer = sock;
    eprintln!("[desmume-nds-rust] connected");

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
