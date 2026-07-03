use super::link::{EmulatorLink, LinkError};
use super::tcp;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

fn hello_parts(line: &str) -> (serde_json::Value, String) {
    let v = serde_json::from_str::<serde_json::Value>(line.trim()).unwrap();
    assert_eq!(
        v.get("method").and_then(|m| m.as_str()),
        Some("hello"),
        "첫 요청은 hello여야 한다: {line}"
    );
    let token = v
        .get("params")
        .and_then(|p| p.get("session_token"))
        .and_then(|t| t.as_str())
        .expect("hello params.session_token")
        .to_string();
    (v["id"].clone(), token)
}

fn write_hello_response(w: &mut TcpStream, line: &str, methods: &[&str]) {
    let (id, token) = hello_parts(line);
    writeln!(
        w,
        "{}",
        serde_json::json!({
            "id": id,
            "ok": true,
            "result": {
                "protocol_version": 1,
                "methods": methods,
                "session_token": token,
            }
        })
    )
    .unwrap();
}

fn write_hello_response_with_token(w: &mut TcpStream, line: &str, methods: &[&str], token: &str) {
    let (id, _) = hello_parts(line);
    writeln!(
        w,
        "{}",
        serde_json::json!({
            "id": id,
            "ok": true,
            "result": {
                "protocol_version": 1,
                "methods": methods,
                "session_token": token,
            }
        })
    )
    .unwrap();
}

/// Lua 역할: 접속해서 hello에 답하고, 이어 read_memory 요청에 응답한다.
fn fake_lua(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();

    // hello 요청 수신 → 응답
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.contains("\"hello\""),
        "첫 요청은 hello여야 한다: {line}"
    );
    write_hello_response(&mut w, &line, &["read_memory"]);

    // read_memory 요청 수신 → 응답
    let mut line2 = String::new();
    reader.read_line(&mut line2).unwrap();
    assert!(line2.contains("\"read_memory\""));
    let id2 = serde_json::from_str::<serde_json::Value>(line2.trim()).unwrap()["id"].clone();
    writeln!(w, r#"{{"id":{id2},"ok":true,"result":{{"hex":"00ff"}}}}"#).unwrap();
}

#[test]
fn tcp_link_does_hello_and_call() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(2)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua(addr, tx));
    rx.recv().unwrap();

    let out = link
        .call("read_memory", serde_json::json!({ "address": 0 }))
        .unwrap();
    assert_eq!(out["hex"], "00ff");
    assert_eq!(link.capabilities().methods, vec!["read_memory".to_string()]);
    h.join().unwrap();
}

#[test]
fn tcp_link_rejects_wrong_session_token() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(2)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        tx.send(()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // hello
        write_hello_response_with_token(&mut w, &line, &["status"], "wrong-session-token");
    });
    rx.recv().unwrap();

    let err = link.call("status", serde_json::json!({})).unwrap_err();
    assert!(
        matches!(err, LinkError::IdentityMismatch { .. }),
        "다른 세션 토큰을 echo한 에뮬레이터는 연결로 받아들이면 안 된다: {err:?}"
    );
    h.join().unwrap();
}

fn fake_lua_with_keepalive(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &["run_frames"]);
    let mut line2 = String::new();
    reader.read_line(&mut line2).unwrap(); // run_frames
    let id2 = serde_json::from_str::<serde_json::Value>(line2.trim()).unwrap()["id"].clone();
    // 먼저 working keepalive, 그 다음 최종 completed
    writeln!(
        w,
        r#"{{"id":{id2},"ok":true,"result":{{"status":"working"}}}}"#
    )
    .unwrap();
    writeln!(
        w,
        r#"{{"id":{id2},"ok":true,"result":{{"status":"completed","frames":5}}}}"#
    )
    .unwrap();
}

#[test]
fn tcp_link_waits_through_keepalive() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(2)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua_with_keepalive(addr, tx));
    rx.recv().unwrap();
    let out = link.call("run_frames", serde_json::json!({"n":5})).unwrap();
    assert_eq!(out["status"], "completed");
    assert_eq!(out["frames"], 5);
    h.join().unwrap();
}

/// hello만 답하고 바로 끊는 클라이언트.
fn fake_lua_hello_then_close(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    write_hello_response(&mut w, &line, &[]);
    // 이후 끊는다(스코프 종료 시 stream drop).
}

// 클라이언트가 죽은 뒤 새 클라이언트로 재연결되는지.
#[test]
fn tcp_link_reconnects_after_client_disconnect() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
    let addr = link.local_addr().to_string();

    // client 1: hello 후 끊김 → status 호출은 에러(죽은 연결 비워져야 함)
    let (tx1, rx1) = std::sync::mpsc::channel();
    let a1 = addr.clone();
    let h1 = std::thread::spawn(move || fake_lua_hello_then_close(a1, tx1));
    rx1.recv().unwrap();
    let r = link.call("status", serde_json::json!({}));
    assert!(
        matches!(r, Err(LinkError::NotConnected)),
        "죽은 클라이언트 write/read는 미연결으로 분류해야: {r:?}"
    );
    h1.join().unwrap();

    // client 2: 정상 응답 → 재연결되어 받아야 함
    let (tx2, rx2) = std::sync::mpsc::channel();
    let h2 = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        tx2.send(()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // hello
        write_hello_response(&mut w, &line, &[]);
        let mut l2 = String::new();
        reader.read_line(&mut l2).unwrap(); // status
        let id2 = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"connected":true}}}}"#
        )
        .unwrap();
    });
    rx2.recv().unwrap();

    let mut ok = false;
    for _ in 0..50 {
        if let Ok(v) = link.call("status", serde_json::json!({})) {
            if v["connected"] == true {
                ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ok, "클라이언트 교체 후 재연결되어 응답을 받아야 한다");
    h2.join().unwrap();
}

/// hello 후 read_memory에 "엉뚱한 id의 늦은 응답 + 올바른 응답" 순으로 답한다.
fn fake_lua_stale_then_real(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &[]);
    let mut l2 = String::new();
    reader.read_line(&mut l2).unwrap(); // read_memory
    let real_id = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
    // 이전 명령의 늦은 응답(엉뚱한 id) → 버려져야 함, 그 다음 올바른 응답.
    writeln!(w, r#"{{"id":999,"ok":true,"result":{{"hex":"stale"}}}}"#).unwrap();
    writeln!(
        w,
        r#"{{"id":{real_id},"ok":true,"result":{{"hex":"00ff"}}}}"#
    )
    .unwrap();
}

#[test]
fn tcp_link_discards_stale_id_response() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(2)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua_stale_then_real(addr, tx));
    rx.recv().unwrap();
    let out = link.call("read_memory", serde_json::json!({})).unwrap();
    assert_eq!(
        out["hex"], "00ff",
        "엉뚱한 id의 늦은 응답은 버리고 올바른 응답을 받아야 한다"
    );
    h.join().unwrap();
}

/// hello만 답하고 이후 응답 없이 행한다(소켓은 release까지 열어둬 EOF가 아닌 타임아웃을 만든다).
fn fake_lua_hello_then_hang(
    addr: String,
    ready: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &[]);
    // 이후 요청에 응답하지 않고 소켓을 연 채 행(EOF 아님 → 호출이 타임아웃).
    let _ = release.recv();
}

// 클라이언트가 EOF 없이 행할 때, 호출 타임아웃 후 conn을 비워 새 클라이언트로 재연결되는지.
#[test]
fn tcp_link_reconnects_after_timeout_on_hung_client() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_millis(150)).unwrap();
    let addr = link.local_addr().to_string();

    // client 1: hello 후 행 → status는 타임아웃, conn이 비워져야 함
    let (tx1, rx1) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let a1 = addr.clone();
    let h1 = std::thread::spawn(move || fake_lua_hello_then_hang(a1, tx1, rel_rx));
    rx1.recv().unwrap();
    let r = link.call("status", serde_json::json!({}));
    assert!(
        matches!(r, Err(LinkError::Timeout)),
        "행된 클라이언트는 타임아웃이어야: {r:?}"
    );

    // client 2: 정상 응답 → 재연결되어 받아야 함(타임아웃이 conn을 비웠을 때만 가능)
    let (tx2, rx2) = std::sync::mpsc::channel();
    let h2 = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        tx2.send(()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // hello
        write_hello_response(&mut w, &line, &[]);
        let mut l2 = String::new();
        reader.read_line(&mut l2).unwrap(); // status
        let id2 = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"connected":true}}}}"#
        )
        .unwrap();
    });
    rx2.recv().unwrap();

    let mut ok = false;
    for _ in 0..50 {
        if let Ok(v) = link.call("status", serde_json::json!({})) {
            if v["connected"] == true {
                ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ok,
        "행 타임아웃 후 새 클라이언트로 재연결되어야 한다(wedge 금지)"
    );
    rel_tx.send(()).ok();
    h1.join().unwrap();
    h2.join().unwrap();
}

// ROM 교체 재연결: 이전 emu가 죽어 conn이 아직 붙들린 상태에서 새 emu가 접속하면, 죽은 conn의
// timeout 누적(MAX×timeout, ~10초)을 기다리지 않고 "새 접속 대기" 신호로 즉시 채택해야 한다.
#[test]
fn tcp_link_adopts_pending_client_while_old_conn_held() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_millis(150)).unwrap();
    let addr = link.local_addr().to_string();

    // client 1: hello + status#1 응답 후 행(서버가 conn을 계속 붙든 채 — pkill된 이전 emu 모사).
    let (tx1, rx1) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let a1 = addr.clone();
    let h1 = std::thread::spawn(move || {
        let stream = TcpStream::connect(a1).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        tx1.send(()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // hello
        write_hello_response(&mut w, &line, &[]);
        let mut l1 = String::new();
        reader.read_line(&mut l1).unwrap(); // status #1
        let id1 = serde_json::from_str::<serde_json::Value>(l1.trim()).unwrap()["id"].clone();
        writeln!(w, r#"{{"id":{id1},"ok":true,"result":{{"which":"A"}}}}"#).unwrap();
        let _ = rel_rx.recv(); // 이후 행(응답 없음)
    });
    rx1.recv().unwrap();
    assert_eq!(
        link.call("status", serde_json::json!({})).unwrap()["which"],
        "A",
        "client1이 먼저 붙어야"
    );

    // client 2(새 ROM) 접속 — 서버 conn은 아직 client1(dead)을 붙든 채다.
    let (tx2, rx2) = std::sync::mpsc::channel();
    let a2 = addr.clone();
    let h2 = std::thread::spawn(move || {
        let stream = TcpStream::connect(a2).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        tx2.send(()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // hello
        write_hello_response(&mut w, &line, &[]);
        let mut l2 = String::new();
        reader.read_line(&mut l2).unwrap(); // status
        let id2 = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
        writeln!(w, r#"{{"id":{id2},"ok":true,"result":{{"which":"B"}}}}"#).unwrap();
    });
    rx2.recv().unwrap();

    // 다음 status: ensure_connected가 대기 중 client2를 채택해야 한다(client1 timeout 누적을 안 기다리고).
    let mut adopted = false;
    for _ in 0..50 {
        if let Ok(v) = link.call("status", serde_json::json!({})) {
            if v["which"] == "B" {
                adopted = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        adopted,
        "새 client 대기 시 붙들린 dead conn을 즉시 교체·채택해야(ROM 교체 wedge 금지)"
    );
    rel_tx.send(()).ok();
    h1.join().unwrap();
    h2.join().unwrap();
}

// 포트 0(기준)으로 지연 바인드하면 OS가 임시포트를 배정한다. endpoint_port()는 후보
// 문자열(":0")이 아니라 실제로 바인드된 포트를 보고해야 한다 — 안 그러면 status가 0을
// 알려 에이전트가 에뮬레이터를 0번 포트로 띄워 영영 연결되지 않는다(조용한 실패).
#[test]
fn tcp_link_reports_actual_ephemeral_port_when_base_is_zero() {
    use super::link::EmulatorLink;
    let mut link = tcp::lazy("127.0.0.1:0", Duration::from_millis(100));
    // 에뮬레이터가 없으니 NotConnected지만, 그 과정에서 :0에 바인드되며 임시포트가 잡힌다.
    let _ = link.call("status", serde_json::json!({}));
    let port = link.endpoint_port().expect("바인드된 포트");
    assert_ne!(
        port, 0,
        "포트 0 바인드 시 OS가 배정한 실제 임시포트를 보고해야(0 오보고 금지)"
    );
}

#[test]
fn tcp_link_preaccepts_after_not_connected_status() {
    use super::link::EmulatorLink;
    let mut link = tcp::lazy("127.0.0.1:0", Duration::from_secs(2));

    let first = link.call("status", serde_json::json!({})).unwrap_err();
    assert!(
        matches!(first, LinkError::NotConnected),
        "첫 status는 포트만 열고 미연결을 보고해야: {first:?}"
    );
    let addr = format!(
        "127.0.0.1:{}",
        link.endpoint_port().expect("첫 status가 바인드한 포트")
    );

    let (hello_tx, hello_rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;

        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(
            hello.contains("\"hello\""),
            "preaccept가 다음 tool call 전 hello를 보내야 한다: {hello}"
        );
        write_hello_response(&mut w, &hello, &["status"]);
        hello_tx.send(()).unwrap();

        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.contains("\"status\""));
        let id2 = serde_json::from_str::<serde_json::Value>(status.trim()).unwrap()["id"].clone();
        writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"connected":true}}}}"#
        )
        .unwrap();
    });

    hello_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("background preaccept가 hello를 먼저 보내야 한다");
    let out = link.call("status", serde_json::json!({})).unwrap();
    assert_eq!(out["connected"], true);
    assert_eq!(link.capabilities().methods, vec!["status".to_string()]);
    h.join().unwrap();
}

// 기준 포트가 u16 최댓값 부근이면 base+off가 u16 범위를 넘는다. 이때 0/저포트로 wrap해
// 엉뚱한(임시/예약) 포트에 바인드하지 말고 PortBusy로 보고해야 한다.
#[test]
fn tcp_link_does_not_wrap_past_u16_max() {
    use super::link::EmulatorLink;
    let owner = match tcp::bind("127.0.0.1:65535", Duration::from_millis(100)) {
        Ok(o) => o,
        Err(_) => return, // 환경상 65535를 못 잡으면 스킵
    };
    let mut loser = tcp::lazy("127.0.0.1:65535", Duration::from_millis(100));
    let err = loser.call("status", serde_json::json!({})).unwrap_err();
    assert!(
        matches!(err, LinkError::PortBusy { .. }),
        "u16 최댓값 초과 후보로 wrap(→0/저포트)하지 말고 PortBusy여야: {err:?}"
    );
    drop(owner);
}

// hello 후 올바른 id 응답 없이 불일치 id 프레임만 쏟아붓는 클라이언트(스트림 desync 폭주).
fn fake_lua_id_mismatch_flood(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &[]);
    let mut l2 = String::new();
    reader.read_line(&mut l2).unwrap(); // read_memory
                                        // 올바른 id를 절대 보내지 않고 불일치 id 프레임을 다수 전송.
    for _ in 0..2000 {
        if writeln!(w, r#"{{"id":999999,"ok":true,"result":{{"hex":"00"}}}}"#).is_err() {
            break; // 수신측이 끊으면(수정 후 빠른 실패) 종료
        }
    }
    std::thread::sleep(Duration::from_millis(300));
}

// id 불일치 프레임이 끝없이 와도 raw_call이 무한 점유되면 안 된다 — 한도/데드라인으로
// Protocol 에러를 내고 연결을 비워야 한다.
#[test]
fn tcp_link_bails_on_id_mismatch_flood() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(3)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua_id_mismatch_flood(addr, tx));
    rx.recv().unwrap();
    let r = link.call("read_memory", serde_json::json!({}));
    assert!(
        matches!(r, Err(LinkError::Protocol(_))),
        "id 불일치 프레임 폭주는 한도로 Protocol 에러를 내야(무한 점유 금지): {r:?}"
    );
    h.join().unwrap();
}

// 세션 고정: 영속화 파일에 적힌 포트를 (재시작 시) 최저빈포트보다 우선 재바인드해야 한다.
// 서버 재시작 후에도 같은 포트를 되찾아야 그 포트로 띄워둔 에뮬레이터가 자동 재연결된다.
#[test]
fn tcp_link_prefers_persisted_port_over_lowest_free() {
    use super::link::EmulatorLink;
    // 빈 base 포트 확보(임시포트 하나 잡았다 놓음)
    let probe = tcp::bind("127.0.0.1:0", Duration::from_millis(100)).unwrap();
    let base = probe.local_addr().port();
    drop(probe);
    let preferred = match base.checked_add(2) {
        Some(p) => p,
        None => return, // base가 u16 끝부근이면 스킵
    };
    // preferred가 실제로 자유로운지 확인(점유 중이면 환경상 스킵 — flaky 방지)
    match std::net::TcpListener::bind(format!("127.0.0.1:{preferred}")) {
        Ok(l) => drop(l),
        Err(_) => return,
    }
    // portfile에 preferred 기록 → lazy(base)가 base(최저빈)가 아니라 preferred를 잡아야 한다.
    let pf = tcp::port_persist_path(base);
    tcp::write_persisted_port(&pf, preferred);

    let mut link = tcp::lazy(&format!("127.0.0.1:{base}"), Duration::from_millis(100));
    let _ = link.call("status", serde_json::json!({})); // 바인드 트리거(에뮬레이터 없음 → NotConnected)
    let chosen = link.endpoint_port().expect("바인드된 포트");
    let _ = std::fs::remove_file(&pf); // 정리

    assert_eq!(
        chosen, preferred,
        "portfile의 포트를 최저빈포트(base={base})보다 우선해야: chosen={chosen} preferred={preferred}"
    );
}

// 영속화 포트가 점유 중이면(다른 인스턴스가 그 포트를 씀) 스캔으로 폴백해 다른 포트라도 잡아야 한다
// (블록/실패 금지 — 데드락·기아 방지).
#[test]
fn tcp_link_falls_back_to_scan_when_persisted_port_busy() {
    use super::link::EmulatorLink;
    let probe = tcp::bind("127.0.0.1:0", Duration::from_millis(100)).unwrap();
    let base = probe.local_addr().port();
    drop(probe);
    let busy_pref = match base.checked_add(1) {
        Some(p) => p,
        None => return,
    };
    // preferred 포트를 다른 소유자가 점유
    let owner = match tcp::bind(
        &format!("127.0.0.1:{busy_pref}"),
        Duration::from_millis(100),
    ) {
        Ok(o) => o,
        Err(_) => return, // 못 잡으면 스킵
    };
    let pf = tcp::port_persist_path(base);
    tcp::write_persisted_port(&pf, busy_pref);

    let mut link = tcp::lazy(&format!("127.0.0.1:{base}"), Duration::from_millis(100));
    let _ = link.call("status", serde_json::json!({}));
    let chosen = link.endpoint_port().expect("폴백으로라도 바인드돼야");
    let _ = std::fs::remove_file(&pf);
    drop(owner);

    assert_ne!(
        chosen, busy_pref,
        "점유된 영속화 포트는 못 잡으니 다른 포트로 폴백해야"
    );
    assert!(
        chosen >= base,
        "스캔은 기준 포트 이상에서: chosen={chosen} base={base}"
    );
}

#[test]
fn tcp_link_not_connected_when_no_client() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_millis(200)).unwrap();
    let err = link.call("read_memory", serde_json::json!({})).unwrap_err();
    assert!(matches!(err, LinkError::NotConnected));
}

// 포트를 다른 인스턴스가 점유 중이면(다중 세션) "에뮬레이터 없음"이 아니라 PortBusy로
// 구분 보고. 에이전트가 포트 분리/타 세션 종료를 알 수 있게 한다.
#[test]
fn tcp_link_auto_selects_next_port_when_occupied() {
    use super::link::EmulatorLink;
    let owner = tcp::bind("127.0.0.1:0", Duration::from_millis(100)).unwrap();
    let base_port = owner.local_addr().port();
    let addr = owner.local_addr().to_string();
    let mut loser = tcp::lazy(&addr, Duration::from_millis(100));
    // 기준 포트가 점유 중 → 다음 빈 포트로 자동 이동. 거기엔 에뮬레이터가 없으니 NotConnected.
    let err = loser.call("status", serde_json::json!({})).unwrap_err();
    assert!(
        matches!(err, LinkError::NotConnected),
        "auto-port로 다음 포트를 잡고 에뮬레이터 없음을 보고해야(PortBusy 아님): {err:?}"
    );
    // 잡은 포트가 점유된 기준 포트가 아니라 다른(다음) 포트여야 — N 세션 자동 격리의 핵심.
    let chosen = loser.endpoint_port().expect("바인드된 포트");
    assert_ne!(chosen, base_port, "점유된 기준 포트를 피해야");
    assert!(
        chosen > base_port,
        "다음 포트로 옮겨야: chosen={chosen} base={base_port}"
    );
    drop(owner);
}

#[test]
fn session_token_reused_for_own_cwd_on_reconnect() {
    // 같은 cwd로 발급한 토큰은 own → 서버 재기동/재연결 시 재사용해 실행 중 에뮬 reclaim.
    let own = tcp::new_session_token();
    assert!(
        tcp::session_token_is_own(&own),
        "현재 cwd로 발급한 토큰은 own이어야 한다"
    );
    // 프리픽스(cwd_hash) 한 글자만 바꿔 다른 세션처럼 → foreign(재사용 안 함, 새 토큰 유지).
    let mut chars: Vec<char> = own.chars().collect();
    chars[0] = if chars[0] == '0' { '1' } else { '0' };
    let foreign: String = chars.into_iter().collect();
    assert!(
        !tcp::session_token_is_own(&foreign),
        "다른 cwd_hash 프리픽스는 foreign이어야 한다"
    );
}

#[test]
fn session_token_path_parent_exists() {
    let path = tcp::session_token_path(59778);
    assert!(
        path.parent().is_some_and(|p| p.is_dir()),
        "session token parent must exist for best-effort write: {}",
        path.display()
    );
}

#[test]
fn reusable_session_token_reuses_own_mints_for_foreign() {
    // 재사용 경로 통합: own 토큰파일은 재사용(reclaim), foreign은 None(새 토큰 발급→guard가 차단).
    let port = 59777u16; // 테스트 전용 포트(충돌 회피)
    let path = tcp::session_token_path(port);
    let own = tcp::new_session_token();
    std::fs::write(&path, &own).unwrap();
    assert_eq!(
        tcp::reusable_session_token(port).as_deref(),
        Some(own.as_str()),
        "own은 재사용"
    );
    let mut chars: Vec<char> = own.chars().collect();
    chars[0] = if chars[0] == '0' { '1' } else { '0' };
    let foreign: String = chars.into_iter().collect();
    std::fs::write(&path, &foreign).unwrap();
    assert_eq!(
        tcp::reusable_session_token(port),
        None,
        "foreign은 재사용 안 함"
    );
    let _ = std::fs::remove_file(&path);
}

/// hello만 답하고 이후 소켓에서 절대 read하지 않는 클라이언트 — 서버 송신 버퍼를 채워 큰 요청의
/// write_all이 스톨→타임아웃되게 한다. release까지 소켓을 연 채 둔다(EOF 아님 → 진짜 write timeout).
fn fake_lua_hello_then_never_read(
    addr: String,
    ready: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &[]);
    // 이후 요청을 절대 읽지 않는다 — 서버 송신 버퍼가 차 큰 요청 write_all이 스톨한다.
    let _ = release.recv();
}

// write_all이 타임아웃되면(플러드된 emu가 recv를 안 비움) 요청 라인의 일부만 전송됐을 수 있다. 같은
// conn을 재사용하면 다음 요청이 그 부분 프레임 뒤에 이어붙어 상대 NDJSON 파서가 오염된다. 그래서
// 쓰기 타임아웃은 비치명 Timeout을 반환하되 conn을 poison(drop)해 다음 호출이 새 클라이언트를
// 재수락하게 한다(부분 송신은 복구 불가 — 읽기 타임아웃의 conn.pending 보존과 대비).
#[test]
fn tcp_link_write_timeout_poisons_conn() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_millis(200)).unwrap();
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua_hello_then_never_read(addr, tx, rel_rx));
    rx.recv().unwrap();
    // 큰 params로 요청 한 줄을 수십 MB로 만들어 송신 버퍼(+peer recv 버퍼)를 넘긴다 → write_all 스톨.
    let big = "x".repeat(32 * 1024 * 1024);
    let r = link.call("read_memory", serde_json::json!({ "blob": big }));
    assert!(
        matches!(r, Err(LinkError::Timeout)),
        "쓰기 타임아웃은 비치명 Timeout이어야(Protocol 아님): {:?}",
        r.map(|_| "ok")
    );
    // 핵심: 부분 송신된 conn은 버려져야 한다 — 다음 호출이 부분 프레임에 이어붙지 않고 재연결하도록.
    assert!(
        !link.has_conn(),
        "쓰기 타임아웃 후 conn은 poison(drop)돼야 — 부분 프레임에 다음 요청이 이어붙으면 프레이밍 오염"
    );
    rel_tx.send(()).ok();
    h.join().unwrap();
}

/// hello 후 run_frames에 완료 프레임 없이 working keepalive를 무한히 흘리는 클라이언트(deferred flood).
/// working은 id가 일치해 매번 consecutive_timeouts를 리셋하므로 3-timeout 가드로는 못 끊는다.
fn fake_lua_working_flood(addr: String, ready: std::sync::mpsc::Sender<()>) {
    let stream = TcpStream::connect(addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    ready.send(()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // hello
    write_hello_response(&mut w, &line, &["run_frames"]);
    let mut l2 = String::new();
    reader.read_line(&mut l2).unwrap(); // run_frames
    let id2 = serde_json::from_str::<serde_json::Value>(l2.trim()).unwrap()["id"].clone();
    // 완료를 절대 안 보내고 working만 계속. 서버가 데드라인으로 conn을 드롭하면 write가 실패해 종료.
    for _ in 0..100_000 {
        if writeln!(
            w,
            r#"{{"id":{id2},"ok":true,"result":{{"status":"working"}}}}"#
        )
        .is_err()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// working keepalive가 끝없이 와도(매번 타임아웃 카운터 리셋) raw_call은 deferred 데드라인으로
// 끊고 Timeout을 내야 한다 — 안 그러면 SharedLink mutex를 쥔 채 영구 wedge된다.
#[test]
fn tcp_link_bails_on_working_flood_past_deadline() {
    let mut link = tcp::bind("127.0.0.1:0", Duration::from_secs(2)).unwrap();
    link.set_deferred_deadline(Duration::from_millis(300));
    let addr = link.local_addr().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || fake_lua_working_flood(addr, tx));
    rx.recv().unwrap();
    let start = std::time::Instant::now();
    let r = link.call("run_frames", serde_json::json!({ "n": 5 }));
    let elapsed = start.elapsed();
    assert!(
        matches!(r, Err(LinkError::Timeout)),
        "working 플러드는 데드라인으로 Timeout이어야(무한 점유 금지): {r:?}"
    );
    // 데드라인(300ms)이 read timeout(2s)보다 먼저 끊어야 — read timeout이었다면 working이 계속 와서
    // 아예 끊기지도 않는다. 넉넉한 상한으로 판정.
    assert!(
        elapsed < Duration::from_millis(1500),
        "deferred 데드라인이 개별 read timeout보다 먼저 컷오프해야: {elapsed:?}"
    );
    h.join().unwrap();
}
