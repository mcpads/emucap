use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::link::{Capabilities, EmulatorLink, LinkError};
use super::protocol::{parse_response, to_line, Request, PROTOCOL_VERSION};

pub struct TcpLink {
    addr: String,
    listener: Option<TcpListener>,
    timeout: Duration,
    conn: Option<Conn>,
    caps: Capabilities,
    session_token: String,
    preaccept_token: Arc<RwLock<String>>,
    next_id: u64,
    preaccept: Option<Preaccept>,
    runtime_store: super::runtime::RuntimeStore,
    runtime_candidates: Vec<Value>,
    /// 연속 요청 타임아웃 횟수. 1회 타임아웃은 느리지만 살아있는 어댑터(큰 read·NMI 장면)일 수 있어
    /// 연결을 끊지 않는다. 응답을 한 번이라도 받으면 0으로 리셋. 임계치 연속이면 진짜 행으로 보고 드롭한다.
    /// 쓰기 타임아웃(플러드된 emu가 recv를 안 비움)도 읽기 타임아웃과 동일하게 여기 편입된다(비치명).
    consecutive_timeouts: u32,
    /// 한 deferred 명령(working keepalive 반복)이 점유할 수 있는 전체 벽시계 상한. working 프레임이 매번
    /// consecutive_timeouts를 리셋해 3-timeout 안전장치가 안 걸리므로, 버그·악성 어댑터가 working을 무한히
    /// 흘리면 raw_call이 이 상한만큼만 대기하고 드롭한다(SharedLink mutex 영구 wedge 방지).
    deferred_deadline: Duration,
}

/// 연속 타임아웃이 이 횟수에 도달하면 행으로 간주해 연결을 드롭한다(재수락 유도). 단발 타임아웃은
/// 무시해 slow-but-alive 어댑터를 죽이지 않는다. 읽기·쓰기 타임아웃이 함께 이 카운터에 쌓인다.
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

/// 한 deferred 명령의 전체 대기 상한(기본값). working keepalive가 매번 consecutive_timeouts를 리셋하므로
/// 개별 read timeout·3-timeout 가드로는 무한 working 플러드를 못 끊는다 — 이 상한이 총 대기를 유한하게
/// 만든다. 정상 대용량 step/run_frames(프레임 인자 상한 안)엔 넉넉하고, 무한 플러드만 끊는다.
const DEFAULT_DEFERRED_DEADLINE: Duration = Duration::from_secs(300);

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    /// 부분 수신한 응답 줄(영속). 요청 타임아웃 시 여기 남겨 두면 다음 호출이 이어 읽어 스트림
    /// desync를 막는다. 한 줄(끝 \n)이 완성되면 비운다. (read_line은 호출마다 새 String을 쓰면
    /// 타임아웃이 줄 중간에 걸릴 때 이미 읽은 바이트를 잃어 desync가 나므로 영속화한다.)
    pending: String,
}

type PreacceptResult = Result<(Conn, Capabilities, String), LinkError>;

struct Preaccept {
    rx: Receiver<PreacceptResult>,
    _handle: JoinHandle<()>,
}

fn fresh(addr: &str, listener: Option<TcpListener>, timeout: Duration) -> TcpLink {
    let session_token = new_session_token();
    TcpLink {
        addr: addr.to_string(),
        listener,
        timeout,
        conn: None,
        caps: Capabilities::empty(),
        preaccept_token: Arc::new(RwLock::new(session_token.clone())),
        session_token,
        next_id: 1,
        preaccept: None,
        runtime_store: super::runtime::RuntimeStore::discover(),
        runtime_candidates: Vec::new(),
        consecutive_timeouts: 0,
        deferred_deadline: DEFAULT_DEFERRED_DEADLINE,
    }
}

pub fn new_session_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    // 포맷 `{cwd_hash}-{session_anchor}-{pid}-{nanos}`. 세션 식별부는 앞 두 필드
    // `{cwd_hash}-{session_anchor}` — cwd는 세션마다 다르고, session_anchor는 제어 세션 id 해시라
    // 같은 세션의 서버 respawn(/mcp 재연결)엔 불변이면서 동시 실행되는 다른 세션과는 달라, 같은 cwd의
    // 형제 세션을 구별한다(reclaim 오인 방지). 부모 PID는 respawn이 다른 부모 아래로 재spawn되면 값이
    // 바뀌어, 자기 토큰을 foreign으로 오판해 실행 중 에뮬을 strand하므로 앵커로 쓰지 않는다.
    // 안정 세션 id가 없으면 앵커 필드는 0으로 적는다(값은 무의미) — 소유 판정은 저장된 앵커가 아니라
    // own_session_identity()가 Some일 때만 매칭하므로, 이 0은 형제 간 own 오판으로 이어지지 않는다.
    format!(
        "{:016x}-{:x}-{:x}-{:x}",
        cwd_hash(),
        session_anchor().unwrap_or(0),
        std::process::id(),
        nanos
    )
}

/// 세션-안정·세션-고유 제어 lease label의 앵커. 명시적인 공통 ID를 우선하고 알려진 host session ID를
/// fallback으로 쓴다. 같은 세션은 /mcp 재연결·서버 respawn에도 값이 불변이고, 동시 실행되는 다른 세션은
/// 값이 달라 형제 에뮬레이터의 조용한 인계를 막는다. 부모 PID(respawn 시 변동)는 쓰지 않는다.
/// 안정된 per-session id가 없으면(plain shell·CI·테스트) `None` — 같은 cwd 형제를
/// 구별할 수 없으므로 fail-closed로 강등한다. `None`이면 어떤 기존 토큰/포트도 own으로 재사용하지 않아
/// (session_token_is_own·reusable_session_token·port_persist_path가 모두 거부/None) 형제가 살아있는
/// 에뮬을 조용히 이어받지 못한다. 옛 동작(앵커 0으로 강등)은 두 형제가 같은 식별부로 붕괴해 서로의
/// 토큰을 own으로 오판하는 인계 회귀를 냈다 — 그래서 0-강등이 아니라 None-거부로 일반화했다.
fn session_anchor() -> Option<u64> {
    for key in [
        "EMUCAP_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
    ] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(fnv1a_64(v.as_bytes()));
            }
        }
    }
    None
}

const AUTO_PORT_RANGE: u16 = 16; // 기준 포트부터 이만큼 빈 포트를 탐색(N 세션 자동 격리)

// ── 세션당 포트 영속화 ───────────────────────────────────────
// 서버는 매 (재)시작 시 [base, base+RANGE)에서 가장 낮은 빈 포트를 잡는다. 그래서 서버가 재시작
// (/mcp 재연결·크래시 등)하면 더 낮은 포트가 비어 있을 때 그리로 옮겨가, 그 세션이 옛 포트로 띄워둔
// 에뮬레이터가 고아가 되고(freeze 화면 손실), 에이전트는 not connected를 본다. 이를 막기 위해 바인드한
// 포트를 파일에 적어두고, 다음 바인드 때 그 포트를 먼저 정확히 재바인드한다 — 서버가 죽으면 리스너가
// 닫혀 포트가 비므로 재시작 서버가 같은 포트를 되찾고, 에뮬레이터(옛 포트 고정)가 자동 재연결된다.
// 세션 구분 키는 세션 식별부(cwd+session id 앵커) — /mcp 재연결에도 유지되고 형제 세션과는 다르다.
// 안정 세션 id가 없으면 영속화 자체를 건너뛴다(port_persist_path=None) — 형제와 같은 포트 파일을
// 공유해 서로의 포트를 가로채는 창을 없앤다(fail-closed). 점유 중이면 스캔 폴백.
/// FNV-1a(64) — 프로세스·실행 무관 결정론적 해시(DefaultHasher의 시드 모호성 회피).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn cwd_hash() -> u64 {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_default();
    fnv1a_64(cwd.as_bytes())
}

/// 이 세션(식별부 `{cwd_hash}-{session_anchor}`)+기준 포트의 포트 영속화 파일 경로. 세션 식별부로 키해
/// 형제 세션과 다른 파일을 쓴다(cwd만으로 키하면 형제가 같은 파일을 공유해 포트를 가로챈다). 안정 세션
/// id가 없거나(fail-closed) base==0(임시포트, 세션 고정 무의미)이면 `None` — 영속화를 건너뛴다.
pub(crate) fn port_persist_path(base: u16) -> Option<std::path::PathBuf> {
    if base == 0 {
        return None;
    }
    let identity = own_session_identity()?;
    Some(std::env::temp_dir().join(format!("emucap-mcp-port-{identity}-{base}")))
}

fn read_persisted_port(path: &std::path::Path) -> Option<u16> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()
}

/// 잡은 포트를 best-effort로 적는다(실패해도 무시 — 영속화는 편의 기능이지 정확성 의존 아님).
pub(crate) fn write_persisted_port(path: &std::path::Path, port: u16) {
    let _ = std::fs::write(path, port.to_string());
}

pub fn session_token_path(port: u16) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::env::temp_dir().join(format!("emucap_session_token_{port}"))
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from(format!("/tmp/emucap_session_token_{port}"))
    }
}

pub(crate) fn write_session_token(port: u16, token: &str) {
    let path = session_token_path(port);
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let Ok(mut file) = options.open(&path) else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .is_err()
        {
            return;
        }
    }
    let _ = file
        .write_all(token.as_bytes())
        .and_then(|()| file.sync_all());
}

/// 토큰 `{cwd_hash}-{session_anchor}-{pid}-{nanos}`의 세션 식별부 `{cwd_hash}-{session_anchor}`
/// (앞 두 필드)를 반환한다. 형식이 아니면(구분자 부족) None.
fn identity_of(token: &str) -> Option<&str> {
    let cwd_end = token.find('-')?;
    let anchor_end = token[cwd_end + 1..].find('-')?;
    Some(&token[..cwd_end + 1 + anchor_end])
}

/// 이 세션의 식별부 `{cwd_hash}-{session_anchor}` — 안정 세션 id가 있을 때만 Some. 없으면
/// None으로 fail-closed 신호를 전파해, 소유 판정·포트 영속화가 모두 거부된다.
fn own_session_identity() -> Option<String> {
    session_anchor().map(|anchor| format!("{:016x}-{anchor:x}", cwd_hash()))
}

/// 토큰이 *이 세션* 소유인지 — cwd만이 아니라 세션 식별부(`{cwd_hash}-{session_anchor}`) 전체
/// 일치로 판정한다. 소유면 서버 respawn/재연결 시 토큰을 회전하지 않고 재사용해, 실행 중인 자기
/// 에뮬레이터를 strand하지 않는다(reclaim-own). 앵커가 session id라 부모 pid가 바뀌는 respawn에도
/// 자기 토큰은 계속 own이다. 같은 cwd라도 session id가 다른 형제 세션의 토큰은 own이 아니라, 그
/// 형제의 살아있는 에뮬레이터를 조용히 이어받지 않는다(identity-guard 충돌 방지).
pub fn session_token_is_own(existing: &str) -> bool {
    // 안정 세션 id가 없으면 own_session_identity()가 None → 어떤 토큰도 own이 아니다(fail-closed).
    // 이것이 두 형제 세션이 앵커 0으로 붕괴해 서로의 토큰을 own으로 오판(=조용한 인계)하던 회귀를 막는다.
    match own_session_identity() {
        Some(identity) => identity_of(existing) == Some(identity.as_str()),
        None => false,
    }
}

/// 두 세션 토큰이 같은 세션(식별부 `{cwd_hash}-{session_anchor}` 일치)인지 — 같은 cwd라도 session id가
/// 다르면 false. 형제 세션 간 reclaim 금지를 프로세스 밖에서(테스트 등) 검증할 때 쓴다.
pub fn same_session_identity(a: &str, b: &str) -> bool {
    match (identity_of(a), identity_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 이 포트의 기존 토큰파일이 *이 세션 소유*면 재사용 후보로 반환한다(없거나 foreign이면 None →
/// 새 토큰 유지; foreign 에뮬은 여전히 mismatch → 진입점이 graceful 처리). 토큰파일은 포트별이나,
/// 같은 cwd의 형제 세션이 잠깐 이 포트를 놓쳐 이 세션이 바인드하더라도, 식별부(cwd+session id)가 달라
/// 형제의 살아있는 토큰은 재사용하지 않는다 — 그 형제 에뮬레이터를 조용히 이어받는 것을 막는다.
pub(crate) fn reusable_session_token(port: u16) -> Option<String> {
    let path = session_token_path(port);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut existing = String::new();
    options
        .open(path)
        .ok()?
        .read_to_string(&mut existing)
        .ok()?;
    let existing = existing.trim();
    if session_token_is_own(existing) {
        Some(existing.to_string())
    } else {
        None
    }
}

fn split_addr(addr: &str) -> (String, u16) {
    if let Some(idx) = addr.rfind(':') {
        if let Ok(port) = addr[idx + 1..].parse::<u16>() {
            return (addr[..idx].to_string(), port);
        }
    }
    (addr.to_string(), 47800)
}

/// 즉시 바인드한다(단일 인스턴스·테스트용). 포트가 점유 중이면 에러.
pub fn bind(addr: &str, timeout: Duration) -> std::io::Result<TcpLink> {
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let port = local.port();
    let link = fresh(&local.to_string(), Some(listener), timeout);
    write_session_token(port, &link.session_token);
    Ok(link)
}

/// 지연 바인드한다 — 포트를 즉시 잡지 않고 첫 에뮬레이터 호출에서 바인드한다. 그래서 MCP
/// 핸드셰이크가 포트와 무관하게 성공하고(헬스체크 통과), 포트가 이미 점유 중이면(다른
/// 인스턴스) 그 호출만 graceful하게 NotConnected가 된다(서버는 죽지 않는다).
pub fn lazy(addr: &str, timeout: Duration) -> TcpLink {
    fresh(addr, None, timeout)
}

fn handshake_stream(
    stream: TcpStream,
    timeout: Duration,
    expected_session_token: Option<&str>,
) -> Result<(Conn, Capabilities), LinkError> {
    stream.set_read_timeout(Some(timeout)).map_err(io_to_link)?;
    // 쓰기에도 같은 상한을 건다 — 플러드된 emu가 recv를 안 비우면 write_all이 영원히 블록해
    // raw_call(및 그것이 쥔 SharedLink mutex)이 통째 wedge된다. 소켓 옵션이라 try_clone한 writer에도
    // 적용된다(hello write·이후 raw_call write 모두 이 상한을 받는다).
    stream
        .set_write_timeout(Some(timeout))
        .map_err(io_to_link)?;
    stream.set_nonblocking(false).map_err(io_to_link)?;

    let mut writer = stream.try_clone().map_err(io_to_link)?;
    let mut reader = BufReader::new(stream);
    writer
        .write_all(
            to_line(&Request::new(
                0,
                "hello",
                match expected_session_token {
                    Some(token) => serde_json::json!({ "session_token": token }),
                    None => serde_json::json!({}),
                },
            ))
            .as_bytes(),
        )
        .map_err(io_to_link)?;

    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return Err(LinkError::NotConnected),
        Ok(_) => {}
        Err(e) => return Err(io_to_link(e)),
    }

    let resp = parse_response(line.trim()).map_err(|e| LinkError::Protocol(e.to_string()))?;
    if !resp.ok {
        return if let Some(err) = resp.error {
            Err(LinkError::Emulator {
                kind: err.kind,
                message: err.message,
            })
        } else {
            Err(LinkError::Protocol("hello ok=false인데 error 없음".into()))
        };
    }
    let caps_val = resp.result.unwrap_or(Value::Null);
    let protocol_version = caps_val
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let methods = caps_val
        .get("methods")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let memory_types = caps_val
        .get("memory_types")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if protocol_version != PROTOCOL_VERSION {
        return Err(LinkError::Protocol(format!(
            "프로토콜 버전 불일치: 서버 {PROTOCOL_VERSION}, 클라이언트 {protocol_version}"
        )));
    }
    let identity = super::link::EmulatorIdentity::from_hello(&caps_val);
    if let Some(expected) = expected_session_token {
        if identity.session_token.as_deref() != Some(expected) {
            return Err(LinkError::IdentityMismatch {
                expected: expected.to_string(),
                actual: identity.session_token.clone(),
                identity: Box::new(identity),
            });
        }
    }

    Ok((
        Conn {
            reader,
            writer,
            pending: String::new(),
        },
        Capabilities {
            protocol_version,
            methods,
            memory_types,
            identity,
        },
    ))
}

impl TcpLink {
    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .as_ref()
            .expect("바인드된 리스너")
            .local_addr()
            .expect("로컬 주소")
    }

    /// 테스트 관측용 — 현재 활성 연결 보유 여부(쓰기 타임아웃 poison 후 conn이 버려졌는지 검증).
    #[cfg(test)]
    pub(crate) fn has_conn(&self) -> bool {
        self.conn.is_some()
    }

    #[cfg(test)]
    pub(crate) fn set_runtime_store(&mut self, store: super::runtime::RuntimeStore) {
        self.runtime_store = store;
    }

    /// 현재 연결을 버린다. 다음 `ensure_connected`가 새 클라이언트를 재수락한다. 죽은·행된
    /// 연결을 붙들면 영영 wedge되어 세션 재시작을 강요하므로, 모든 끊김 신호(쓰기 실패·읽기
    /// EOF·읽기 에러·hello 실패·타임아웃)에서 한곳을 거쳐 비운다.
    fn drop_conn(&mut self) {
        self.conn = None;
        self.caps = Capabilities::empty();
    }

    fn set_reclaim_token(&mut self, token: &str) {
        self.session_token.clear();
        self.session_token.push_str(token);
        *self
            .preaccept_token
            .write()
            .unwrap_or_else(|e| e.into_inner()) = token.to_string();
        if let Some(port) = self.endpoint_port() {
            write_session_token(port, token);
        }
    }

    fn finish_preaccept(&mut self, wait: Duration) -> Result<bool, LinkError> {
        let Some(pre) = self.preaccept.as_ref() else {
            return Ok(false);
        };
        let msg = if wait.is_zero() {
            match pre.rx.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        } else {
            match pre.rx.recv_timeout(wait) {
                Ok(v) => Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        };

        match msg {
            Some(Ok((conn, caps, token))) if token == self.session_token => {
                self.conn = Some(conn);
                self.caps = caps;
                self.preaccept = None;
                Ok(true)
            }
            Some(Ok(_)) => {
                self.preaccept = None;
                Ok(false)
            }
            Some(Err(e)) => {
                self.preaccept = None;
                Err(e)
            }
            None => Ok(false),
        }
    }

    fn arm_preaccept(&mut self) -> Result<(), LinkError> {
        if self.preaccept.is_some() || self.conn.is_some() {
            return Ok(());
        }
        let listener = self
            .listener
            .as_ref()
            .ok_or(LinkError::NotConnected)?
            .try_clone()
            .map_err(io_to_link)?;
        let timeout = self.timeout;
        let token_source = Arc::clone(&self.preaccept_token);
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let session_token = token_source
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let result = handshake_stream(stream, timeout, Some(&session_token))
                        .map(|(conn, caps)| (conn, caps, session_token));
                    let _ = tx.send(result);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = tx.send(Err(io_to_link(e)));
                    break;
                }
            }
        });
        self.preaccept = Some(Preaccept {
            rx,
            _handle: handle,
        });
        Ok(())
    }

    /// conn을 보유 중이라도, listener에 새 클라이언트가 대기하면(ROM 교체 relaunch 등 새 에뮬 접속)
    /// 그것을 handshake한다. 성공하면(같은 세션 토큰) 기존 conn을 버리고 새 것을 채택한다 — 죽은 이전
    /// conn이 read에서 EOF 대신 timeout(EAGAIN)을 내 MAX_CONSECUTIVE_TIMEOUTS×timeout 동안 재연결을
    /// wedge하던 문제를, "새 접속 대기" 신호로 즉시 해소한다. handshake 실패(foreign/불완전)면 새 것을
    /// 버리고 기존 conn을 유지한다(건강한 conn을 spurious 접속에 잃지 않는다). 대기 없으면(WouldBlock) no-op.
    fn try_adopt_pending_client(&mut self) {
        let Some(listener) = self.listener.as_ref() else {
            return;
        };
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(_) => return, // WouldBlock(대기 없음)·기타 → 기존 conn 유지
        };
        let expected = self.session_token.clone();
        if let Ok((conn, caps)) = handshake_stream(stream, self.timeout, Some(&expected)) {
            self.conn = Some(conn);
            self.caps = caps;
            self.consecutive_timeouts = 0;
        }
        // handshake 실패면 stream은 handshake_stream이 소비·폐기 — 기존 conn 그대로.
    }

    /// 대기 중인 클라이언트를 수락하고 hello를 교환한다. 없으면 NotConnected.
    fn ensure_connected(&mut self) -> Result<(), LinkError> {
        if self.conn.is_some() {
            self.try_adopt_pending_client();
            return Ok(());
        }
        if self.finish_preaccept(Duration::from_millis(250))? {
            return Ok(());
        }
        if self.preaccept.is_some() {
            return Err(LinkError::NotConnected);
        }
        // 지연 바인드: 아직 포트를 안 잡았으면 지금 잡는다. 점유 중이면(다른 인스턴스)
        // graceful하게 NotConnected — 서버는 살아 있고 다음 호출에서 다시 시도한다.
        if self.listener.is_none() {
            // 자동 포트 선택: 기준 포트가 점유 중이면(다른 세션의 emucap-mcp) 다음 빈 포트로 옮긴다.
            // 그래서 N개 세션이 전역 설정(같은 EMUCAP_PORT)을 공유해도 각자 다른 포트를 잡아 격리된다.
            // 잡은 포트를 self.addr에 반영 — 에뮬레이터는 이 포트로 접속해야 한다(status가 알려줌).
            let (host, base) = split_addr(&self.addr);
            let mut bound = None;
            self.runtime_candidates.clear();
            let mut reclaim_token = None;
            if base != 0 {
                match select_runtime_generation(&self.runtime_store, base)? {
                    RuntimeSelection::Attach {
                        port,
                        token,
                        candidate,
                    } => match TcpListener::bind(format!("{host}:{port}")) {
                        Ok(listener) => {
                            listener.set_nonblocking(true).map_err(io_to_link)?;
                            self.addr = listener
                                .local_addr()
                                .map(|addr| addr.to_string())
                                .unwrap_or_else(|_| format!("{host}:{port}"));
                            self.runtime_candidates = vec![candidate];
                            reclaim_token = Some(token);
                            bound = Some(listener);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            self.runtime_candidates = vec![candidate];
                            return Err(LinkError::PortBusy {
                                addr: format!("{host}:{port}"),
                            });
                        }
                        Err(error) => return Err(io_to_link(error)),
                    },
                    RuntimeSelection::Blocked(candidates) => {
                        self.runtime_candidates = candidates;
                        return Err(LinkError::PortBusy {
                            addr: format!("{host}:{base}"),
                        });
                    }
                    RuntimeSelection::None => {}
                }
            }
            // 세션 고정(서버 재시작 시 같은 포트 되찾기): 지난번 바인드한 포트를 먼저 정확히 시도한다.
            // 성공하면 그 포트의 에뮬레이터가 자동 재연결돼 freeze 화면을 잃지 않는다. base==0(임시포트)은
            // 세션 고정 의미가 없어 건너뛴다. 점유 중이거나 파일이 없으면 아래 스캔으로 폴백(기존 동작).
            // 단일 bind 시도라 TOCTOU로 막혀도 그냥 폴백 — 절대 블록/루프하지 않는다. base==0(임시포트)이나
            // 안정 세션 id가 없으면(fail-closed) port_persist_path가 None → 영속화를 건너뛰고 스캔만 한다.
            let persist = port_persist_path(base);
            let persisted_port = persist.as_ref().and_then(|pf| read_persisted_port(pf));
            let mut bound_from_persist = false;
            if bound.is_none() {
                if let Some(pp) = persisted_port {
                    // 이 세션의 범위 안에 있을 때만(범위 밖/쓰레기 값은 무시). 점유면 폴백.
                    if pp >= base && (pp as u32) < base as u32 + AUTO_PORT_RANGE as u32 {
                        if let Ok(l) = TcpListener::bind(format!("{host}:{pp}")) {
                            if l.set_nonblocking(true).is_ok() {
                                self.addr = l
                                    .local_addr()
                                    .map(|a| a.to_string())
                                    .unwrap_or_else(|_| format!("{host}:{pp}"));
                                bound = Some(l);
                                bound_from_persist = true;
                            }
                        }
                    }
                }
            }
            // 폴백: 기준 포트부터 빈 포트 스캔(N 세션 자동 격리 — 같은 EMUCAP_PORT 공유 시 각자 다른 포트).
            if bound.is_none() {
                for off in 0..AUTO_PORT_RANGE {
                    // u16 범위를 넘으면 0/저포트로 wrap하지 않는다 — 더 높은 후보는 없다.
                    let port = match base.checked_add(off) {
                        Some(p) => p,
                        None => break,
                    };
                    let cand = format!("{host}:{port}");
                    match TcpListener::bind(&cand) {
                        Ok(l) => {
                            l.set_nonblocking(true).map_err(io_to_link)?;
                            // 후보 문자열이 아니라 실제로 바인드된 주소를 반영한다 — 기준 포트가
                            // 0이면 OS가 임시포트를 배정하므로, status가 진짜 포트를 알려준다.
                            self.addr = l.local_addr().map(|a| a.to_string()).unwrap_or(cand);
                            bound = Some(l);
                            break;
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
                        Err(e) => return Err(io_to_link(e)),
                    }
                }
            }
            match bound {
                Some(l) => {
                    // 잡은 포트를 영속화 — 다음 (재)시작이 이 포트를 되찾도록(best-effort, 실패 무시).
                    if let Some(pf) = persist.as_ref() {
                        if let Ok(a) = l.local_addr() {
                            write_persisted_port(pf, a.port());
                        }
                    }
                    if let Ok(a) = l.local_addr() {
                        // reclaim-own: 이 포트의 기존 토큰이 이 세션(cwd+session id 식별부) 소유면
                        // 재사용해, 서버 respawn/재연결이 토큰을 회전하지 않게 한다 — 실행 중인 자기
                        // 에뮬레이터가 옛 토큰으로 strand되지 않고 reclaim된다. 형제 세션(다른 session id)·
                        // foreign이면 새 토큰 유지 → 그 살아있는 에뮬을 이어받지 않는다.
                        if let Some(tok) = reclaim_token.take().or_else(|| {
                            bound_from_persist
                                .then(|| {
                                    self.runtime_store
                                        .live_current_with_auth(a.port())
                                        .ok()
                                        .flatten()
                                        .map(|(_, token)| token)
                                })
                                .flatten()
                                .or_else(|| reusable_session_token(a.port()))
                        }) {
                            self.set_reclaim_token(&tok);
                        }
                        write_session_token(a.port(), &self.session_token);
                    }
                    self.listener = Some(l);
                }
                // 범위 내 전부 점유 — 그래도 죽지 않고 보고만 한다.
                None => {
                    return Err(LinkError::PortBusy {
                        addr: self.addr.clone(),
                    })
                }
            }
        }
        // 막 연결된 클라이언트가 accept 큐에 들어올 짧은 여유를 준다(논블로킹 accept가
        // connect 직후를 한 박자 놓치는 레이스 흡수). 연결이 없으면 ~20ms 후 NotConnected.
        let stream = {
            let mut accepted = None;
            let listener = self.listener.as_ref().expect("위에서 바인드됨");
            for attempt in 0..10 {
                match listener.accept() {
                    Ok((s, _)) => {
                        accepted = Some(s);
                        break;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if attempt < 9 {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    Err(e) => return Err(io_to_link(e)),
                }
            }
            match accepted {
                Some(s) => s,
                None => {
                    self.arm_preaccept()?;
                    return Err(LinkError::NotConnected);
                }
            }
        };
        // hello 교환. 실패하면 half-connected 상태를 남기지 않고 비운다(다음 시도가
        // 깨끗이 재수락하도록).
        let expected = self.session_token.clone();
        let (conn, caps) = match handshake_stream(stream, self.timeout, Some(&expected)) {
            Ok(v) => v,
            Err(e) => {
                self.drop_conn();
                return Err(e);
            }
        };
        self.conn = Some(conn);
        self.caps = caps;
        // 새 클라이언트를 갓 채택했으니 이전 죽은 conn에서 누적된 타임아웃 카운트를 지운다(try_adopt_
        // pending_client와 동일). 안 그러면 stale 카운트가 신규 conn 첫 타임아웃에 그대로 이어져 조기 드롭.
        self.consecutive_timeouts = 0;
        Ok(())
    }

    /// 연결이 있다고 가정하고 요청을 보낸 뒤, 최종 응답까지 기다린다.
    /// `status:"working"` keepalive 프레임은 건너뛴다(지연 명령 진행 중).
    fn raw_call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request::new(id, method, params);

        {
            let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
            if let Err(e) = conn.writer.write_all(to_line(&req).as_bytes()) {
                // 쓰기 실패는 타임아웃이든 broken pipe든 conn을 버린다. 쓰기 타임아웃은 요청 라인의 일부만
                // 전송됐을 수 있어(대용량 params), 같은 conn에 다음 요청을 이어붙이면 상대 NDJSON 프레이밍이
                // 오염된다 — 부분 송신은 복구 불가다. 읽기 타임아웃은 conn.pending으로 부분 수신을 보존해
                // 유지하지만, 송신은 그럴 수 없으므로 버려서 다음 호출이 새 클라이언트를 재수락하게 한다
                // (부분 프레임에 새 요청이 이어붙지 못하게).
                self.drop_conn();
                if is_timeout(&e) {
                    return Err(LinkError::Timeout);
                }
                return Err(io_to_link(e));
            }
        }

        // deferred(working keepalive) 명령의 전체 벽시계 데드라인. working은 id가 일치해 매 Ok read가
        // consecutive_timeouts를 리셋하므로 3-timeout 가드가 못 끊는다 — 이 상한으로 총 대기를 유한하게.
        let deadline = std::time::Instant::now() + self.deferred_deadline;

        // id 불일치(늦은 응답·desync) 프레임은 버리고 계속 읽되, 한도를 둔다 — 끝없이
        // 오면 raw_call이 무한 점유되므로 desync로 간주해 빠르게 실패한다. working
        // keepalive는 id가 일치하므로 이 카운터에 잡히지 않는다(긴 명령은 영향 없음).
        const MAX_ID_MISMATCH: u32 = 256;
        let mut mismatch = 0u32;
        loop {
            // deferred 데드라인 초과 = working이 끝없이 오거나 id 불일치가 오래 이어짐. 진짜 완료가 안
            // 오는 것으로 보고 드롭+Timeout(무한 점유 금지). 개별 read 타임아웃은 아래 arm이 처리한다.
            if std::time::Instant::now() >= deadline {
                self.consecutive_timeouts = 0;
                self.drop_conn();
                return Err(LinkError::Timeout);
            }
            // 영속 버퍼(conn.pending)로 읽는다 — 타임아웃이 줄 중간에 걸려도 이미 읽은 바이트가 보존돼
            // 다음 호출이 이어 읽는다. read_line 결과만 받고 conn 빌림을 끝내(아래 drop_conn과 충돌 방지).
            let read_result = {
                let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
                conn.reader.read_line(&mut conn.pending)
            };
            match read_result {
                Ok(0) => {
                    self.drop_conn();
                    return Err(LinkError::NotConnected);
                }
                Ok(_) => {
                    // read_line은 첫 \n까지 — pending에 완성된 한 줄(+\n). 꺼내 비운다(다음 줄 대비).
                    let line = {
                        let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
                        std::mem::take(&mut conn.pending)
                    };
                    self.consecutive_timeouts = 0; // 응답 수신 = 어댑터 살아있음 → 타임아웃 카운터 리셋
                    let resp = parse_response(line.trim())
                        .map_err(|e| LinkError::Protocol(e.to_string()))?;
                    if resp.id != id {
                        mismatch += 1;
                        if mismatch > MAX_ID_MISMATCH {
                            self.drop_conn();
                            return Err(LinkError::Protocol(
                                "id 불일치 프레임이 한도를 초과 — 스트림 desync".into(),
                            ));
                        }
                        // 이전에 타임아웃된 명령의 늦은 응답 등 — id가 안 맞으면 버리고 계속.
                        // (안 버리면 응답이 한 칸씩 밀려 스트림이 desync된다.)
                        continue;
                    }
                    if !resp.ok {
                        return if let Some(err) = resp.error {
                            Err(LinkError::Emulator {
                                kind: err.kind,
                                message: err.message,
                            })
                        } else {
                            Err(LinkError::Protocol("ok=false인데 error 없음".into()))
                        };
                    }
                    let result = resp.result.unwrap_or(Value::Null);
                    if super::protocol::result_status(&result) == super::protocol::STATUS_WORKING {
                        continue; // keepalive — 다음 줄을 더 읽는다
                    }
                    return Ok(result);
                }
                Err(ref e) if is_timeout(e) => {
                    // 요청 타임아웃은 단발이면 연결을 끊지 '않는다'. 느리지만 살아있는 어댑터(큰 VRAM/OAM
                    // read, NMI 빈번 장면, frozen 캡처 등)를 죽이면 공들인 게임 상태가 날아간다. 부분 수신한
                    // 줄은 conn.pending에 남아 다음 호출이 이어 읽으므로 스트림 desync도 없다(늦은 응답은
                    // id 불일치로 버려진다). 단, 연속 임계치면 진짜 행으로 보고 드롭해 재수락을 유도한다
                    // (안 그러면 죽은 어댑터에 영영 wedge). 진짜 죽음은 쓰기 실패·EOF로도 감지된다.
                    self.consecutive_timeouts += 1;
                    if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                        self.consecutive_timeouts = 0;
                        self.drop_conn();
                    }
                    return Err(LinkError::Timeout);
                }
                Err(e) => {
                    self.drop_conn();
                    return Err(io_to_link(e));
                }
            }
        }
    }

    /// deferred 데드라인을 짧게 바꿔 테스트에서 working 플러드 컷오프를 빠르게 검증한다(프로덕션 미포함).
    #[cfg(test)]
    pub(crate) fn set_deferred_deadline(&mut self, d: Duration) {
        self.deferred_deadline = d;
    }
}

impl EmulatorLink for TcpLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.ensure_connected()?;
        self.raw_call(method, params)
    }

    fn endpoint_port(&self) -> Option<u16> {
        // self.addr는 자동 선택 후 잡은 포트를 반영한다(미바인드면 기준 포트).
        Some(split_addr(&self.addr).1)
    }

    fn session_token(&self) -> Option<&str> {
        Some(&self.session_token)
    }

    fn replace_reclaim_token(&mut self, token: &str) -> Result<bool, LinkError> {
        self.drop_conn();
        self.set_reclaim_token(token);
        Ok(true)
    }

    fn runtime_candidates(&self) -> Vec<Value> {
        self.runtime_candidates.clone()
    }
}

pub(crate) enum RuntimeSelection {
    None,
    Attach {
        port: u16,
        token: String,
        candidate: Value,
    },
    Blocked(Vec<Value>),
}

pub(crate) fn select_runtime_generation(
    store: &super::runtime::RuntimeStore,
    base: u16,
) -> Result<RuntimeSelection, LinkError> {
    let Some(control_key) = super::runtime::control_session_key() else {
        return Ok(RuntimeSelection::None);
    };
    let holder = super::runtime::capture_process(std::process::id());
    let mut preferred = Vec::new();
    let mut reclaimable = Vec::new();
    let mut same_session_blocked = Vec::new();
    let mut unreclaimable = Vec::new();
    for offset in 0..AUTO_PORT_RANGE {
        let Some(port) = base.checked_add(offset) else {
            break;
        };
        let Some(current) = store.read_current(port).map_err(io_to_link)? else {
            continue;
        };
        if current.process_state() != super::runtime::ProcessState::Alive {
            continue;
        }
        let record = store
            .read_link_json::<super::continuity::LinkRecord>(port, &current.launch_id)
            .map_err(io_to_link)?
            .filter(|record| record.launch_id == current.launch_id);
        let Some(lease) = record.as_ref().and_then(|record| record.lease.as_ref()) else {
            // No lease means there is no holder whose death can be proven. A same-session
            // persisted port can still recover through the legacy exact-port path below, but a
            // range scan must not adopt this generation as merely "available".
            continue;
        };
        let lease_state = {
            if lease.holder == holder {
                super::runtime::LeaseState::Held
            } else {
                match super::runtime::process_state(&lease.holder) {
                    super::runtime::ProcessState::Alive => super::runtime::LeaseState::Occupied,
                    super::runtime::ProcessState::Exited => super::runtime::LeaseState::Available,
                    super::runtime::ProcessState::Unknown => super::runtime::LeaseState::Unknown,
                }
            }
        };
        let same_control = lease.control_session_key.as_deref() == Some(control_key.as_str());
        let token = store
            .read_auth(port, &current.launch_id)
            .map_err(io_to_link)?;
        let mut candidate = current.public_value_with_lease(&super::runtime::LeaseView {
            state: lease_state,
            holder_pid: Some(lease.holder.pid),
        });
        if let Some(object) = candidate.as_object_mut() {
            object.insert(
                "attach".into(),
                serde_json::json!({"port": port, "launch_id": current.launch_id}),
            );
            object.insert(
                "reclaim_capability_available".into(),
                Value::Bool(token.is_some()),
            );
        }
        if token.is_none() && (same_control || lease_state == super::runtime::LeaseState::Available)
        {
            unreclaimable.push(candidate);
            continue;
        }
        match (same_control, lease_state, token) {
            (true, super::runtime::LeaseState::Held, Some(token)) => {
                preferred.push((port, token, candidate));
            }
            (true, super::runtime::LeaseState::Available, Some(token)) => {
                preferred.push((port, token, candidate));
            }
            (
                true,
                super::runtime::LeaseState::Occupied | super::runtime::LeaseState::Unknown,
                _,
            ) => {
                same_session_blocked.push(candidate);
            }
            (_, super::runtime::LeaseState::Available, Some(token)) => {
                reclaimable.push((port, token, candidate));
            }
            _ => {}
        }
    }
    if preferred.len() == 1 {
        let (port, token, candidate) = preferred.pop().expect("one preferred candidate");
        return Ok(RuntimeSelection::Attach {
            port,
            token,
            candidate,
        });
    }
    if preferred.len() > 1 {
        return Ok(RuntimeSelection::Blocked(
            preferred.into_iter().map(|(_, _, value)| value).collect(),
        ));
    }
    if !same_session_blocked.is_empty() {
        return Ok(RuntimeSelection::Blocked(same_session_blocked));
    }
    if !unreclaimable.is_empty() {
        return Ok(RuntimeSelection::Blocked(unreclaimable));
    }
    if reclaimable.len() == 1 {
        let (port, token, candidate) = reclaimable.pop().expect("one reclaimable candidate");
        return Ok(RuntimeSelection::Attach {
            port,
            token,
            candidate,
        });
    }
    if reclaimable.len() > 1 {
        return Ok(RuntimeSelection::Blocked(
            reclaimable.into_iter().map(|(_, _, value)| value).collect(),
        ));
    }
    Ok(RuntimeSelection::None)
}

/// read/write가 설정된 상한 안에 진행하지 못했을 때의 타임아웃(SO_RCVTIMEO/SO_SNDTIMEO). 블로킹
/// 소켓에서 OS는 EAGAIN(WouldBlock) 또는 ETIMEDOUT(TimedOut) 중 하나로 알린다 — 둘 다 비치명 타임아웃.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn io_to_link(e: std::io::Error) -> LinkError {
    match e.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::UnexpectedEof => LinkError::NotConnected,
        _ => LinkError::Protocol(format!("io: {e}")),
    }
}
