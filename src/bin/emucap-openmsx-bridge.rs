use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{anyhow, Context};
use emucap::launch::openmsx::PreparedSession;
use emucap::live::reconnect::cancellation::TemporalAdmission;
use emucap::live::reconnect::owned::serve_reconnecting_owned;
use emucap::live::reconnect::{serve_reconnecting_cancellable, BridgeReply};
use emucap::openmsx_bridge::{OpenMsxBridge, XmlControl};

fn main() -> anyhow::Result<()> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if !(7..=8).contains(&args.len()) {
        eprintln!(
            "usage: emucap-openmsx-bridge <EMUCAP_PORT> <OPENMSX_BIN> <SESSION_MANIFEST> \
             <RUNTIME_HOME> <DISPLAY:0|1> <PID_FILE> [SOUND:0|1]"
        );
        std::process::exit(2);
    }
    let port = args[1]
        .to_string_lossy()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| anyhow!("EMUCAP_PORT must be a decimal port in 1..=65535"))?;
    let binary = PathBuf::from(&args[2]);
    let session_manifest = PathBuf::from(&args[3]);
    let runtime_home = PathBuf::from(&args[4]);
    let display = match args[5].to_string_lossy().as_ref() {
        "0" => false,
        "1" => true,
        other => return Err(anyhow!("DISPLAY must be 0 or 1, got {other:?}")),
    };
    let pid_file = PathBuf::from(&args[6]);
    let sound = match args.get(7).map(|value| value.to_string_lossy()).as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        other => return Err(anyhow!("SOUND must be 0 or 1, got {other:?}")),
    };
    let session_bytes =
        emucap::path_safety::read_bounded_regular_file_no_follow(&session_manifest, 1024 * 1024)
            .with_context(|| {
                format!(
                    "read bounded session manifest {}",
                    session_manifest.display()
                )
            })?;
    let session: PreparedSession =
        serde_json::from_slice(&session_bytes).context("parse openMSX session manifest")?;
    session
        .verify()
        .context("verify openMSX session identity before process spawn")?;

    let control = XmlControl::spawn(&binary, &session, &runtime_home, display)
        .context("start pinned openMSX XML control channel")?;
    let terminal = control.terminal_handle();
    let mut bridge = OpenMsxBridge::new(control, &session, &runtime_home, display, sound)
        .context("initialize openMSX bridge")?;
    write_pid_file(&pid_file, bridge.child_pid()).context("publish openMSX child pid")?;

    let runtime = std::env::var("EMUCAP_LAUNCH_ID")
        .ok()
        .filter(|id| !id.is_empty());
    let result = if let Some(runtime) = runtime {
        serve_reconnecting_owned(
            port,
            "openmsx-rust",
            bridge,
            move || {
                terminal
                    .load(Ordering::Acquire)
                    .then(|| "openMSX control channel closed".to_string())
            },
            TemporalAdmission {
                runtime,
                methods: vec!["step".into()],
            },
        )
    } else {
        serve_reconnecting_cancellable(
            port,
            "openmsx-rust",
            move |request, cancellation| {
                let response = bridge.handle_request_cancellable(request, cancellation);
                if bridge.backend_terminal() {
                    BridgeReply::terminate_with(response)
                } else {
                    BridgeReply::continue_with(response)
                }
            },
            move || {
                terminal
                    .load(Ordering::Acquire)
                    .then(|| "openMSX control channel closed".to_string())
            },
            TemporalAdmission {
                runtime: std::env::var("EMUCAP_LAUNCH_ID").unwrap_or_default(),
                methods: vec!["step".into()],
            },
        )
    }
    .context("serve reconnecting openMSX session");
    let _ = fs::remove_file(pid_file);
    result
}

fn write_pid_file(path: &Path, pid: u32) -> std::io::Result<()> {
    emucap::path_safety::atomic_write_file(path, pid.to_string().as_bytes())
}
