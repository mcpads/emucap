use std::io::Write;
use std::net::{Shutdown, TcpStream};

use super::recording_member_sink::MemberSinkServer;
use crate::bundle::recording_manifest::InitialSnapshotRequest;

fn request(label: &str, length: u64) -> InitialSnapshotRequest {
    InitialSnapshotRequest {
        label: label.into(),
        memory_type: "snesWorkRam".into(),
        address: 0,
        length,
    }
}

#[test]
fn authenticated_member_sink_accepts_exact_ordered_binary_members() {
    let server = MemberSinkServer::spawn("capture-test", &[request("wram", 4)], 1000).unwrap();
    let mut stream = TcpStream::connect(&server.endpoint).unwrap();
    writeln!(
        stream,
        "{{\"token\":\"{}\",\"capture_id\":\"capture-test\"}}",
        server.token
    )
    .unwrap();
    writeln!(stream, "{{\"label\":\"wram\",\"bytes\":4}}").unwrap();
    stream.write_all(b"A\n\0Z").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();

    let outcome = server.finish(1000);
    assert_eq!(outcome.error, None);
    assert!(outcome.partial.is_none());
    assert_eq!(outcome.members.len(), 1);
    assert_eq!(outcome.members[0].request.label, "wram");
    assert_eq!(outcome.members[0].bytes, b"A\n\0Z");
}

#[test]
fn member_sink_rejects_reordered_or_short_members() {
    let server = MemberSinkServer::spawn(
        "capture-test",
        &[request("first", 2), request("second", 2)],
        1000,
    )
    .unwrap();
    let mut stream = TcpStream::connect(&server.endpoint).unwrap();
    writeln!(
        stream,
        "{{\"token\":\"{}\",\"capture_id\":\"capture-test\"}}",
        server.token
    )
    .unwrap();
    writeln!(stream, "{{\"label\":\"second\",\"bytes\":2}}").unwrap();
    stream.write_all(b"XX").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();

    let outcome = server.finish(1000);
    assert!(outcome.members.is_empty());
    assert!(outcome.partial.is_none());
    assert!(outcome.error.unwrap().contains("order, label, or length"));
}

#[test]
fn member_sink_preserves_a_bounded_short_prefix_for_quarantine() {
    let server = MemberSinkServer::spawn("capture-test", &[request("wram", 4)], 1000).unwrap();
    let mut stream = TcpStream::connect(&server.endpoint).unwrap();
    writeln!(
        stream,
        "{{\"token\":\"{}\",\"capture_id\":\"capture-test\"}}",
        server.token
    )
    .unwrap();
    writeln!(stream, "{{\"label\":\"wram\",\"bytes\":4}}").unwrap();
    stream.write_all(b"AB").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();

    let outcome = server.finish(1000);
    assert!(outcome.error.unwrap().contains("short member"));
    assert_eq!(outcome.partial.unwrap().bytes, b"AB");
}

#[test]
fn member_sink_rejects_trailing_bytes_after_the_declared_member_set() {
    let server = MemberSinkServer::spawn("capture-test", &[request("wram", 2)], 1000).unwrap();
    let mut stream = TcpStream::connect(&server.endpoint).unwrap();
    writeln!(
        stream,
        "{{\"token\":\"{}\",\"capture_id\":\"capture-test\"}}",
        server.token
    )
    .unwrap();
    writeln!(stream, "{{\"label\":\"wram\",\"bytes\":2}}").unwrap();
    stream.write_all(b"ABC").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();

    let outcome = server.finish(1000);
    assert!(outcome.error.unwrap().contains("trailing bytes"));
}
