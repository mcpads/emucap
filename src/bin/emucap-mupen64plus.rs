#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::{anyhow, Context};
    use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply};
    use emucap::n64_adapter::Mupen64PlusHost;

    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        return Err(anyhow!(
            "usage: emucap-mupen64plus <EMUCAP_PORT> <ROM_PATH> <M64P_ROOT> <RUNTIME_HOME>"
        ));
    }
    let port = args[1]
        .parse::<u16>()
        .context("EMUCAP_PORT must be a non-zero decimal port")?;
    if port == 0 {
        return Err(anyhow!("EMUCAP_PORT must be non-zero"));
    }

    let host = Mupen64PlusHost::prepare(
        Path::new(&args[3]),
        Path::new(&args[4]),
        Path::new(&args[2]),
    )?;
    let mut host = host;
    let execution = host.begin_execution();
    let host = Arc::new(Mutex::new(host));
    let server_host = Arc::clone(&host);
    let server = std::thread::spawn(move || {
        serve_reconnecting_controlled(
            port,
            "mupen64plus-native",
            move |request| {
                let response = server_host
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .handle_request(request);
                if Mupen64PlusHost::terminal_reason().is_some() {
                    BridgeReply::terminate_with(response)
                } else {
                    BridgeReply::continue_with(response)
                }
            },
            Mupen64PlusHost::terminal_reason,
        )
    });

    let release_host = Arc::clone(&host);
    let release = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let result = release_host
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .release_initial_pause();
            match result {
                Ok(()) => return Ok(()),
                Err(error) if std::time::Instant::now() < deadline => {
                    eprintln!("[mupen64plus-native] waiting to release initial pause: {error}");
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        }
    });

    let execute_result = execution.execute_blocking();
    let release_result = release
        .join()
        .map_err(|_| anyhow!("initial debugger release thread panicked"))?;
    let server_result = server
        .join()
        .map_err(|_| anyhow!("N64 control server thread panicked"))?;
    release_result?;
    server_result?;
    Ok(execute_result?)
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the initial Mupen64Plus adapter currently supports Unix hosts only")
}
