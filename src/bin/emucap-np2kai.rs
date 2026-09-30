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

    loop {
        let command = if host.is_running() {
            match host.next_frame_start(Instant::now()) {
                // Unlimited pacing still services one queued command between frames.
                None => match command_rx.try_recv() {
                    Ok(command) => Some(command),
                    Err(mpsc::TryRecvError::Empty) => {
                        host.run_scheduled_frame()?;
                        None
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                },
                Some(start) => {
                    let now = Instant::now();
                    if now >= start {
                        host.run_scheduled_frame()?;
                        None
                    } else {
                        match command_rx.recv_timeout(start - now) {
                            Ok(command) => Some(command),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                }
            }
        } else {
            match command_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };
        if let Some((request, reply_tx)) = command {
            let _ = reply_tx.send(host.handle_request(request));
        }
    }
    terminal.store(true, Ordering::Release);
    server
        .join()
        .map_err(|_| anyhow!("NP2kai reconnect server panicked"))??;
    Ok(())
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the NP2kai frontend currently supports Unix hosts only")
}
