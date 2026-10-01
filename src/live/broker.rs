//! 에뮬레이터 연결 broker — 레지스트리·페어링·양방향 펌프. 페어링 후 줄을 무해석 전달한다.
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::control_session::{self, Attachment, EventKind, SessionEvent};
use super::protocol::{read_ndjson_frame, to_line, Request};

mod outbound;

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

struct Emu {
    to_emu: outbound::Outbound, // registration-scoped serialized producer writer
    lifecycle_runtime: Option<String>,
    methods: Vec<String>,
    identity: serde_json::Value,
    session: Option<TcpStream>, // 페어링된 세션 writer(없으면 드레인)
    gen: u64,                   // 등록 세대(단조 증가). 정리 시 clobber 방지에 씀.
    session_gen: u64,           // 현재 페어링된 세션의 세대(0=없음). 세션 정리 clobber 방지.
}

/// 세션→broker 제어 메시지 `_ping`(heartbeat)인지. 에뮬레이터로 전달하지 않고 드레인한다.
fn is_ping_line(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| {
            v.get("method")
                .and_then(|m| m.as_str())
                .map(|s| s == "_ping")
        })
        .unwrap_or(false)
}

// ── 세션-세대 응답 펜싱 ────────────────────────────────────
// 앞단 세션이 끊긴 뒤 그 세션의 in-flight 응답이 늦게 도착할 수 있다. 세션마다 요청 id 공간이
// 겹치므로(둘 다 1부터), 다음 명시적 attach가 그 응답을 자기 것으로 오인하지 않게 요청 id에
// session_gen을 넣고 현재 세션과 다른 응답을 버린다.
//
// id는 Lua 어댑터(double, 53비트 가수)도 손실 없이 echo하도록 2^53 아래로 유지한다: 하위 32비트=원본
// id, 다음 20비트=세대 필드. 원본 id는 세션 수명 내 2^32을 넘지 않고, 세대는 20비트로 접어도 steal 시
// 옛/신 세션 세대가 소수 차이라 앨리어싱하지 않는다.
const FENCE_ID_BITS: u64 = 32;
const FENCE_GEN_MASK: u64 = 0xF_FFFF; // 20비트 필드 마스크
const FENCE_ORIG_MASK: u64 = 0xFFFF_FFFF; // 32비트

/// 세션 세대를 20비트 세대 *필드*로 사상한다. 필드값 0은 "네임스페이스 안 됨" 센티널로 예약하므로,
/// 실제 세대는 절대 0으로 접히지 않도록 [1, 2^20-1]에 1-based로 사상한다 — 세대가 2^20의 배수일 때
/// 옛 단순 마스킹이 필드 0을 만들어 그 세션의 펜싱이 조용히 무력화되던 것을 막는다.
fn fence_gen_field(gen: u64) -> u64 {
    (gen % FENCE_GEN_MASK) + 1 // ∈ [1, 2^20-1] — 절대 0 아님
}

fn fence_pack(gen: u64, orig: u64) -> u64 {
    (fence_gen_field(gen) << FENCE_ID_BITS) | (orig & FENCE_ORIG_MASK)
}
fn fence_gen(packed: u64) -> u64 {
    (packed >> FENCE_ID_BITS) & FENCE_GEN_MASK
}
fn fence_orig(packed: u64) -> u64 {
    packed & FENCE_ORIG_MASK
}

/// 세션→에뮬레이터로 나가는 요청 줄의 `id`를 이 세션 세대로 네임스페이스한다. numeric `id`가 있으면
/// `fence_pack(gen, id)`로 치환해 재직렬화, 없거나(heartbeat 등) 파싱 불가면 원본 그대로.
fn fence_outgoing(line: &str, gen: u64) -> String {
    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("id".into(), serde_json::json!(fence_pack(gen, id)));
                return v.to_string();
            }
        }
    }
    line.to_string()
}

/// 에뮬레이터→세션으로 가는 응답 줄을 현재 세션 세대 `cur_gen`으로 검사한다.
/// - 박힌 세대 필드가 현재 세션과 같으면 원본 id로 복원한 줄을 반환(Some).
/// - 박힌 세대 필드가 있고 다르면(옛/steal된 세션 응답) None → 드롭(fence).
/// - 세대 필드가 0이면(비-JSON·id 없음·우리가 네임스페이스하지 않은 unsolicited) 원본 그대로 전달(Some).
///   우리 네임스페이싱은 세대 필드를 절대 0으로 만들지 않으므로(1-based) 이 경로는 진짜 미네임스페이스만 탄다.
fn fence_incoming(line: &str, cur_gen: u64) -> Option<String> {
    let mut v = match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => v,
        Err(_) => return Some(line.to_string()), // 비-JSON은 무해석 전달
    };
    let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
        return Some(line.to_string()); // id 없음 → 그대로 전달
    };
    let gen = fence_gen(id);
    if gen == 0 {
        return Some(line.to_string()); // 네임스페이스 안 된 id → 그대로 전달
    }
    if gen != fence_gen_field(cur_gen) {
        return None; // 옛/steal된 세션의 응답 → 드롭
    }
    if let Some(obj) = v.as_object_mut() {
        obj.insert("id".into(), serde_json::json!(fence_orig(id)));
    }
    Some(v.to_string())
}

struct Registry {
    instance: String,
    emus: HashMap<String, Emu>,
    anon: u64,
    next_gen: u64, // 등록·attach마다 증가하는 세대 카운터.
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            instance: super::temporal::fresh_identity(),
            emus: HashMap::new(),
            anon: 0,
            next_gen: 0,
        }
    }
}

type Shared = Arc<Mutex<Registry>>;

// poison 내성 lock: 한 핸들러 스레드가 가드 보유 중 패닉해도 broker 전체가 연쇄
// 사망하지 않도록 poison을 무시하고 내부 데이터를 복구한다(emucap-mcp와 동일 정책).
fn lock(reg: &Shared) -> MutexGuard<'_, Registry> {
    reg.lock().unwrap_or_else(|e| e.into_inner())
}

fn write_line(s: &mut TcpStream, line: &str) -> std::io::Result<()> {
    s.write_all(line.as_bytes())?;
    if !line.ends_with('\n') {
        s.write_all(b"\n")?;
    }
    Ok(())
}

/// Run both listeners. Session accept runs on a dedicated thread; emulator accept runs here.
pub fn serve(emu_listener: TcpListener, session_listener: TcpListener) {
    let reg: Shared = Arc::new(Mutex::new(Registry::default()));
    let reg_s = reg.clone();
    std::thread::spawn(move || {
        for s in session_listener.incoming().flatten() {
            let reg = reg_s.clone();
            std::thread::spawn(move || handle_session(s, reg));
        }
    });
    for s in emu_listener.incoming().flatten() {
        let reg = reg.clone();
        std::thread::spawn(move || handle_emulator(s, reg));
    }
}

fn handle_emulator(stream: TcpStream, reg: Shared) {
    if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        return;
    }
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut to_emu = stream;
    // broker가 서버로서 hello를 보낸다(직접 emucap-mcp와 동일).
    if write_line(
        &mut to_emu,
        &to_line(&Request::new(0, "hello", serde_json::json!({}))),
    )
    .is_err()
    {
        return;
    }
    let mut pending = Vec::new();
    let hello = match read_ndjson_frame(&mut reader, &mut pending) {
        Ok(Some(frame)) => frame,
        Ok(None) | Err(_) => return,
    };
    let v: serde_json::Value = match serde_json::from_str(hello.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let result = v.get("result").cloned().unwrap_or(serde_json::Value::Null);
    let lifecycle_runtime = match control_session::advertised_runtime(&result) {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let identity = super::link::EmulatorIdentity::from_hello(&result);
    if identity.adapter.as_deref() == Some("mesen2-live") && !identity.has_mesen_native_halt() {
        return;
    }
    let methods: Vec<String> = result
        .get("methods")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // writer clone은 lock 밖에서 — FD 고갈(EMFILE 등) 시 패닉(→mutex poison) 대신
    // 조용히 연결을 종료한다.
    let emu_writer = match to_emu.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let emu_writer = match outbound::Outbound::new(emu_writer) {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let (name, my_gen, old_session) = {
        let mut g = lock(&reg);
        let nm = result
            .get("name")
            .and_then(|n| n.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                g.anon += 1;
                format!("emu{}", g.anon)
            });
        // 같은 name 재등록: 구 Emu를 꺼내되 session만 밖으로 move한다.
        // 구 to_emu 소켓을 shutdown해 구 리더(handle_emulator 스레드)를 즉시 EOF로 깨운다.
        // shutdown은 블록하지 않으므로 lock 안에서 OK.
        // lock 밖에서 알림을 쓴다(lock 쥔 채 소켓 write 금지).
        let old_session = g.emus.remove(&nm).and_then(|old| {
            old.to_emu.close();
            old.session
        });
        g.next_gen += 1;
        let gen = g.next_gen;
        g.emus.insert(
            nm.clone(),
            Emu {
                to_emu: emu_writer,
                lifecycle_runtime,
                methods: methods.clone(),
                identity: result.clone(),
                session: None,
                gen,
                session_gen: 0,
            },
        );
        (nm, gen, old_session)
    };
    // lock 해제 후: 구 세션이 있으면 알림 송신 + 소켓 종료.
    // 종료까지 해야 구 세션 리더가 EOF로 깨어나 종료한다 — 안 그러면 그 리더가 신규
    // 에뮬레이터로 명령을 계속 주입하고, 뒤늦게 종료하며 신규 세션 페어링을 unpair한다.
    if let Some(mut old_s) = old_session {
        let _ = write_line(
            &mut old_s,
            r#"{"id":0,"ok":false,"error":{"kind":"not_connected","message":"emulator replaced"}}"#,
        );
        let _ = old_s.shutdown(std::net::Shutdown::Both);
    }
    // 에뮬레이터-리더: 줄을 읽어 페어링 세션으로(없으면 드레인).
    // writer는 lock 안에서 try_clone만 — 쓰기는 lock 밖. 현재 세션 세대로 응답을 펜싱해, steal 이전
    // 옛 세션의 in-flight 응답이 신규 소유자에게 오배달되는 것을 막는다(fence_incoming).
    while let Ok(Some(line)) = read_ndjson_frame(&mut reader, &mut pending) {
        let raw = line.trim_end();
        // lock 안에서는 값싼 스냅샷(writer clone + session_gen 복사)만 잡고, 값비싼 JSON 파싱/재직렬화
        // (fence_incoming)는 lock 밖에서 한다 — 안 그러면 emu→session 줄마다 두 번의 full JSON 패스가
        // 전역 레지스트리 mutex를 쥔 채 실행돼 broker 트래픽이 그 뒤로 직렬화된다. session_gen은 여기서
        // 복사한 값으로 펜싱하므로(스냅샷 시점 세대) 동시 steal에도 의미가 바뀌지 않는다(옛 writer로의
        // 쓰기는 steal 시 소켓이 닫혀 무해).
        let target = response_target(&reg, &name, my_gen);
        // 페어링 세션 없음, 또는 fence_incoming이 None(옛/steal된 세션 응답)이면 폐기.
        if let Some((mut s, sess_gen)) = target {
            if let Some(out) = fence_incoming(raw, sess_gen) {
                if write_line(&mut s, &out).is_err() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
    // 에뮬레이터 끊김: gen 가드로 내가 등록한 엔트리일 때만 제거 + 페어링 세션에 알림.
    // 같은 name 재등록(신규 gen)이 먼저 이뤄진 경우 remove를 건너뛰어 신규 등록을 clobber하지 않는다.
    let detached_session = {
        let mut g = lock(&reg);
        if g.emus.get(&name).map(|e| e.gen) == Some(my_gen) {
            g.emus.remove(&name).and_then(|e| e.session)
        } else {
            None
        }
    };
    if let Some(mut s) = detached_session {
        let _ = write_line(
            &mut s,
            r#"{"id":0,"ok":false,"error":{"kind":"not_connected","message":"emulator gone"}}"#,
        );
    }
}

fn handle_session(stream: TcpStream, reg: Shared) {
    if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        return;
    }
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut to_sess = stream;
    let mut pending = Vec::new();
    let attach = match read_ndjson_frame(&mut reader, &mut pending) {
        Ok(Some(frame)) => frame,
        Ok(None) | Err(_) => return,
    };
    let av: serde_json::Value = match serde_json::from_str(attach.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let req_id = av.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
    let want = av
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from);
    let expected_registration_id = av
        .get("params")
        .and_then(|params| params.get("expected_registration_id"))
        .and_then(|registration| registration.as_u64());
    let expected_launch_id = av
        .get("params")
        .and_then(|params| params.get("expected_launch_id"))
        .and_then(|launch_id| launch_id.as_str());
    // session writer clone은 lock 밖에서 — FD 고갈 시 패닉(→mutex poison) 대신 조용히 종료.
    let sess_writer = match to_sess.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    // 이 세션의 페어링 세대. 종료 정리 시 '내가 설정한 페어링일 때만' unpair하는 데 쓴다.
    let mut my_session_gen = 0u64;
    // 대상 선택 + 페어링 — lock 안에서 try_clone만, 쓰기는 lock 밖.
    let chosen: Result<(String, Vec<String>, serde_json::Value, u64), String> = {
        let mut g = lock(&reg);
        let names: Vec<String> = g.emus.keys().cloned().collect();
        let pick = match &want {
            Some(n) if g.emus.contains_key(n) => Ok(n.clone()),
            Some(_) => Err(format!(
                r#"{{"kind":"no_such_emulator","names":{}}}"#,
                serde_json::to_string(&names).unwrap()
            )),
            None if names.len() == 1 => Ok(names[0].clone()),
            None if names.is_empty() => Err(r#"{"kind":"not_connected"}"#.to_string()),
            None => Err(format!(
                r#"{{"kind":"ambiguous","names":{}}}"#,
                serde_json::to_string(&names).unwrap()
            )),
        };
        match pick {
            Ok(nm) => {
                g.next_gen += 1;
                let sg = g.next_gen;
                let instance = g.instance.clone();
                let e = g.emus.get_mut(&nm).unwrap();
                let same_registration =
                    expected_registration_id.is_none_or(|expected| expected == e.gen);
                let same_launch = expected_launch_id.is_some_and(|expected| {
                    e.identity
                        .get("launch_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(expected)
                });
                if expected_launch_id.is_some() && !same_launch {
                    Err(r#"{"kind":"identity_mismatch","message":"managed launch identity changed"}"#.to_string())
                } else if !same_registration && !same_launch {
                    Err(r#"{"kind":"identity_mismatch","message":"broker emulator registration changed"}"#.to_string())
                } else if e.session.is_some()
                    && (expected_registration_id.is_none() || !same_registration)
                {
                    Err(r#"{"kind":"busy"}"#.to_string())
                } else {
                    match pair_session(e, &instance, sess_writer, sg) {
                        Ok(()) => {
                            my_session_gen = sg;
                            Ok((nm, e.methods.clone(), e.identity.clone(), e.gen))
                        }
                        Err(_) => Err(r#"{"kind":"not_connected","message":"producer lifecycle delivery failed"}"#.to_string()),
                    }
                }
            }
            Err(x) => Err(x),
        }
    };
    let chosen = match chosen {
        Ok(c) => c,
        Err(err) => {
            let _ = write_line(
                &mut to_sess,
                &format!(r#"{{"id":{req_id},"ok":false,"error":{err}}}"#),
            );
            return;
        }
    };
    let (name, methods, identity, registration) = chosen;
    // Producer metadata is opaque to the broker. Preserve extensions and override only
    // broker-owned routing fields after copying the complete hello result.
    let mut result = identity.as_object().cloned().unwrap_or_default();
    result.insert(
        "attached_name".into(),
        serde_json::Value::String(name.clone()),
    );
    result.insert("methods".into(), serde_json::json!(methods));
    result.insert(
        "broker_registration_id".into(),
        serde_json::json!(registration),
    );
    let resp = serde_json::json!({"id": req_id, "ok": true, "result": result});
    if write_line(&mut to_sess, &resp.to_string()).is_err() {
        // 세션 끊김: 내가 설정한 페어링일 때만 언페어.
        detach_session(&reg, &name, registration, my_session_gen);
        return;
    }
    // Admit under the registry lock; the registration's sole writer performs I/O outside it.
    while let Ok(Some(line)) = read_ndjson_frame(&mut reader, &mut pending) {
        let trimmed = line.trim_end();
        if is_ping_line(trimmed) {
            continue;
        }
        let out = fence_outgoing(trimmed, my_session_gen);
        if !matches!(
            enqueue_request(&reg, &name, registration, my_session_gen, out),
            Ok(true)
        ) {
            break;
        }
    }
    // 세션 끊김: 내가 설정한 페어링일 때만 언페어(에뮬레이터는 유지 = 지속성).
    // 그 사이 에뮬레이터 replace로 다른 세션이 페어링됐다면(session_gen 불일치) 건드리지 않는다.
    detach_session(&reg, &name, registration, my_session_gen);
}

fn attachment(instance: &str, registration: u64, session: u64) -> Attachment {
    Attachment {
        broker_instance: instance.into(),
        registration,
        session,
    }
}
fn lifecycle(emu: &Emu, instance: &str, session: u64, kind: EventKind) -> std::io::Result<()> {
    if let Some(runtime) = &emu.lifecycle_runtime {
        emu.to_emu.enqueue(
            SessionEvent {
                kind,
                runtime: runtime.clone(),
                attachment: attachment(instance, emu.gen, session),
            }
            .envelope()
            .to_string(),
        )?;
    }
    Ok(())
}
fn pair_session(
    emu: &mut Emu,
    instance: &str,
    writer: TcpStream,
    session: u64,
) -> std::io::Result<()> {
    if let Some(old) = emu.session.take() {
        let _ = old.shutdown(std::net::Shutdown::Both);
        lifecycle(emu, instance, emu.session_gen, EventKind::Detach)?;
    }
    lifecycle(emu, instance, session, EventKind::Attach)?;
    emu.session = Some(writer);
    emu.session_gen = session;
    Ok(())
}
fn detach_session(reg: &Shared, name: &str, registration: u64, session: u64) {
    let mut g = lock(reg);
    let instance = g.instance.clone();
    if let Some(emu) = g.emus.get_mut(name).filter(|emu| {
        emu.gen == registration && emu.session_gen == session && emu.session.is_some()
    }) {
        emu.session = None;
        // Queue failure closes the producer transport; it cannot silently omit owner loss.
        let _ = lifecycle(emu, &instance, session, EventKind::Detach);
    }
}

/// Linearization point for frontend messages. A stale buffered line cannot acquire the new route.
fn enqueue_request(
    reg: &Shared,
    name: &str,
    registration: u64,
    session: u64,
    line: String,
) -> std::io::Result<bool> {
    // Parse and serialize outside the global registry lock; recheck admission after encoding.
    let mut parsed = serde_json::from_str::<serde_json::Value>(&line).ok();
    if parsed
        .as_ref()
        .is_some_and(control_session::has_reserved_fields)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "reserved control lifecycle fields",
        ));
    }
    let stamp = {
        let g = lock(reg);
        let Some(emu) = g.emus.get(name).filter(|emu| {
            emu.gen == registration && emu.session_gen == session && emu.session.is_some()
        }) else {
            return Ok(false);
        };
        emu.lifecycle_runtime
            .as_ref()
            .map(|_| attachment(&g.instance, registration, session))
    };
    let line = if let Some(stamp) = stamp {
        let value = parsed.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid controlled request",
            )
        })?;
        let params = value
            .get_mut("params")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "controlled request params must be an object",
                )
            })?;
        params.insert(
            control_session::ATTACHMENT_FIELD.into(),
            serde_json::to_value(stamp).unwrap(),
        );
        value.to_string()
    } else {
        line
    };
    let g = lock(reg);
    let Some(emu) = g.emus.get(name).filter(|emu| {
        emu.gen == registration && emu.session_gen == session && emu.session.is_some()
    }) else {
        return Ok(false);
    };
    emu.to_emu.enqueue(line)?;
    Ok(true)
}

fn response_target(reg: &Shared, name: &str, registration: u64) -> Option<(TcpStream, u64)> {
    let g = lock(reg);
    let emu = g.emus.get(name).filter(|emu| emu.gen == registration)?;
    emu.session
        .as_ref()?
        .try_clone()
        .ok()
        .map(|session| (session, emu.session_gen))
}

#[cfg(test)]
#[path = "broker/routing_tests.rs"]
mod routing_tests;

#[cfg(test)]
mod fence_tests {
    use super::*;

    #[test]
    fn session_whose_gen_maps_to_zero_is_still_fenced() {
        // 세대가 2^20의 배수면 옛 단순 20비트 마스킹이 세대 필드 0을 만들어, fence_incoming의 gen==0
        // 분기가 그 응답을 무조건 전달 → 그 세션(매 2^20번째)의 steal 펜싱이 조용히 꺼졌다. 1-based
        // 필드 사상으로 어떤 세대도 0으로 접히지 않아 항상 펜싱되어야 한다.
        let cur_gen = 1u64 << 20; // 2^20 → 옛 스킴에서 세대 필드 0
        assert_ne!(
            fence_gen_field(cur_gen),
            0,
            "2^20 배수 세대의 필드는 0이면 안 된다"
        );

        // 현재 세션이 보낸 요청(orig id=7)의 응답은 통과하고 orig id로 복원돼야 한다.
        let packed = fence_pack(cur_gen, 7);
        assert_ne!(fence_gen(packed), 0);
        let mine = format!(r#"{{"id":{packed},"ok":true}}"#);
        let out = fence_incoming(&mine, cur_gen).expect("현재 세션 응답은 통과");
        assert!(out.contains(r#""id":7"#), "orig id 복원: {out}");

        // steal된 옛 세션(다른 세대)의 뒤늦은 응답은 같은 orig id라도 드롭(fence)돼야 한다.
        let stale_packed = fence_pack(cur_gen + 1, 7);
        let stale = format!(r#"{{"id":{stale_packed},"ok":true,"result":{{"stale":true}}}}"#);
        assert_eq!(
            fence_incoming(&stale, cur_gen),
            None,
            "2^20 배수 세대라도 옛/steal 세션 응답은 펜싱(드롭)돼야"
        );
    }
}
