//! 에뮬레이터 연결 broker — 레지스트리·페어링·양방향 펌프. 페어링 후 줄을 무해석 전달한다.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::protocol::{to_line, Request};

/// 페어링된 세션이 stale(hang)인지 — 마지막 활동 이후 threshold를 넘겼는지.
/// 시계 역행에도 안전하도록 saturating으로 잰다.
pub fn is_stale(now: Instant, last_seen: Instant, threshold: Duration) -> bool {
    now.saturating_duration_since(last_seen) > threshold
}

struct Emu {
    to_emu: TcpStream, // 에뮬레이터로 쓰는 writer
    methods: Vec<String>,
    identity: serde_json::Value,
    session: Option<TcpStream>, // 페어링된 세션 writer(없으면 드레인)
    gen: u64,                   // 등록 세대(단조 증가). 정리 시 clobber 방지에 씀.
    session_gen: u64,           // 현재 페어링된 세션의 세대(0=없음). 세션 정리 clobber 방지.
    last_seen: Instant,         // 페어링 세션의 마지막 활동(명령·heartbeat). steal 판정용.
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

#[derive(Default)]
struct Registry {
    emus: HashMap<String, Emu>,
    anon: u64,
    next_gen: u64, // 등록·attach마다 증가하는 세대 카운터.
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

/// 두 리스너를 구동한다(블로킹). 세션 accept는 별도 스레드, 에뮬레이터 accept는 이 스레드.
/// `stale_threshold`: 페어링 세션이 이만큼 활동(명령·heartbeat)이 없으면 hang으로 보고
/// 신규 attach가 그 에뮬레이터를 steal할 수 있다.
pub fn serve(emu_listener: TcpListener, session_listener: TcpListener, stale_threshold: Duration) {
    let reg: Shared = Arc::new(Mutex::new(Registry::default()));
    let reg_s = reg.clone();
    std::thread::spawn(move || {
        for s in session_listener.incoming().flatten() {
            let reg = reg_s.clone();
            std::thread::spawn(move || handle_session(s, reg, stale_threshold));
        }
    });
    for s in emu_listener.incoming().flatten() {
        let reg = reg.clone();
        std::thread::spawn(move || handle_emulator(s, reg));
    }
}

fn handle_emulator(stream: TcpStream, reg: Shared) {
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
    let mut hello = String::new();
    if reader.read_line(&mut hello).unwrap_or(0) == 0 {
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(hello.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let result = v.get("result").cloned().unwrap_or(serde_json::Value::Null);
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
            let _ = old.to_emu.shutdown(std::net::Shutdown::Both);
            old.session
        });
        g.next_gen += 1;
        let gen = g.next_gen;
        g.emus.insert(
            nm.clone(),
            Emu {
                to_emu: emu_writer,
                methods: methods.clone(),
                identity: result.clone(),
                session: None,
                gen,
                session_gen: 0,
                last_seen: Instant::now(),
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
    // writer는 lock 안에서 try_clone만 — 쓰기는 lock 밖.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        let sess = {
            let g = lock(&reg);
            g.emus
                .get(&name)
                .and_then(|e| e.session.as_ref())
                .and_then(|s| s.try_clone().ok())
        };
        if let Some(mut s) = sess {
            let _ = write_line(&mut s, line.trim_end());
        }
        // 세션 없으면 드레인(폐기)
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

fn handle_session(stream: TcpStream, reg: Shared, stale_threshold: Duration) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut to_sess = stream;
    let mut attach = String::new();
    if reader.read_line(&mut attach).unwrap_or(0) == 0 {
        return;
    }
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
    // session writer clone은 lock 밖에서 — FD 고갈 시 패닉(→mutex poison) 대신 조용히 종료.
    let sess_writer = match to_sess.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    // 이 세션의 페어링 세대. 종료 정리 시 '내가 설정한 페어링일 때만' unpair하는 데 쓴다.
    let mut my_session_gen = 0u64;
    // 대상 선택 + 페어링 — lock 안에서 try_clone만, 쓰기는 lock 밖.
    let chosen: Result<(String, Vec<String>, serde_json::Value), String> = {
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
                let e = g.emus.get_mut(&nm).unwrap();
                // 페어링됐고 아직 살아있으면(최근 활동) Busy. stale(hang)이면 steal한다.
                // 정상 종료 세션은 리더 EOF에서 이미 None이 되고, hang 세션만 여기서 회수된다.
                if e.session.is_some() && !is_stale(Instant::now(), e.last_seen, stale_threshold) {
                    Err(r#"{"kind":"busy"}"#.to_string())
                } else {
                    // 기존(stale) 세션 소켓을 종료해 구 리더를 EOF로 깨워 정리한다.
                    if let Some(old) = e.session.take() {
                        let _ = old.shutdown(std::net::Shutdown::Both);
                    }
                    let methods = e.methods.clone();
                    let identity = e.identity.clone();
                    e.session = Some(sess_writer);
                    e.session_gen = sg;
                    e.last_seen = Instant::now();
                    my_session_gen = sg;
                    Ok((nm, methods, identity))
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
    let (name, methods, identity) = chosen;
    let mut result = serde_json::Map::new();
    result.insert(
        "attached_name".into(),
        serde_json::Value::String(name.clone()),
    );
    result.insert("methods".into(), serde_json::json!(methods));
    if let Some(obj) = identity.as_object() {
        for key in ["system", "adapter", "name", "session_token", "content"] {
            if let Some(v) = obj.get(key) {
                result.insert(key.into(), v.clone());
            }
        }
    }
    let resp = serde_json::json!({"id": req_id, "ok": true, "result": result});
    if write_line(&mut to_sess, &resp.to_string()).is_err() {
        // 세션 끊김: 내가 설정한 페어링일 때만 언페어.
        let mut g = lock(&reg);
        if let Some(e) = g.emus.get_mut(&name) {
            if e.session_gen == my_session_gen {
                e.session = None;
            }
        }
        return;
    }
    // 세션-리더: 줄을 읽어 페어링 에뮬레이터로 전달.
    // writer는 lock 안에서 try_clone만 — 쓰기는 lock 밖.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break; // 세션 EOF
        }
        let trimmed = line.trim_end();
        let ping = is_ping_line(trimmed);
        let emu = {
            let mut g = lock(&reg);
            // 받은 모든 줄(명령·heartbeat)은 활동 신호 — 내가 설정한 페어링일 때만 last_seen 갱신.
            if let Some(e) = g.emus.get_mut(&name) {
                if e.session_gen == my_session_gen {
                    e.last_seen = Instant::now();
                }
            }
            if ping {
                None // _ping은 에뮬레이터로 전달하지 않고 드레인(heartbeat 전용)
            } else {
                g.emus.get(&name).and_then(|e| e.to_emu.try_clone().ok())
            }
        };
        if ping {
            continue;
        }
        match emu {
            Some(mut e) => {
                let _ = write_line(&mut e, trimmed);
            }
            None => break, // 에뮬레이터 사라짐
        }
    }
    // 세션 끊김: 내가 설정한 페어링일 때만 언페어(에뮬레이터는 유지 = 지속성).
    // 그 사이 에뮬레이터 replace로 다른 세션이 페어링됐다면(session_gen 불일치) 건드리지 않는다.
    let mut g = lock(&reg);
    if let Some(e) = g.emus.get_mut(&name) {
        if e.session_gen == my_session_gen {
            e.session = None;
        }
    }
}
