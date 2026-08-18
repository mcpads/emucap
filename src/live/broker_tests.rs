use super::broker;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

// Start the broker on two ephemeral ports and return (emulator_addr, session_addr).
fn start_broker() -> (String, String) {
    let emu = TcpListener::bind("127.0.0.1:0").unwrap();
    let sess = TcpListener::bind("127.0.0.1:0").unwrap();
    let ea = emu.local_addr().unwrap().to_string();
    let sa = sess.local_addr().unwrap().to_string();
    std::thread::spawn(move || broker::serve(emu, sess));
    (ea, sa)
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

fn fake_mesen_emu(
    addr: String,
    name: &str,
    host_features: &[&str],
    hold_ms: u64,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    let host_features: Vec<String> = host_features
        .iter()
        .map(|value| value.to_string())
        .collect();
    std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "methods": ["status", "record_window", "abort_recording"],
                    "name": name,
                    "adapter": "mesen2-live",
                    "mesen_host_api": 1,
                    "host_features": host_features,
                    "memory_types": ["workram"],
                    "memory_regions": [{"memory_type": "workram", "size": 131072}],
                    "breakpoint_kinds": [{
                        "kind": "device_boundary",
                        "range_unit": "scanline",
                        "memory_type_used": false,
                        "snapshot": true
                    }],
                    "host_build": {
                        "upstream_commit": "0123456789abcdef0123456789abcdef01234567",
                        "patchset_sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    },
                    "recording": {
                        "revision": super::recording_capability::INITIAL_RECORDING_CAPABILITY_REVISION,
                        "origins": ["next_frame_boundary"],
                        "units": ["frames"],
                        "default_event_classes": ["frame_boundary"],
                        "event_classes": [{
                            "id": "frame_boundary",
                            "contract_sha256": "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd",
                            "clock_domains": ["frame"],
                            "exact": true
                        }],
                        "limits": {
                            "max_frames": 300,
                            "max_events": 100000,
                            "max_bytes": 67108864,
                            "max_line_bytes": 65536,
                            "max_host_ms": 30000,
                            "progress_interval_ms": 250
                        }
                    }
                }
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
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
fn broker_attach_preserves_contract_advertisement() {
    let (emulator_addr, session_addr) = start_broker();
    let emulator = std::thread::spawn(move || {
        let stream = TcpStream::connect(emulator_addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "name": "nds-contracts",
                    "adapter": "desmume-nds-rust-gdb",
                    "system": "nds",
                    "methods": ["status", "step_instructions", "call_stack"],
                    "contracts": crate::contracts::advertisement_value(&[
                        "nds.execution.frame-step-vblank",
                        "nds.call-stack.best-effort",
                    ]),
                }
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
    std::thread::sleep(Duration::from_millis(100));

    let stream = TcpStream::connect(session_addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    writeln!(
        writer,
        "{}",
        serde_json::json!({
            "v": 1,
            "id": 1,
            "method": "attach",
            "params": {"name": "nds-contracts"},
        })
    )
    .unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let value: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(
        value["result"]["contracts"]["catalog"],
        crate::contracts::CATALOG_ID
    );
    assert_eq!(
        value["result"]["contracts"]["active_exceptions"],
        serde_json::json!([
            "nds.execution.frame-step-vblank",
            "nds.call-stack.best-effort"
        ])
    );
    emulator.join().unwrap();
}

#[test]
fn broker_rejects_mesen_without_native_halt_features() {
    let (emu_addr, session_addr) = start_broker();
    let emulator = fake_mesen_emu(emu_addr, "unpatched", &["code_break_idle"], 200);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&session_addr, Some("unpatched"));
    assert!(
        response.contains("no_such_emulator"),
        "incompatible Mesen must not enter the broker registry: {response}"
    );
    emulator.join().unwrap();
}

#[test]
fn broker_forwards_mesen_native_halt_identity() {
    let (emu_addr, session_addr) = start_broker();
    let emulator = fake_mesen_emu(
        emu_addr,
        "patched",
        &["code_break_idle", "native_halt_service"],
        500,
    );
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&session_addr, Some("patched"));
    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    let result = &response["result"];
    assert_eq!(result["mesen_host_api"], 1);
    assert_eq!(
        result["host_features"],
        serde_json::json!(["code_break_idle", "native_halt_service"])
    );
    assert_eq!(
        result["memory_regions"],
        serde_json::json!([{"memory_type": "workram", "size": 131072}])
    );
    assert_eq!(
        result["breakpoint_kinds"],
        serde_json::json!([{
            "kind": "device_boundary",
            "range_unit": "scanline",
            "memory_type_used": false,
            "snapshot": true
        }])
    );
    assert_eq!(
        result["host_build"]["upstream_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        result["recording"]["default_event_classes"],
        serde_json::json!(["frame_boundary"])
    );
    emulator.join().unwrap();
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
    let params = name
        .map(|name| serde_json::json!({"name": name}))
        .unwrap_or_else(|| serde_json::json!({}));
    attach_with_params(sa, params)
}

fn attach_with_params(sa: &str, params: serde_json::Value) -> (TcpStream, String) {
    let s = TcpStream::connect(sa).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s.try_clone().unwrap();
    writeln!(
        w,
        "{}",
        serde_json::json!({"v": 1, "id": 1, "method": "attach", "params": params})
    )
    .unwrap();
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

#[test]
fn broker_exact_registration_can_resume_the_same_application_link() {
    let (ea, sa) = start_broker();
    let _emulator = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (old_session, first) = attach(&sa, Some("g"));
    let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    let registration_id = first["result"]["broker_registration_id"]
        .as_u64()
        .expect("broker registration identity");

    let (_new_session, resumed) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": registration_id,
        }),
    );
    assert!(
        resumed.contains("attached_name") && resumed.contains("broker_registration_id"),
        "exact registration should resume the same application link: {resumed}"
    );
    drop(old_session);
}

#[test]
fn broker_rejects_a_reconnect_after_emulator_registration_changes() {
    let (ea, sa) = start_broker();
    let _emulator = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": u64::MAX,
        }),
    );
    assert!(
        response.contains("identity_mismatch"),
        "a stale registration identity must not bind a different emulator: {response}"
    );
}

#[test]
fn broker_accepts_reregistration_only_for_the_same_returned_launch() {
    let (ea, sa) = start_broker();
    let _first_emulator = fake_managed_emu_alive(ea.clone(), "g", "launch-same", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (first_session, first) = attach(&sa, Some("g"));
    let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    let first_registration = first["result"]["broker_registration_id"].as_u64().unwrap();

    let _replacement_emulator = fake_managed_emu_alive(ea, "g", "launch-same", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (_continued_session, continued) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": first_registration,
            "expected_launch_id": "launch-same",
        }),
    );
    let continued: serde_json::Value = serde_json::from_str(continued.trim()).unwrap();
    assert_eq!(continued["ok"], true);
    assert_ne!(
        continued["result"]["broker_registration_id"], first_registration,
        "adapter re-registration must retain a distinct transport identity"
    );
    drop(first_session);
}

#[test]
fn broker_rejects_reregistration_for_a_different_launch() {
    let (ea, sa) = start_broker();
    let _emulator = fake_managed_emu_alive(ea, "g", "launch-new", 4000);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": u64::MAX,
            "expected_launch_id": "launch-old",
        }),
    );
    assert!(
        response.contains("identity_mismatch"),
        "a durable launch mismatch must reject transport recovery: {response}"
    );
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

fn fake_managed_emu_alive(
    addr: String,
    name: &str,
    launch_id: &str,
    hold_ms: u64,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    let launch_id = launch_id.to_string();
    std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "methods": ["status"],
                    "name": name,
                    "launch_id": launch_id,
                },
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
    })
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
fn broker_does_not_transfer_an_open_session_after_elapsed_time() {
    // heartbeat 지연은 관측 신호일 뿐이며 열린 세션의 배타 제어권을 이전하지 않는다.
    let (ea, sa) = start_broker();
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    std::thread::sleep(Duration::from_millis(350));
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("busy"),
        "열린 세션은 경과 시간만으로 이전되면 안 된다: {ar_b}"
    );
    let _ = a_sock;
}

#[test]
fn broker_heartbeat_does_not_change_exclusive_control() {
    // _ping은 transport 관측일 뿐이며 열린 세션의 배타 제어권을 바꾸지 않는다.
    let (ea, sa) = start_broker();
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (mut a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    // A가 heartbeat를 주기적으로 보내도 소유권 근거로 사용하지 않는다.
    for _ in 0..4 {
        writeln!(a_sock, r#"{{"v":1,"method":"_ping"}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }
    // A의 연결이 열려 있으므로 B는 busy.
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("busy"),
        "활동 중 세션은 busy 유지해야: {ar_b}"
    );
}

// hello 응답 후 명령 하나를 읽어 *받은 그대로의 id*를 기억하고, `go` 신호가 올 때까지 응답을 보류한다.
// The signal releases a late reply after a front-session handoff.
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
        // Hold the reply until the front-session handoff completes.
        let _ = go.recv();
        // A의 요청에 대한 뒤늦은 응답 — 펜싱되어 신규 소유자 B에게 배달되면 안 된다.
        writeln!(w, r#"{{"id":{id},"ok":true,"result":{{"stale":true}}}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    })
}

#[test]
fn broker_fences_a_late_response_after_closed_session_reattach() {
    // 세션 A가 닫힌 뒤 명시적으로 붙은 B는 A의 늦은 응답을 자기 응답으로 받지 않아야 한다.
    let (ea, sa) = start_broker();
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

    drop(a_sock);
    std::thread::sleep(Duration::from_millis(100));
    let (b_sock, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "A가 닫힌 뒤 B가 명시적으로 attach: {ar_b}"
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
