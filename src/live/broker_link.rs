//! BrokerLink — broker 세션 포트에 접속해 attach 후 명령을 위임하는 EmulatorLink.
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::link::{Capabilities, EmulatorIdentity, EmulatorLink, LinkError};
use super::protocol::{
    parse_response, read_ndjson_frame, result_status, to_line, Request, PROTOCOL_VERSION,
    STATUS_WORKING,
};

/// 세션 liveness heartbeat 주기. broker가 hang 세션을 stale로 판정하는 임계(기본 15초)보다
/// 충분히 짧아야 한다(여기선 3회 여유).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// 연속 read 타임아웃이 이 횟수면 broker가 행된 것으로 보고 NotConnected를 올린다 — LazyBrokerLink가
/// inner를 버리고 재connect+attach하게 해 자가복구시킨다(TcpLink의 drop+재accept에 대응).
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

/// deferred(working keepalive) 명령의 총 벽시계 상한. working은 성공 read라 consecutive_timeouts를 매번
/// 리셋해 3-timeout 가드로는 못 끊는다 — 이 상한 초과면 NotConnected로 poison해 LazyBrokerLink가 재attach
/// 하게 한다(TcpLink의 deferred_deadline 동형).
const DEFAULT_DEFERRED_DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub struct BrokerLink {
    reader: BufReader<TcpStream>,
    // writer는 raw_call과 heartbeat 스레드가 공유하므로 Mutex로 보호한다(한 줄 write 단위 락).
    writer: Arc<Mutex<TcpStream>>,
    caps: Capabilities,
    next_id: u64,
    hb_stop: Arc<AtomicBool>,
    hb_handle: Option<JoinHandle<()>>,
    /// 부분 수신한 응답 frame. read timeout 뒤에도 이어 읽되 protocol payload cap을 넘기지 않는다.
    pending: Vec<u8>,
    /// 연속 read 타임아웃 횟수. Ok read 하나로 0 리셋, 임계치면 hung broker로 보고 NotConnected.
    consecutive_timeouts: u32,
    /// deferred 명령의 총 벽시계 상한(working keepalive가 끝없이 와도 유한하게 끊기 위함).
    deferred_deadline: Duration,
}

impl Drop for BrokerLink {
    fn drop(&mut self) {
        self.hb_stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.hb_handle.take() {
            let _ = h.join();
        }
    }
}

/// 세션 포트로 접속해 attach{name?}한다. 실패는 명시 LinkError.
pub fn connect(
    session_addr: &str,
    name: Option<String>,
    timeout: Duration,
) -> Result<BrokerLink, LinkError> {
    let stream = TcpStream::connect(session_addr).map_err(|_| LinkError::NotConnected)?;
    stream.set_read_timeout(Some(timeout)).map_err(io_e)?;
    // 쓰기 타임아웃도 건다. 없으면 broker가 recv를 안 비우는(백프레셔) 대량 요청에서 write_all이 영원히
    // 블록해 링크 뮤텍스를 쥔 채 MCP를 wedge한다. 쓰기 실패는 poison → NotConnected로 처리한다.
    stream.set_write_timeout(Some(timeout)).map_err(io_e)?;
    let reader = BufReader::new(stream.try_clone().map_err(io_e)?);
    let mut link = BrokerLink {
        reader,
        writer: Arc::new(Mutex::new(stream)),
        caps: Capabilities {
            protocol_version: PROTOCOL_VERSION,
            methods: vec![],
            memory_types: vec![],
            breakpoint_kinds: vec![],
            contracts: crate::contracts::ContractAdvertisement::Unreported,
            identity: EmulatorIdentity::default(),
        },
        next_id: 1,
        hb_stop: Arc::new(AtomicBool::new(false)),
        hb_handle: None,
        pending: Vec::new(),
        consecutive_timeouts: 0,
        deferred_deadline: DEFAULT_DEFERRED_DEADLINE,
    };
    let params = match name {
        Some(n) => json!({ "name": n }),
        None => json!({}),
    };
    let res = link.raw_call("attach", params)?;
    let methods = res
        .get("methods")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let memory_types = res
        .get("memory_types")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let breakpoint_kinds = res
        .get("breakpoint_kinds")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter(|value| value.is_object())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    link.caps = Capabilities {
        protocol_version: PROTOCOL_VERSION,
        methods,
        memory_types,
        breakpoint_kinds,
        contracts: crate::contracts::advertisement_from_hello(&res),
        identity: EmulatorIdentity::from_hello(&res),
    };
    link.start_heartbeat();
    Ok(link)
}

fn io_e(e: std::io::Error) -> LinkError {
    LinkError::Protocol(format!("io: {e}"))
}

impl BrokerLink {
    /// 테스트용 — deferred 데드라인을 짧게 설정한다(working-flood 컷오프 검증).
    #[cfg(test)]
    pub(crate) fn set_deferred_deadline(&mut self, d: Duration) {
        self.deferred_deadline = d;
    }

    /// write-only heartbeat 스레드를 시작한다 — broker가 idle 세션을 hang으로 오판해 steal하지
    /// 않도록 주기적으로 `_ping`을 보낸다(응답 불필요). stop 플래그로 drop 시 종료한다.
    fn start_heartbeat(&mut self) {
        let writer = self.writer.clone();
        let stop = self.hb_stop.clone();
        let ping = to_line(&Request::new(0, "_ping", json!({})));
        self.hb_handle = Some(std::thread::spawn(move || {
            loop {
                // 주기를 100ms로 쪼개 stop을 빠르게 감지(drop 지연 최소화).
                let ticks = HEARTBEAT_INTERVAL.as_millis() / 100;
                for _ in 0..ticks {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
                if w.write_all(ping.as_bytes()).is_err() {
                    return; // 연결 끊김 — 스레드 종료
                }
            }
        }));
    }

    fn raw_call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        // id 불일치 프레임을 무제한 버리면, 악성·버그 피어가 매칭 안 되는 프레임을 스트림하는 것만으로
        // raw_call을 영구 wedge시킨다(이 호출은 outer SharedLink mutex를 쥐고 있어 MCP 전체가 정지).
        // TcpLink(MAX_ID_MISMATCH)와 동일하게 상한을 둔다.
        const MAX_ID_MISMATCH: u32 = 256;
        let id = self.next_id;
        self.next_id += 1;
        let req = Request::new(id, method, params);
        {
            let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
            w.write_all(to_line(&req).as_bytes())
                .map_err(|_| LinkError::NotConnected)?;
        }
        let mut mismatches = 0u32;
        // deferred(working) 응답이 끝없이 와도 매 성공 read가 consecutive_timeouts를 리셋해 3-timeout
        // 가드가 못 끊는다 — 총 벽시계 데드라인으로 유한하게 끊는다. 초과면 NotConnected로 poison해
        // LazyBrokerLink가 inner를 버리고 재attach하게 한다(SharedLink mutex 무한 wedge 방지).
        let deadline = Instant::now() + self.deferred_deadline;
        loop {
            if Instant::now() > deadline {
                return Err(LinkError::NotConnected);
            }
            // 영속 버퍼로 읽어 timeout 경계의 부분 frame을 보존한다. protocol cap 초과나 불완전 EOF는
            // 이 BrokerLink를 폐기할 수 있는 연결 오류로 반환한다.
            match read_ndjson_frame(&mut self.reader, &mut self.pending) {
                Ok(None) => return Err(LinkError::NotConnected),
                Ok(Some(line)) => {
                    self.consecutive_timeouts = 0; // 응답 수신 = broker 살아있음 → 카운터 리셋
                    let resp = parse_response(line.trim())
                        .map_err(|e| LinkError::Protocol(e.to_string()))?;
                    if resp.id != id {
                        // id 불일치 — 버린다(상한 내에서). 초과하면 스트림 desync로 보고 끊는다.
                        mismatches += 1;
                        if mismatches > MAX_ID_MISMATCH {
                            return Err(LinkError::Protocol(format!(
                                "broker id 불일치 {MAX_ID_MISMATCH}회 초과 — 스트림 desync"
                            )));
                        }
                        continue;
                    }
                    if !resp.ok {
                        return match resp.error {
                            Some(e) => Err(map_broker_error(line.trim(), &e.kind, e.message)),
                            None => Err(LinkError::Protocol("ok=false인데 error 없음".into())),
                        };
                    }
                    let result = resp.result.unwrap_or(Value::Null);
                    if result_status(&result) == STATUS_WORKING {
                        // keepalive — 다음 줄을 더 읽는다
                        continue;
                    }
                    return Ok(result);
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // 단발 타임아웃은 비치명(느린 op일 수 있음). 부분 수신 줄은 pending에 보존된다.
                    // 연속 임계치면 hung broker로 보고 NotConnected를 올려 LazyBrokerLink가 재attach하게
                    // 한다 — 안 그러면 행된 broker에 영구 Timeout으로 wedge된다(M3 self-heal).
                    self.consecutive_timeouts += 1;
                    if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                        self.consecutive_timeouts = 0;
                        return Err(LinkError::NotConnected);
                    }
                    return Err(LinkError::Timeout);
                }
                Err(_) => return Err(LinkError::NotConnected),
            }
        }
    }
}

/// broker 에러 kind + 원본 줄에서 LinkError로 변환.
/// busy/not_connected는 명시 변형. no_such_emulator/ambiguous는 원본 줄에서 names를
/// 파싱해 살린다(ProtocolError.message엔 names가 없으므로 raw 줄을 재파싱).
fn map_broker_error(raw_line: &str, kind: &str, message: String) -> LinkError {
    match kind {
        "busy" => LinkError::Busy,
        "not_connected" => LinkError::NotConnected,
        "no_such_emulator" => {
            let names = extract_names(raw_line);
            LinkError::NoSuchEmulator { names }
        }
        "ambiguous" => {
            let names = extract_names(raw_line);
            LinkError::Ambiguous { names }
        }
        _ => LinkError::Emulator {
            kind: kind.to_string(),
            message,
        },
    }
}

/// 에러 응답 JSON 원본에서 `error.names` 배열을 꺼낸다. 없으면 빈 Vec.
fn extract_names(raw_line: &str) -> Vec<String> {
    let v: Value = match serde_json::from_str(raw_line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.get("error")
        .and_then(|e| e.get("names"))
        .and_then(|n| n.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

impl EmulatorLink for BrokerLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.raw_call(method, params)
    }
}

/// 지연 BrokerLink — 첫 call 시에 connect+attach를 시도한다. 실패 시 직접 모드로 폴백하지
/// 않고 LinkError를 반환한다. broker opt-in 후 다른 에뮬레이터로 조용히 연결되는 사태를 막는다.
pub struct LazyBrokerLink {
    addr: String,
    name: Option<String>,
    timeout: Duration,
    inner: Option<BrokerLink>,
}

/// tcp::lazy에 대응하는 broker 지연 접속 팩토리. EMUCAP_BROKER 모드에서 SharedLink로 감싸
/// 폴백 없는 broker-only link를 만든다. 실제 접속·attach는 첫 call() 호출로 미뤄진다.
pub fn lazy(session_addr: &str, name: Option<String>, timeout: Duration) -> LazyBrokerLink {
    LazyBrokerLink {
        addr: session_addr.to_string(),
        name,
        timeout,
        inner: None,
    }
}

impl LazyBrokerLink {
    fn ensure_connected(&mut self) -> Result<&mut BrokerLink, LinkError> {
        if self.inner.is_none() {
            self.inner = Some(connect(&self.addr, self.name.clone(), self.timeout)?);
        }
        Ok(self.inner.as_mut().unwrap())
    }
}

impl EmulatorLink for LazyBrokerLink {
    fn capabilities(&self) -> &Capabilities {
        static EMPTY: std::sync::OnceLock<Capabilities> = std::sync::OnceLock::new();
        self.inner
            .as_ref()
            .map(|l| l.capabilities())
            .unwrap_or_else(|| {
                EMPTY.get_or_init(|| Capabilities {
                    protocol_version: PROTOCOL_VERSION,
                    methods: vec![],
                    memory_types: vec![],
                    breakpoint_kinds: vec![],
                    contracts: crate::contracts::ContractAdvertisement::Unreported,
                    identity: EmulatorIdentity::default(),
                })
            })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        let result = self.ensure_connected()?.raw_call(method, params);
        // 연결이 죽었거나 protocol desync가 확인되면 inner를 비워 다음 call이 재attach하게 한다.
        // 그러지 않으면 stale BrokerLink로 영구 실패해 /mcp 재시작이 필요하다(TcpLink는 drop+재accept로
        // 자가복구). Timeout은 일시적(느린 op)일 수 있어 같은 연결을 유지한다.
        if matches!(
            result,
            Err(LinkError::NotConnected | LinkError::Protocol(_))
        ) {
            self.inner = None;
        }
        result
    }

    fn supports_session_reconnect(&self) -> bool {
        true
    }

    fn prepare_reconnect(&mut self) {
        self.inner = None;
    }
}
