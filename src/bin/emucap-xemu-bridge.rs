use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::gdb_rsp::{GdbBridgeEnv, GdbRspClient};
use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply, ProcessDependency};
use emucap::qmp::QmpClient;
use emucap::xemu_bridge::{XemuBridge, XemuMachineIdentity, XemuStateEnvironment};

fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        eprintln!("usage: emucap-xemu-bridge <EMUCAP_PORT> <QMP_HOST:PORT> <GDB_HOST:PORT>");
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let (qmp_host, qmp_port) = parse_endpoint(&args[2]).context("invalid QMP endpoint")?;
    let (gdb_host, gdb_port) = parse_endpoint(&args[3]).context("invalid GDB endpoint")?;
    let screen_root = std::env::var_os("EMUCAP_XEMU_SCREEN_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("EMUCAP_XEMU_SCREEN_ROOT is required"))?;

    eprintln!(
        "[xemu-rust] connecting qmp={qmp_host}:{qmp_port} gdb={gdb_host}:{gdb_port} emucap=127.0.0.1:{emucap_port}"
    );
    let qmp = QmpClient::connect(
        &qmp_host,
        qmp_port,
        Duration::from_secs(5),
        Duration::from_secs(30),
    )
    .with_context(|| format!("connect xemu QMP at {qmp_host}:{qmp_port}"))?;
    let gdb = GdbRspClient::connect(
        &gdb_host,
        gdb_port,
        Duration::from_secs(5),
        Duration::from_secs(30),
    )
    .with_context(|| format!("connect xemu GDB at {gdb_host}:{gdb_port}"))?;
    let controlled_start = std::env::var("EMUCAP_START_FROZEN").ok().as_deref() == Some("1");
    let machine_identity =
        XemuMachineIdentity::from_process_env().map_err(|error| anyhow!(error))?;
    let state_environment =
        XemuStateEnvironment::from_process_env().map_err(|error| anyhow!(error))?;
    let mut bridge = XemuBridge::new(
        qmp,
        gdb,
        GdbBridgeEnv::from_process_env(),
        screen_root,
        controlled_start,
        machine_identity,
        state_environment,
    );
    let dependency =
        ProcessDependency::from_process_env().context("load emulator process dependency")?;

    serve_reconnecting_controlled(
        emucap_port,
        "xemu-rust",
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
    .context("serve reconnecting xemu session")
}

fn parse_port(raw: &str) -> anyhow::Result<u16> {
    let port = raw
        .parse::<u16>()
        .map_err(|_| anyhow!("expected decimal TCP port, got {raw:?}"))?;
    if port == 0 {
        Err(anyhow!("port must be in 1..=65535"))
    } else {
        Ok(port)
    }
}

fn parse_endpoint(raw: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("expected endpoint as HOST:PORT"))?;
    if host.is_empty() {
        return Err(anyhow!("endpoint host is empty"));
    }
    Ok((host.into(), parse_port(port)?))
}
