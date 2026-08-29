#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    use anyhow::{anyhow, Context};
    use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply};
    use emucap::np2kai_adapter::Np2kaiHost;

    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 9 {
        return Err(anyhow!("usage: emucap-np2kai <PORT> <CONTENT> <CORE> <FIRMWARE_DIR> <RUNTIME_HOME> <UPSTREAM_COMMIT> <PATCHSET_SHA256> <BUILD_PROFILE>"));
    }
    let port = args[1]
        .parse::<u16>()
        .context("PORT must be a non-zero decimal port")?;
    if port == 0 {
        return Err(anyhow!("PORT must be non-zero"));
    }
    let mut host = Np2kaiHost::open(
        Path::new(&args[2]),
        Path::new(&args[3]),
        Path::new(&args[4]),
        Path::new(&args[5]),
        &args[6],
        &args[7],
        &args[8],
    )?;
    let (command_tx, command_rx) = mpsc::channel::<(
        emucap::live::protocol::Request,
        mpsc::SyncSender<emucap::live::protocol::Response>,
    )>();
    let terminal = Arc::new(AtomicBool::new(false));
    let server_terminal = Arc::clone(&terminal);
    let server = std::thread::spawn(move || {
        let probe_terminal = Arc::clone(&server_terminal);
        serve_reconnecting_controlled(
            port,
            "np2kai-libretro",
            move |request| {
                let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                if command_tx.send((request, reply_tx)).is_err() {
                    server_terminal.store(true, Ordering::Release);
                    return BridgeReply::terminate_with(emucap::live::protocol::Response {
                        id: 0,
                        ok: false,
                        result: None,
                        error: Some(emucap::live::protocol::ProtocolError {
                            kind: "adapter_error".into(),
                            message: "NP2kai worker ended".into(),
                        }),
                    });
                }
                match reply_rx.recv() {
                    Ok(response) => BridgeReply::continue_with(response),
                    Err(_) => {
                        server_terminal.store(true, Ordering::Release);
                        BridgeReply::terminate_with(emucap::live::protocol::Response {
                            id: 0,
                            ok: false,
                            result: None,
                            error: Some(emucap::live::protocol::ProtocolError {
                                kind: "adapter_error".into(),
                                message: "NP2kai worker did not return a response".into(),
                            }),
                        })
                    }
                }
            },
            move || {
                probe_terminal
                    .load(Ordering::Acquire)
                    .then(|| "NP2kai worker terminated".to_string())
            },
        )
    });

    let mut next_frame = Instant::now() + host.frame_duration();
    loop {
        let command = if host.is_running() {
            let now = Instant::now();
            if now >= next_frame {
                host.run_scheduled_frame()?;
                next_frame = Instant::now() + host.frame_duration();
                continue;
            }
            match command_rx.recv_timeout(next_frame - now) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    host.run_scheduled_frame()?;
                    next_frame = Instant::now() + host.frame_duration();
                    None
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match command_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };
        if let Some((request, reply_tx)) = command {
            let was_running = host.is_running();
            let response = host.handle_request(request);
            let running = host.is_running();
            let _ = reply_tx.send(response);
            next_frame = deadline_after_command(
                next_frame,
                was_running,
                running,
                Instant::now(),
                host.frame_duration(),
            );
        }
    }
    terminal.store(true, Ordering::Release);
    server
        .join()
        .map_err(|_| anyhow!("NP2kai reconnect server panicked"))??;
    Ok(())
}

#[cfg(unix)]
fn deadline_after_command(
    previous: std::time::Instant,
    was_running: bool,
    is_running: bool,
    now: std::time::Instant,
    frame_duration: std::time::Duration,
) -> std::time::Instant {
    if !was_running && is_running {
        now + frame_duration
    } else {
        previous
    }
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the NP2kai frontend currently supports Unix hosts only")
}

#[cfg(all(test, unix))]
#[path = "../emucap_np2kai_frontend_tests.rs"]
mod tests;
