use super::broker;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

// broker를 ephemeral 두 포트로 띄우고 (emu_addr, sess_addr) 반환.
fn start_broker_with(stale: Duration) -> (String, String) {
    let emu = TcpListener::bind("127.0.0.1:0").unwrap();
    let sess = TcpListener::bind("127.0.0.1:0").unwrap();
    let ea = emu.local_addr().unwrap().to_string();
    let sa = sess.local_addr().unwrap().to_string();
    std::thread::spawn(move || broker::serve(emu, sess, stale));
    (ea, sa)
}

fn start_broker() -> (String, String) {
    start_broker_with(Duration::from_secs(15))
}

// 가짜 에뮬레이터: 접속→hello 응답(name, methods)→이후 명령 echo 응답.
fn fake_emu(addr: String, name: Option<String>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let s = TcpStream::connect(addr).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap(); // hello
        let nm = name
            .map(|n| format!(r#","name":"{n}""#))
            .unwrap_or_default();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"]{nm}}}}}"#
        )
        .unwrap();
        // 명령 하나: status → working keepalive + completed(양방향 펌프 검증)
        let mut cmd = String::new();
        if r.read_line(&mut cmd).unwrap() > 0 {
            let id = serde_json::from_str::<serde_json::Value>(cmd.trim()).unwrap()["id"].clone();
            writeln!(
                w,
                r#"{{"id":{id},"ok":true,"result":{{"status":"working"}}}}"#
            )
            .unwrap();
            writeln!(
                w,
                r#"{{"id":{id},"ok":true,"result":{{"connected":true}}}}"#
            )
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
    })
}

#[test]
fn broker_routes_and_pumps_keepalive() {
    let (ea, sa) = start_broker();
    let h = fake_emu(ea, None);
    std::thread::sleep(Duration::from_millis(100)); // 등록 여유

    // 세션: attach(이름 없음, 단일) → status → working 건너뛰고 completed 받기
    let s = TcpStream::connect(sa).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    writeln!(w, r#"{{"v":1,"id":1,"method":"attach","params":{{}}}}"#).unwrap();
    let mut ar = String::new();
    r.read_line(&mut ar).unwrap();
    assert!(ar.contains("attached_name"), "attach 응답: {ar}");

    writeln!(w, r#"{{"v":1,"id":2,"method":"status","params":{{}}}}"#).unwrap();
    // working + completed 두 줄이 와야 함(양방향 펌프)
    let mut l1 = String::new();
    r.read_line(&mut l1).unwrap();
    let mut l2 = String::new();
    r.read_line(&mut l2).unwrap();
    assert!(l1.contains("working"), "첫 줄 working: {l1}");
    assert!(l2.contains("connected"), "둘째 줄 completed: {l2}");
    h.join().unwrap();
}

#[test]
fn broker_atomic_two_port_bind() {
    // 한 포트를 미리 점유 → 같은 포트 이중 바인드가 AddrInUse임을 확인(bind-as-lock 전제).
    // emucap-broker는 에뮬레이터 포트를 먼저 바인드하고, 세션 포트 실패 시 에뮬레이터 포트를
    // drop해 해제한다(부분 점유 없음). 이 테스트는 OS 레벨 guard가 작동하는 전제를 검증.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let second = std::net::TcpListener::bind(addr);
    assert!(
        second.is_err(),
        "이중 바인드는 실패해야 bind-as-lock 선출이 성립"
    );
}

fn attach(sa: &str, name: Option<&str>) -> (TcpStream, String) {
    let s = TcpStream::connect(sa).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s.try_clone().unwrap();
    let p = name
        .map(|n| format!(r#"{{"name":"{n}"}}"#))
        .unwrap_or_else(|| "{}".into());
    writeln!(w, r#"{{"v":1,"id":1,"method":"attach","params":{p}}}"#).unwrap();
    let mut ar = String::new();
    r.read_line(&mut ar).unwrap();
    (s, ar)
}

#[test]
fn broker_routes_by_name_and_ambiguous() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea.clone(), Some("alpha".into()));
    let _b = fake_emu(ea, Some("beta".into()));
    std::thread::sleep(Duration::from_millis(150));
    // 이름 없이 → Ambiguous
    let (_s, ar) = attach(&sa, None);
    assert!(
        ar.contains("ambiguous"),
        "다중인데 이름 없으면 ambiguous: {ar}"
    );
    // 이름 beta → 성공
    let (_s2, ar2) = attach(&sa, Some("beta"));
    assert!(
        ar2.contains("attached_name") && ar2.contains("beta"),
        "beta 라우팅: {ar2}"
    );
}

#[test]
fn broker_second_session_busy() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea, Some("g".into()));
    std::thread::sleep(Duration::from_millis(100));
    let (_s1, ar1) = attach(&sa, Some("g")); // 살아있는 첫 세션
    assert!(ar1.contains("attached_name"), "{ar1}");
    let (_s2, ar2) = attach(&sa, Some("g")); // 둘째 → Busy
    assert!(ar2.contains("busy"), "둘째 세션은 busy: {ar2}");
}

// hello만 응답하고 명령을 echo하지 않은 채 hold_ms 동안 살아있는 에뮬레이터.
fn fake_emu_alive(addr: String, name: &str, hold_ms: u64) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    std::thread::spawn(move || {
        let s = TcpStream::connect(addr).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"],"name":"{name}"}}}}"#
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
    })
}

#[test]
fn is_stale_only_after_threshold() {
    let now = Instant::now();
    let threshold = Duration::from_millis(100);
    assert!(
        !broker::is_stale(now, now, threshold),
        "방금 활동 = 살아있음"
    );
    assert!(
        !broker::is_stale(now, now - Duration::from_millis(50), threshold),
        "임계 내 = 살아있음"
    );
    assert!(
        broker::is_stale(now, now - Duration::from_millis(200), threshold),
        "임계 초과 = stale"
    );
}

#[test]
fn broker_old_session_reader_does_not_clobber_new_pairing() {
    // 같은 EMUCAP_NAME 에뮬레이터가 replace된 뒤, 구 세션의 리더가 뒤늦게 종료하며
    // 신규 세션의 페어링을 unpair하면 안 된다(session generation 가드).
    let (ea, sa) = start_broker();
    let _e1 = fake_emu_alive(ea.clone(), "g", 1000); // 곧 replace됨
    std::thread::sleep(Duration::from_millis(100));

    // 세션 A: 구 에뮬레이터에 페어링(소켓 보관 — 아직 종료 안 함).
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");

    // 같은 이름 재등록(replace) — 신규 에뮬레이터는 오래 살아있다.
    let _e2 = fake_emu_alive(ea.clone(), "g", 3000);
    std::thread::sleep(Duration::from_millis(200)); // A는 replaced 통지, 신규 등록 완료

    // 세션 B: 신규 에뮬레이터에 페어링.
    let (_b_sock, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "B attach(신규 에뮬레이터): {ar_b}"
    );

    // 구 세션 A 종료 → 구 세션 리더가 EOF로 깨어나 정리한다.
    drop(a_sock);
    std::thread::sleep(Duration::from_millis(200));

    // B의 페어링이 유지되면 제3 세션 C는 busy여야 한다.
    // 가드가 없으면 A의 리더가 B를 unpair해 C가 에뮬레이터를 탈취한다.
    let (_c_sock, ar_c) = attach(&sa, Some("g"));
    assert!(
        ar_c.contains("busy"),
        "B 페어링이 유지되어야 C는 busy — 구 세션 리더가 신규 페어링을 clobber함: {ar_c}"
    );
}

#[test]
fn broker_steals_emulator_from_stale_session() {
    // 세션이 hang(명령·heartbeat 모두 없음)하면 짧은 stale 임계 후 신규 attach가 steal한다.
    let (ea, sa) = start_broker_with(Duration::from_millis(150));
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    // 세션 A: attach 후 조용(stale 됨).
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    std::thread::sleep(Duration::from_millis(350)); // > 150ms 임계
                                                    // 세션 B: stale A를 steal.
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "stale 세션에서 steal해야: {ar_b}"
    );
    let _ = a_sock;
}

#[test]
fn broker_keeps_busy_for_active_session() {
    // _ping(heartbeat)을 보내 살아있는 세션은 idle여도 steal당하지 않는다(busy 유지).
    let (ea, sa) = start_broker_with(Duration::from_millis(250));
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (mut a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    // A가 heartbeat를 주기적으로 보내 활동 신호 유지.
    for _ in 0..4 {
        writeln!(a_sock, r#"{{"v":1,"method":"_ping"}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }
    // 마지막 활동이 임계(250ms) 내 → B는 busy.
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("busy"),
        "활동 중 세션은 busy 유지해야: {ar_b}"
    );
}

// hello 응답 후 명령 하나를 읽어 *받은 그대로의 id*를 기억하고, `go` 신호가 올 때까지 응답을 보류한다.
// 신호가 오면 그 id로 (뒤늦은) 응답을 보낸다 — steal 이후 도착하는 in-flight 응답을 재현한다.
fn fake_emu_hold_reply(
    addr: String,
    name: &str,
    go: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    std::thread::spawn(move || {
        let s = TcpStream::connect(addr).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"],"name":"{name}"}}}}"#
        )
        .unwrap();
        // A의 요청 한 줄을 읽어 broker가 부여한(네임스페이스된) id를 그대로 보관.
        let mut cmd = String::new();
        r.read_line(&mut cmd).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(cmd.trim()).unwrap()["id"].clone();
        // steal이 끝날 때까지 응답 보류.
        let _ = go.recv();
        // A의 요청에 대한 뒤늦은 응답 — 펜싱되어 신규 소유자 B에게 배달되면 안 된다.
        writeln!(w, r#"{{"id":{id},"ok":true,"result":{{"stale":true}}}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    })
}

#[test]
fn broker_fences_stale_response_after_steal() {
    // steal 안전: stale 세션 A의 in-flight 응답이 뒤늦게 와도, 세대 펜싱으로 신규 소유자 B가 그것을
    // 자기 응답으로 받지 않아야 한다(A·B가 같은 요청 id를 써도).
    let (ea, sa) = start_broker_with(Duration::from_millis(150));
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let _e = fake_emu_hold_reply(ea, "g", go_rx);
    std::thread::sleep(Duration::from_millis(100)); // 등록 여유

    // 세션 A: attach 후 요청(id=7) 전송 — 에뮬레이터는 읽되 응답 보류.
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    {
        let mut wa = a_sock.try_clone().unwrap();
        writeln!(wa, r#"{{"v":1,"id":7,"method":"status","params":{{}}}}"#).unwrap();
    }
    std::thread::sleep(Duration::from_millis(80)); // 에뮬레이터가 A 요청을 읽을 시간

    // A가 조용해져(heartbeat 없음) stale → B가 steal.
    std::thread::sleep(Duration::from_millis(250)); // > 150ms 임계
    let (b_sock, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "B가 stale A를 steal: {ar_b}"
    );
    // B도 같은 id=7로 자기 요청 전송(겹치는 id 공간 재현).
    {
        let mut wb = b_sock.try_clone().unwrap();
        writeln!(wb, r#"{{"v":1,"id":7,"method":"status","params":{{}}}}"#).unwrap();
    }

    // 이제 에뮬레이터가 A의 요청에 대한 뒤늦은 응답을 방출.
    go_tx.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(150));

    // B는 A의 stale 응답(id=7, "stale":true)을 자기 것으로 받아선 안 된다.
    b_sock
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut rb = BufReader::new(b_sock);
    let mut got = String::new();
    let n = rb.read_line(&mut got).unwrap_or(0);
    assert!(
        !got.contains("stale"),
        "B가 옛 세션 A의 in-flight 응답을 받으면 안 됨(fence): n={n}, got={got:?}"
    );
    let _ = a_sock;
}

#[test]
fn broker_persists_across_session() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea, Some("g".into()));
    std::thread::sleep(Duration::from_millis(100));
    {
        let (s1, ar1) = attach(&sa, Some("g"));
        assert!(ar1.contains("attached_name"));
        drop(s1);
    }
    std::thread::sleep(Duration::from_millis(100)); // 언페어 여유
    let (_s2, ar2) = attach(&sa, Some("g")); // 재attach 같은 에뮬레이터
    assert!(
        ar2.contains("attached_name") && ar2.contains("g"),
        "재attach 복귀: {ar2}"
    );
}
