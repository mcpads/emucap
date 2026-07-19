use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::live::reconnect::serve_reconnecting;
use emucap::pcsx2_bridge::{Pcsx2Bridge, PineSocket};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: emucap-pcsx2-bridge <EMUCAP_PORT> <PINE_SLOT> [PINE_SOCKET_PATH]");
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let pine_slot = parse_port(&args[2]).context("invalid PINE_SLOT")?;
    let socket_path = args.get(3).map(PathBuf::from);

    eprintln!("[pcsx2-rust] connecting pine_slot={pine_slot} emucap=127.0.0.1:{emucap_port}");
    let pine = PineSocket::connect(pine_slot, socket_path.as_deref(), Duration::from_secs(12))
        .context("connect compatible PCSX2 PINE fork")?;
    let mut bridge = Pcsx2Bridge::new(pine).context("verify PCSX2 host API")?;

    serve_reconnecting(emucap_port, "pcsx2-rust", move |request| {
        bridge.handle_request(request)
    })
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
