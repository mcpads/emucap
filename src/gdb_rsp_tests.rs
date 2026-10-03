use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn process_env_cannot_override_the_embedded_adapter_build() {
    let _lock = crate::test_env::lock_env();
    let _restore = crate::test_env::EnvGuard::new(&["EMUCAP_BUILD_HASH"]);
    std::env::set_var("EMUCAP_BUILD_HASH", "forged-launch-build");

    let env = GdbBridgeEnv::from_process_env();

    assert_eq!(
        env.build.as_deref(),
        Some(crate::build_identity::BUILD_HASH)
    );
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        if request.len() >= 4 && request[request.len() - 3] == b'#' {
            return request;
        }
    }
}

#[test]
fn client_sends_acknowledged_packet_and_decodes_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert_eq!(std::str::from_utf8(&request).unwrap(), "$g#67");
        stream.write_all(b"+").unwrap();
        let payload = b"OK";
        let frame = format!(
            "$OK#{:02x}",
            payload
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
        );
        stream.write_all(frame.as_bytes()).unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.send("g").unwrap(), "OK");
    handle.join().unwrap();
}

#[test]
fn interrupt_reads_and_acknowledges_async_stop_before_any_query() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut interrupt = [0u8; 1];
        stream.read_exact(&mut interrupt).unwrap();
        assert_eq!(interrupt[0], 0x03);

        stream.write_all(b"$S02#b5").unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');

        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut unexpected = [0u8; 1];
        let err = stream.read_exact(&mut unexpected).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "interrupt must not send a trailing `?` request: {err}"
        );
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.interrupt().unwrap(), "S02");
    handle.join().unwrap();
}

#[test]
fn stream_error_poisons_only_the_failed_client_and_replacement_is_clean() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        assert_eq!(read_request(&mut first), b"$g#67");
        first.write_all(b"+$OK#00").unwrap();

        let (mut replacement, _) = listener.accept().unwrap();
        assert_eq!(read_request(&mut replacement), b"$g#67");
        replacement.write_all(b"+$OK#9a").unwrap();
        let mut ack = [0u8; 1];
        replacement.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut failed = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(!failed.is_terminal());
    assert!(matches!(failed.send("g"), Err(GdbError::Io(_))));
    assert!(failed.is_terminal());
    assert!(matches!(failed.send("g"), Err(GdbError::Poisoned)));
    drop(failed);

    let mut replacement = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(!replacement.is_terminal());
    assert_eq!(replacement.send("g").unwrap(), "OK");
    handle.join().unwrap();
}

#[test]
fn nonblocking_poll_retains_each_fragment_without_poisoning_the_channel() {
    for split in [1, 3, 4, 5, 6] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let packet = b"$S05#b8";
            stream.write_all(&packet[..split]).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stream.write_all(&packet[split..]).unwrap();
            let mut ack = [0];
            stream.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [b'+']);
        });
        let mut client = GdbRspClient::connect(
            "127.0.0.1",
            port,
            Duration::from_secs(1),
            Duration::from_millis(50),
        )
        .unwrap();
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            assert_eq!(
                client
                    .recv_nonblocking_with_timeout(Duration::from_millis(100))
                    .unwrap(),
                None
            );
            if !client.buf.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!client.is_terminal());
        release_tx.send(()).unwrap();
        loop {
            if let Some(packet) = client
                .recv_nonblocking_with_timeout(Duration::from_millis(100))
                .unwrap()
            {
                assert_eq!(packet, "S05");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!client.is_terminal());
        assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(1));
        server.join().unwrap();
    }
}

#[test]
fn nonblocking_poll_decodes_buffered_packet_before_reading_later_socket_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut ack = [0];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [b'+']);
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(1),
        Duration::from_millis(50),
    )
    .unwrap();
    client.buf.extend(b"+$S05#b8");
    assert_eq!(client.recv_nonblocking().unwrap(), Some("S05".into()));
    assert!(client.buf.is_empty());
    assert!(!client.is_terminal());
    server.join().unwrap();
}

#[test]
fn bounded_exchange_retires_trickling_reply_and_restores_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"+$").unwrap();
        // Every byte arrives within the socket timeout, but the packet never completes.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(30));
            if stream.write_all(b"a").is_err() {
                break;
            }
        }
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )
    .unwrap();
    let start = Instant::now();
    assert!(client
        .send_with_timeout("g", Duration::from_millis(120))
        .is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(client.is_terminal());
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    assert_eq!(
        client.stream.write_timeout().unwrap(),
        Some(Duration::from_secs(2))
    );
    assert!(matches!(client.recv_reply(), Err(GdbError::Poisoned)));
    drop(client);
    handle.join().unwrap();
}

#[test]
fn bounded_exchange_success_allows_next_reply_and_restores_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"+$OK#9a$S05#b8").unwrap();
        let mut acks = [0; 2];
        stream.read_exact(&mut acks).unwrap();
        assert_eq!(&acks, b"++");
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        client
            .send_with_timeout("g", Duration::from_secs(1))
            .unwrap(),
        "OK"
    );
    assert_eq!(
        client
            .recv_reply_with_timeout(Duration::from_secs(1))
            .unwrap(),
        "S05"
    );
    assert!(!client.is_terminal());
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    handle.join().unwrap();
}

#[test]
fn bounded_dispatch_and_interrupt_do_not_consume_deferred_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(
            read_request(&mut stream),
            GdbRspClient::frame("QEmucap,framestep:2")
        );
        stream.write_all(b"+").unwrap();
        let mut raw = [0];
        stream.read_exact(&mut raw).unwrap();
        assert_eq!(raw, [3]);
        stream.write_all(b"$S02#b5").unwrap();
        stream.read_exact(&mut raw).unwrap();
        assert_eq!(raw, [b'+']);
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    client
        .send_no_reply_with_timeout("QEmucap,framestep:2", Duration::from_millis(200))
        .unwrap();
    client
        .request_interrupt_with_timeout(Duration::from_millis(200))
        .unwrap();
    assert_eq!(
        client
            .recv_reply_with_timeout(Duration::from_millis(200))
            .unwrap(),
        "S02"
    );
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    handle.join().unwrap();
}

fn connected_pair() -> (GdbRspClient, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = GdbRspClient::connect(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    let (peer, _) = listener.accept().unwrap();
    (client, peer)
}

#[test]
fn rsp_exact_payload_limit_preserves_following_packet() {
    let (mut client, mut peer) = connected_pair();
    let payload = "x".repeat(MAX_RSP_PAYLOAD_BYTES);
    client.buf.extend(GdbRspClient::frame(&payload));
    client.buf.extend(GdbRspClient::frame("OK"));
    assert_eq!(client.recv_reply().unwrap(), payload);
    assert_eq!(client.recv_nonblocking().unwrap(), Some("OK".into()));
    let mut ack = [0; 2];
    peer.read_exact(&mut ack).unwrap();
    assert_eq!(&ack, b"++");
    assert!(!client.is_terminal());
}

#[test]
fn oversized_rsp_retires_blocking_and_polling_connections_without_ack() {
    for (poll, complete) in [(false, true), (false, false), (true, true), (true, false)] {
        let (mut client, mut peer) = connected_pair();
        let payload = "x".repeat(MAX_RSP_PAYLOAD_BYTES + 5);
        let packet = if complete {
            GdbRspClient::frame(&payload)
        } else {
            format!("${payload}").into_bytes()
        };
        client.buf.extend(packet);
        let error = if poll {
            client.recv_nonblocking().map(|_| ()).unwrap_err()
        } else {
            client.recv_reply().map(|_| ()).unwrap_err()
        };
        assert!(matches!(error,GdbError::Io(ref e) if e.kind()==std::io::ErrorKind::InvalidData));
        assert!(client.is_terminal());
        assert!(matches!(client.send("?"), Err(GdbError::Poisoned)));
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn oversized_rsp_request_is_rejected_before_socket_write() {
    let (mut client, mut peer) = connected_pair();
    assert!(client
        .send_no_reply(&"x".repeat(MAX_RSP_PAYLOAD_BYTES + 1))
        .is_err());
    assert!(client.is_terminal());
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn bounded_failure_preserves_primary_error_when_timeout_cleanup_also_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let result: GdbResult<()> = client.bounded(Duration::from_millis(100), |client| {
        // Some kernels reject timeout restoration on a shut-down socket. Preserve
        // the primary category and message on either kernel behavior.
        client.stream.shutdown(std::net::Shutdown::Both).unwrap();
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "primary protocol failure").into())
    });
    let Err(GdbError::Io(error)) = result else {
        panic!("wrong primary error: {result:?}")
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("primary protocol failure"));
    assert!(client.poisoned);
}
