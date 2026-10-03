#[cfg(any(unix, windows))]
fn main() -> anyhow::Result<()> {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    use anyhow::{anyhow, Context};
    use emucap::live::reconnect::serve_reconnecting_controlled;
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
    let (command_tx, command_rx) = mpsc::channel::<emucap::np2kai_adapter::dispatch::Command>();
    let terminal = Arc::new(AtomicBool::new(false));
    let server_terminal = Arc::clone(&terminal);
    let server = std::thread::spawn(move || {
        let mut control = emucap::np2kai_adapter::dispatch::Control::new(command_tx);
        let probe = move || {
            server_terminal
                .load(Ordering::Acquire)
                .then(|| "NP2kai owner terminated".to_string())
        };
        if let Some(runtime) = std::env::var("EMUCAP_LAUNCH_ID")
            .ok()
            .filter(|v| !v.is_empty())
        {
            emucap::live::reconnect::owned::serve_reconnecting_owned(
                port,
                "np2kai-libretro",
                control,
                probe,
                emucap::live::reconnect::cancellation::TemporalAdmission {
                    runtime,
                    methods: vec!["step".into()],
                },
            )
        } else {
            serve_reconnecting_controlled(
                port,
                "np2kai-libretro",
                move |request| control.legacy(request),
                probe,
            )
        }
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
        if let Some(command) = command {
            host.process_command(command);
            if host.backend_terminal() {
                break;
            }
        }
    }
    terminal.store(true, Ordering::Release);
    server
        .join()
        .map_err(|_| anyhow!("NP2kai reconnect server panicked"))??;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the NP2kai frontend requires a Unix or Windows host")
}
