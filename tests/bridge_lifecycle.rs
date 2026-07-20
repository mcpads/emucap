#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl ChildGuard {
    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.0.try_wait().unwrap().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn_emulator() -> ChildGuard {
    ChildGuard(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn spawn_bridge(emucap_port: u16, gdb_port: u16, emulator_pid: u32) -> ChildGuard {
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_emucap-mame-pc98-bridge"))
            .arg(emucap_port.to_string())
            .arg(format!("127.0.0.1:{gdb_port}"))
            .env("EMUCAP_EMULATOR_PID", emulator_pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn accept_within(listener: &TcpListener, timeout: Duration) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for bridge connection"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("accept bridge connection: {error}"),
        }
    }
}

fn complete_initial_gdb_query(mut gdb: TcpStream) -> TcpStream {
    gdb.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut query = [0u8; 5];
    gdb.read_exact(&mut query).unwrap();
    assert_eq!(&query, b"$?#3f");
    gdb.write_all(b"+$S05#b8").unwrap();
    let mut ack = [0u8; 1];
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');
    gdb
}

fn connected_bridge() -> (ChildGuard, ChildGuard, TcpStream, TcpStream) {
    let front_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let gdb_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut emulator = spawn_emulator();
    let bridge = spawn_bridge(
        front_listener.local_addr().unwrap().port(),
        gdb_listener.local_addr().unwrap().port(),
        emulator.0.id(),
    );
    let gdb = complete_initial_gdb_query(accept_within(&gdb_listener, Duration::from_secs(3)));
    let front = accept_within(&front_listener, Duration::from_secs(3));
    assert!(!emulator.wait_for_exit(Duration::from_millis(1)));
    (emulator, bridge, gdb, front)
}

#[test]
fn idle_bridge_exits_when_its_emulator_generation_ends() {
    let (mut emulator, mut bridge, _gdb, front) = connected_bridge();
    front
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    emulator.0.kill().unwrap();
    emulator.0.wait().unwrap();

    let mut reader = BufReader::new(front);
    let mut line = String::new();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "the bridge must release its front connection after the emulator exits"
    );
    assert!(
        bridge.wait_for_exit(Duration::from_secs(3)),
        "the bridge process must end with its emulator generation"
    );
}

#[test]
fn gdb_eof_makes_the_bridge_terminal_instead_of_reconnecting_the_front() {
    let (_emulator, mut bridge, gdb, mut front) = connected_bridge();
    drop(gdb);
    front
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    front
        .write_all(b"{\"v\":1,\"id\":17,\"method\":\"get_state\",\"params\":{}}\n")
        .unwrap();

    let mut reader = BufReader::new(front);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 17);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["kind"], "bridge_error");
    let mut eof = String::new();
    assert_eq!(
        reader.read_line(&mut eof).unwrap(),
        0,
        "a poisoned GDB client must not survive into another front session"
    );
    assert!(
        bridge.wait_for_exit(Duration::from_secs(3)),
        "the bridge must exit after a terminal backend error"
    );
}
