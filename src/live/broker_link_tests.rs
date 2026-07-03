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

// M1: 응답이 read 타임아웃 경계에 쪼개져 도착해도 pending 버퍼로 이어 읽어 스트림 desync가 없어야
// 한다. 수정 전(호출마다 새 String)엔 앞 절반을 잃어, 뒤 절반이 다음 호출에서 깨진 줄로 읽혀 Protocol
// desync가 난다.
#[test]
fn broker_link_preserves_partial_reply_across_timeout() {
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
        // status#1
        let mut c = String::new();
        r.read_line(&mut c).unwrap();
        let id1 = serde_json::from_str::<serde_json::Value>(c.trim()).unwrap()["id"].clone();
        // 응답 앞 절반만(개행 없음) → 클라 read 타임아웃 유발(timeout=120ms). 완성은 1회 타임아웃
        // 뒤·연속 임계치(3회) 전에 오도록 170ms에 보낸다(M3 self-heal와 간섭 않게).
        write!(w, r#"{{"id":{id1},"ok":true,"resu"#).unwrap();
        w.flush().unwrap();
        std::thread::sleep(Duration::from_millis(170)); // 1회 타임아웃 경계 확보(120ms<170ms<360ms)
        // 나머지 절반 + 개행(=#1 응답 완성). 클라의 다음 호출에선 id 불일치로 버려진다.
        writeln!(w, r#"lt":{{"connected":true}}}}"#).unwrap();
        // 이후 오는 status마다 n=2로 응답(각 호출이 자기 id 응답을 결국 받게 — 데드라인 없는 demux).
        loop {
            let mut cn = String::new();
            match r.read_line(&mut cn) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let idn = match serde_json::from_str::<serde_json::Value>(cn.trim()) {
                Ok(v) => v["id"].clone(),
                Err(_) => break,
            };
            if writeln!(
                w,
                r#"{{"id":{idn},"ok":true,"result":{{"connected":true,"n":2}}}}"#
            )
            .is_err()
            {
                break;
            }
        }
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_millis(120)).unwrap();
    // 반복 호출: 쪼개진 #1은 타임아웃되고 pending에 보존된다. 이어 읽기로 결국 유효 응답을 받아야 하고,
    // 절대 Protocol(desync)로 깨지면 안 된다.
    let mut ok = false;
    for _ in 0..40 {
        match link.call("status", serde_json::json!({})) {
            Ok(v) => {
                assert_eq!(v["n"], 2, "쪼개진 #1을 버리고 후속 응답을 받아야: {v}");
                ok = true;
                break;
            }
            Err(LinkError::Timeout) => continue,
            Err(LinkError::Protocol(e)) => {
                panic!("쪼개진 응답으로 스트림 desync(Protocol) — pending 버퍼 미보존: {e}")
            }
            other => panic!("Ok/Timeout 기대: {other:?}"),
        }
    }
    assert!(ok, "pending 이어읽기로 결국 유효 응답을 받아야(desync 없이)");
    drop(link); // 소켓을 닫아 broker의 응답 루프 read_line이 EOF로 끝나게(join 무한대기 방지).
    h.join().unwrap();
}

// M3: broker가 attach 후 hang(응답 없음)이면 연속 read 타임아웃이 쌓인다. 임계치에서 NotConnected를
// 올려 LazyBrokerLink가 재attach로 자가복구하게 해야 한다 — 수정 전엔 영구 Timeout으로 wedge된다.
#[test]
fn broker_link_self_heals_after_consecutive_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
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
        // 이후 어떤 요청에도 응답하지 않고 소켓을 연 채 hang(EOF 아님 → read 타임아웃 누적).
        let _ = rel_rx.recv();
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_millis(80)).unwrap();
    let mut saw_not_connected = false;
    for _ in 0..10 {
        match link.call("status", serde_json::json!({})) {
            Err(LinkError::Timeout) => {}
            Err(LinkError::NotConnected) => {
                saw_not_connected = true;
                break;
            }
            other => panic!("Timeout 또는 NotConnected 기대: {other:?}"),
        }
    }
    assert!(
        saw_not_connected,
        "연속 read 타임아웃이 결국 NotConnected(self-heal 신호)를 내야 — hung broker 영구 wedge 금지"
    );
    rel_tx.send(()).ok();
    h.join().unwrap();
}

// F3: broker가 attach 후 후속 요청을 안 읽으면(백프레셔) 큰 요청의 write_all이 스톨한다. set_write_timeout이
// 없으면 영구 블록으로 링크 뮤텍스를 쥔 채 wedge된다. 쓰기 실패는 NotConnected로 poison해 LazyBrokerLink가
// 재attach하게 해야 한다(부분 송신된 conn을 재사용하지 않도록).
#[test]
fn broker_link_write_timeout_poisons() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap(); // attach
        let id = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id},"ok":true,"result":{{"attached_name":"g","methods":["read_memory"]}}}}"#
        )
        .unwrap();
        // 이후 요청을 절대 읽지 않는다 — broker recv 버퍼가 차 큰 요청의 write_all이 스톨한다.
        let _ = rel_rx.recv();
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_millis(200)).unwrap();
    // 큰 params로 요청 한 줄을 수십 MB로 만들어 송신+broker recv 버퍼를 넘긴다 → write_all 스톨.
    let big = "x".repeat(32 * 1024 * 1024);
    let r = link.call("read_memory", serde_json::json!({ "blob": big }));
    assert!(
        matches!(r, Err(LinkError::NotConnected)),
        "broker 쓰기 타임아웃(백프레셔)은 NotConnected로 poison해야(LazyBrokerLink 재attach): {:?}",
        r.map(|_| "ok")
    );
    rel_tx.send(()).ok();
    h.join().unwrap();
}

// broker가 attach 후 완료 없이 working keepalive만 무한히 흘리면(deferred flood), 매 working이 성공 read라
// consecutive_timeouts가 안 오른다 — deferred_deadline 초과 시 NotConnected로 끊어 LazyBrokerLink 재attach를
// 유도해야 한다(안 그러면 SharedLink mutex를 쥔 채 영구 wedge). TcpLink의 working-flood 컷오프 대응.
#[test]
fn broker_link_bails_on_working_flood_past_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut a = String::new();
        r.read_line(&mut a).unwrap(); // attach
        let aid = serde_json::from_str::<serde_json::Value>(a.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{aid},"ok":true,"result":{{"attached_name":"g","methods":["run_frames"]}}}}"#
        )
        .unwrap();
        let mut l2 = String::new();
        r.read_line(&mut l2).unwrap(); // run_frames
        let id2 = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
        // 완료를 절대 안 보내고 working만 계속. 데드라인으로 링크가 끊으면 write가 실패해 종료.
        for _ in 0..100_000 {
            if writeln!(w, r#"{{"id":{id2},"ok":true,"result":{{"status":"working"}}}}"#).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = rel_rx.recv();
    });
    let mut link = broker_link::connect(&addr, None, Duration::from_secs(2)).unwrap();
    link.set_deferred_deadline(Duration::from_millis(300));
    let r = link.call("run_frames", serde_json::json!({ "n": 1000 }));
    assert!(
        matches!(r, Err(LinkError::NotConnected)),
        "working keepalive 무한 시 deferred_deadline 초과로 NotConnected(재attach 신호)여야 — wedge 금지: {:?}",
        r.map(|_| "ok")
    );
    // 소켓을 닫아 fake broker의 working write가 broken pipe로 실패→루프 종료하게 한다(안 그러면 링크가
    // 살아있어 write가 안 실패 → 스레드가 100_000회를 다 돌아 h.join이 사실상 무한 대기).
    drop(link);
    rel_tx.send(()).ok();
    h.join().unwrap();
}
