//! Emulator connection broker.
//!
//! Binds both ports atomically and exits if either cannot be acquired. SO_REUSEPORT is not used,
//! preserving bind-as-lock election.
use std::net::TcpListener;

use emucap::live::broker;

fn main() {
    let emu_port: u16 = std::env::var("EMUCAP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(47800);
    let sess_port: u16 = std::env::var("EMUCAP_BROKER_SESSION_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(emu_port + 100);

    // Bind the emulator port first.
    let emu = match TcpListener::bind(("127.0.0.1", emu_port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("broker: could not bind emulator port {emu_port} ({e}); another broker may already be running");
            std::process::exit(0);
        }
    };

    // Bind the session port. A failure drops the emulator listener before exit.
    let sess = match TcpListener::bind(("127.0.0.1", sess_port)) {
        Ok(l) => l,
        Err(e) => {
            drop(emu);
            eprintln!("broker: could not bind session port {sess_port} ({e})");
            std::process::exit(0);
        }
    };

    if std::env::var_os("EMUCAP_BROKER_STALE_MS").is_some() {
        eprintln!(
            "broker: EMUCAP_BROKER_STALE_MS is ignored; elapsed time never transfers control"
        );
    }
    eprintln!("broker: listening emu={emu_port} session={sess_port}");
    broker::serve(emu, sess);
}
