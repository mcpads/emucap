use super::broker_link;
use super::link::{EmulatorLink, LinkError};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn broker_link_attaches_and_calls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap(); // attach
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id},"ok":true,"result":{{"attached_name":"g","methods":["status"]}}}}"#
        )
        .unwrap();
        let mut c = String::new();
        r.read_line(&mut c).unwrap(); // status
        let id2 = serde_json::from_str::<serde_json::Value>(c.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"connected":true}}}}"#
        )
        .unwrap();
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_secs(2)).unwrap();
    assert_eq!(link.capabilities().methods, vec!["status".to_string()]);
    let out = link.call("status", serde_json::json!({})).unwrap();
    assert_eq!(out["connected"], true);
    h.join().unwrap();
}

#[test]
fn broker_link_connect_returns_busy_on_busy_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap(); // attach
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(w, r#"{{"id":{id},"ok":false,"error":{{"kind":"busy"}}}}"#).unwrap();
    });
    let err = broker_link::connect(&addr, None, Duration::from_secs(2)).unwrap_err();
    assert!(matches!(err, LinkError::Busy), "Busy 매핑 실패: {err:?}");
    h.join().unwrap();
}

#[test]
fn broker_link_skips_keepalive_and_returns_final() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        // attach 응답
        let mut a = String::new();
        r.read_line(&mut a).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id},"ok":true,"result":{{"attached_name":"g","methods":["run_frames"]}}}}"#
        )
        .unwrap();
        // run_frames 명령
        let mut c = String::new();
        r.read_line(&mut c).unwrap();
        let id2 = serde_json::from_str::<serde_json::Value>(c.trim()).unwrap()["id"].clone();
        // keepalive 먼저
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"status":"working","frame":10}}}}"#
        )
        .unwrap();
        // 최종 응답
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"status":"completed","frame":60}}}}"#
        )
        .unwrap();
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_secs(2)).unwrap();
    let out = link
        .call("run_frames", serde_json::json!({"n": 60}))
        .unwrap();
    // "completed"는 기본값이므로 result_status가 STATUS_WORKING 아님 → 최종 반환
    assert_eq!(
        out["frame"], 60,
        "keepalive 건너뛰고 최종 응답 받아야: {out}"
    );
    h.join().unwrap();
}

#[test]
fn broker_link_maps_no_such_emulator_with_names() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id},"ok":false,"error":{{"kind":"no_such_emulator","names":["alpha","beta"]}}}}"#
        )
        .unwrap();
    });
    let err =
        broker_link::connect(&addr, Some("gamma".into()), Duration::from_secs(2)).unwrap_err();
    match err {
        LinkError::NoSuchEmulator { names } => {
            assert!(
                names.contains(&"alpha".to_string()),
                "names에 alpha 있어야: {names:?}"
            );
            assert!(
                names.contains(&"beta".to_string()),
                "names에 beta 있어야: {names:?}"
            );
        }
        other => panic!("NoSuchEmulator 기대했는데: {other:?}"),
    }
    h.join().unwrap();
}

#[test]
fn broker_link_maps_ambiguous_with_names() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        // 실제 broker 포맷: message 없음
        writeln!(
            w,
            r#"{{"id":{id},"ok":false,"error":{{"kind":"ambiguous","names":["a","b"]}}}}"#
        )
        .unwrap();
    });
    let err = broker_link::connect(&addr, None, Duration::from_secs(2)).unwrap_err();
    match err {
        LinkError::Ambiguous { names } => {
            assert!(
                names.contains(&"a".to_string()),
                "names에 a 있어야: {names:?}"
            );
            assert!(
                names.contains(&"b".to_string()),
                "names에 b 있어야: {names:?}"
            );
        }
        other => panic!("Ambiguous 기대했는데: {other:?}"),
    }
    h.join().unwrap();
}
