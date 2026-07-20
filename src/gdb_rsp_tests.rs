use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

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
