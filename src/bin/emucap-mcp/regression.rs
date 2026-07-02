use emucap::analysis::bisect::{self, CmpOp, Predicate};
use emucap::analysis::regression;
use emucap::live::link::{EmulatorLink, LinkError};

// 본체 bin의 핸들러들이 `regression::load_suite` 등 외부 크레이트 항목을 *이 로컬 모듈을 통해*
// 참조하도록 재노출한다(본체는 `mod regression;`이라 `use emucap::analysis::regression;`을 동시에
// 둘 수 없다 — 이름 충돌). 핸들러 본문을 손대지 않고 경로를 그대로 유지하기 위함.
pub(crate) use emucap::analysis::regression::{load_case, load_suite, CaseResult, Summary};

#[cfg(test)]
#[path = "regression_tests.rs"]
pub(crate) mod tests;

/// 결정론 게이트 판정. 측정 무효(MeasurementInvalid)와 진짜 재현 불가(NotReproducible)를
/// 절대 섞지 않는다.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DetOutcome {
    Reproducible,
    NotReproducible,
    Unsupported(String),
    MeasurementInvalid(String),
}
impl DetOutcome {
    pub(crate) fn passed(&self) -> Option<bool> {
        match self {
            DetOutcome::Reproducible => Some(true),
            DetOutcome::NotReproducible => Some(false),
            DetOutcome::Unsupported(_) | DetOutcome::MeasurementInvalid(_) => None,
        }
    }
    pub(crate) fn code(&self) -> String {
        match self {
            DetOutcome::Reproducible => "reproducible".into(),
            DetOutcome::NotReproducible => "not_reproducible".into(),
            DetOutcome::Unsupported(m) => format!("unsupported:{m}"),
            DetOutcome::MeasurementInvalid(m) => format!("measurement_invalid:{m}"),
        }
    }
}

pub(crate) struct DetResult {
    pub(crate) outcome: DetOutcome,
    pub(crate) observe_kind: String,
    pub(crate) replays: u32,
    pub(crate) hashes: Vec<String>,
}

/// 한 번의 재생 + 관측. InputReplay는 frozen observe_hash, Savestate+Memory는 원자 probe.
fn run_repro_observe(
    link: &mut dyn EmulatorLink,
    dir: &std::path::Path,
    repro: &regression::Repro,
    observe: &emucap::track::observe::ObserveSpec,
) -> Result<emucap::track::observe::ObserveOutcome, DetOutcome> {
    use emucap::track::observe::{observe_hash, sha256_hex, ObserveOutcome, ObserveSpec};
    match repro {
        regression::Repro::Savestate {
            state_sha1,
            advance_frames,
        } => {
            // 광역 관측치는 savestate 원자 경로(probe)가 못 싣는다 → 강등.
            let ObserveSpec::Memory {
                memory_type,
                address,
                length,
            } = observe
            else {
                return Err(DetOutcome::Unsupported("savestate_broad_observe".into()));
            };
            if !has_method(link, "probe") {
                return Err(DetOutcome::Unsupported("probe".into()));
            }
            let mss = dir.join(format!("{state_sha1}.mss"));
            if !mss.is_file() {
                return Err(DetOutcome::MeasurementInvalid("missing_payload".into()));
            }
            // probe_bytes는 length 1~8 제약 없음 — dummy op/value Predicate.
            let pred = Predicate {
                memory_type: memory_type.clone(),
                address: *address,
                length: *length,
                op: CmpOp::Eq,
                value: 0,
            };
            match bisect::probe_bytes(link, &mss.to_string_lossy(), *advance_frames, &pred) {
                Ok(bytes) => Ok(ObserveOutcome {
                    kind_used: "memory".into(),
                    sha256: sha256_hex(&bytes),
                    byte_len: bytes.len(),
                }),
                Err(e) => Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}"))),
            }
        }
        regression::Repro::InputReplay {
            start,
            movie,
            anchor,
        } => {
            // 재생 필수 메서드 + observe 요구 메서드 capability 게이트(재생 전 선검사).
            // read_memory는 재생 자체엔 안 쓴다(observe=Memory일 때만 필요 → observe_missing이 검사).
            // 무조건 포함하면 screenshot/state-only 어댑터를 거짓 Unsupported로 거부한다(강등 위반).
            let mut required = vec!["pause", "set_input", "step", "clear_all_breakpoints"];
            if start == "reset" {
                required.push("reset");
            } else {
                required.push("load_state");
            }
            if anchor.is_some() {
                required.extend(["set_breakpoint", "poll_events"]);
            }
            let observe_missing = match observe {
                ObserveSpec::Auto => {
                    !has_method(link, "screenshot") && !has_method(link, "get_state")
                }
                ObserveSpec::Memory { .. } => !has_method(link, "read_memory"),
                ObserveSpec::Screenshot => !has_method(link, "screenshot"),
                ObserveSpec::State => !has_method(link, "get_state"),
            };
            let missing = missing_methods(link, &required);
            if !missing.is_empty() || observe_missing {
                return Err(DetOutcome::Unsupported(format!(
                    "missing methods: {missing:?} observe_missing={observe_missing}"
                )));
            }
            if start != "reset" {
                match load_state_replay_supported(link) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(DetOutcome::Unsupported(
                            "load_state_nondeterministic".into(),
                        ))
                    }
                    Err(e) => {
                        return Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}")))
                    }
                }
            }
            let movie_path = dir.join(movie);
            let text = match std::fs::read_to_string(&movie_path) {
                Ok(t) => t,
                Err(_) => return Err(DetOutcome::MeasurementInvalid("missing_payload".into())),
            };
            let mv = match regression::parse_movie(&text) {
                Ok(m) => m,
                Err(e) => return Err(DetOutcome::MeasurementInvalid(format!("invalid: {e}"))),
            };
            // 시작 복원(케이스 격리)
            let start_res = if start == "reset" {
                link.call("reset", serde_json::json!({})).map(|_| ())
            } else {
                let mss = dir.join(format!("{start}.mss"));
                if !mss.is_file() {
                    return Err(DetOutcome::MeasurementInvalid("missing_payload".into()));
                }
                link.call(
                    "load_state",
                    serde_json::json!({"path": mss.to_string_lossy()}),
                )
                .map(|_| ())
            };
            if let Err(e) = start_res {
                return Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}")));
            }
            // frozen 재생 → 종점 observe_hash(capability 선검사 통과 → 여기 에러는 측정 무효)
            let mut obs =
                |l: &mut dyn EmulatorLink| observe_hash(l, observe).map_err(|e| e.to_string());
            match replay_movie_observe(link, &mv, anchor.as_ref(), &mut obs) {
                Ok(Some(o)) => Ok(o),
                Ok(None) => Err(DetOutcome::MeasurementInvalid("anchor_miss".into())),
                Err(e) => Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}"))),
            }
        }
    }
}

pub(crate) fn parse_observe_spec(
    observe: Option<&str>,
    memory_type: Option<String>,
    address: Option<u64>,
    length: Option<u64>,
) -> Result<emucap::track::observe::ObserveSpec, String> {
    use emucap::track::observe::ObserveSpec;
    match observe.unwrap_or("auto") {
        "auto" => Ok(ObserveSpec::Auto),
        "screenshot" => Ok(ObserveSpec::Screenshot),
        "state" => Ok(ObserveSpec::State),
        "memory" => {
            let memory_type = memory_type.ok_or("observe=memory엔 memory_type 필요")?;
            let address = address.ok_or("observe=memory엔 address 필요")?;
            let length = length.ok_or("observe=memory엔 length 필요")?;
            if length == 0 {
                return Err("observe=memory length는 1 이상이어야".into());
            }
            Ok(ObserveSpec::Memory {
                memory_type,
                address,
                length,
            })
        }
        other => Err(format!(
            "알 수 없는 observe: {other} (auto|memory|screenshot|state)"
        )),
    }
}

/// 재현 레시피를 replays회 재생해 관측치 해시 일치를 판정한다. 어느 재생이든 Err면 그
/// outcome으로 즉시 중단한다. 재생 구간 내 로깅 없음.
pub(crate) fn verify_determinism_core(
    link: &mut dyn EmulatorLink,
    dir: &std::path::Path,
    case: &regression::Case,
    observe: &emucap::track::observe::ObserveSpec,
    replays: u32,
) -> DetResult {
    debug_assert!(
        replays >= 2,
        "verify_determinism_core requires replays >= 2"
    );
    if replays < 2 {
        return DetResult {
            outcome: DetOutcome::MeasurementInvalid("replays_lt_2".into()),
            observe_kind: String::new(),
            replays,
            hashes: vec![],
        };
    }
    // rom 대조(get_rom_info 가용 시). 불일치는 측정 무효.
    if has_method(link, "get_rom_info") {
        if let Ok(info) = link.call("get_rom_info", serde_json::json!({})) {
            // Mednafen은 content_md5가 canonical(run_start에 그 값을 넘김), Mesen/PC-98은 sha1. 저장된 키와 같은 우선순위로 대조.
            let h = info
                .get("content_md5")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| info.get("sha1").and_then(|s| s.as_str()))
                .unwrap_or("");
            if !h.is_empty() && h != "skipped:too_large" && h != case.rom.sha1 {
                return DetResult {
                    outcome: DetOutcome::MeasurementInvalid("rom_mismatch".into()),
                    observe_kind: String::new(),
                    replays,
                    hashes: vec![],
                };
            }
        }
    }
    let mut hashes = Vec::new();
    let mut observe_kind = String::new();
    for _ in 0..replays {
        match run_repro_observe(link, dir, &case.repro, observe) {
            Ok(o) => {
                observe_kind = o.kind_used.clone();
                hashes.push(o.sha256);
            }
            Err(outcome) => {
                return DetResult {
                    outcome,
                    observe_kind,
                    replays,
                    hashes,
                }
            }
        }
    }
    let all_equal = hashes.windows(2).all(|w| w[0] == w[1]);
    let outcome = if all_equal {
        DetOutcome::Reproducible
    } else {
        DetOutcome::NotReproducible
    };
    DetResult {
        outcome,
        observe_kind,
        replays,
        hashes,
    }
}

/// 한 케이스를 현재 link으로 재현·판정한다. savestate는 원자적 probe로, 읽기 검증은 evaluate가.
pub(crate) fn run_one_case(
    link: &mut dyn EmulatorLink,
    dir: &std::path::Path,
    case: &regression::Case,
) -> regression::Verdict {
    // 케이스 자체 검증: id == 디렉토리명
    let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if dir_name != case.id {
        return regression::Verdict::Invalid(format!("id({})≠디렉토리({})", case.id, dir_name));
    }
    if case.predicate.length == 0 || case.predicate.length > 8 {
        return regression::Verdict::Invalid(format!("length 위반: {}", case.predicate.length));
    }
    if let Err(e) = ensure_capabilities_loaded(link) {
        return regression::Verdict::ReproError(format!("{e}"));
    }
    // rom 대조(get_rom_info를 advertise하는 어댑터만 — capability 체크로 분기). 불일치는 합산 제외.
    if link
        .capabilities()
        .methods
        .iter()
        .any(|m| m == "get_rom_info")
    {
        if let Ok(info) = link.call("get_rom_info", serde_json::json!({})) {
            // Mednafen은 content_md5가 canonical(run_start에 그 값을 넘김), Mesen/PC-98은 sha1. 저장된 키와 같은 우선순위로 대조.
            let h = info
                .get("content_md5")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| info.get("sha1").and_then(|s| s.as_str()))
                .unwrap_or("");
            if !h.is_empty() && h != "skipped:too_large" && h != case.rom.sha1 {
                return regression::Verdict::RomMismatch;
            }
        }
    }
    match &case.repro {
        regression::Repro::Savestate {
            state_sha1,
            advance_frames,
        } => {
            if !has_method(link, "probe") {
                return regression::Verdict::Unsupported;
            }
            let mss = dir.join(format!("{state_sha1}.mss"));
            if !mss.is_file() {
                return regression::Verdict::MissingPayload;
            }
            // 원자적 probe(load→진행→읽기). 비결정론·네트워크 갭 없음. 바이트로 invalid_read 검증.
            match bisect::probe_bytes(
                link,
                &mss.to_string_lossy(),
                *advance_frames,
                &case.predicate,
            ) {
                Ok(bytes) => regression::evaluate(&bytes, &case.predicate, case.expect),
                Err(e) => regression::Verdict::ReproError(format!("{e}")),
            }
        }
        regression::Repro::InputReplay {
            start,
            movie,
            anchor,
        } => {
            let mut required = vec![
                "pause",
                "set_input",
                "step",
                "read_memory",
                "clear_all_breakpoints",
            ];
            if start == "reset" {
                required.push("reset");
            } else {
                required.push("load_state");
            }
            if anchor.is_some() {
                required.extend(["set_breakpoint", "poll_events"]);
            }
            let missing = missing_methods(link, &required);
            if !missing.is_empty() {
                return regression::Verdict::Unsupported;
            }
            if start != "reset" {
                match load_state_replay_supported(link) {
                    Ok(true) => {}
                    Ok(false) => return regression::Verdict::Unsupported,
                    Err(e) => return regression::Verdict::ReproError(e),
                }
            }
            let movie_path = dir.join(movie);
            let text = match std::fs::read_to_string(&movie_path) {
                Ok(t) => t,
                Err(_) => return regression::Verdict::MissingPayload,
            };
            let mv = match regression::parse_movie(&text) {
                Ok(m) => m,
                Err(e) => return regression::Verdict::Invalid(e),
            };
            // 시작점 복원(케이스 격리)
            let start_res = if start == "reset" {
                link.call("reset", serde_json::json!({})).map(|_| ())
            } else {
                let mss = dir.join(format!("{start}.mss"));
                if !mss.is_file() {
                    return regression::Verdict::MissingPayload;
                }
                link.call(
                    "load_state",
                    serde_json::json!({"path": mss.to_string_lossy()}),
                )
                .map(|_| ())
            };
            if let Err(e) = start_res {
                return regression::Verdict::ReproError(format!("{e}"));
            }
            // frozen에서 무비를 프레임별로 적용(자유 실행 누수 차단)
            match replay_movie_and_read(link, &mv, anchor.as_ref(), &case.predicate) {
                Ok(Some(bytes)) => regression::evaluate(&bytes, &case.predicate, case.expect),
                Ok(None) => regression::Verdict::DriftSuspected, // 앵커 미히트
                Err(e) => regression::Verdict::ReproError(e),
            }
        }
    }
}

pub(crate) fn ensure_capabilities_loaded(link: &mut dyn EmulatorLink) -> Result<(), LinkError> {
    if link.capabilities().methods.is_empty() {
        link.call("status", serde_json::json!({}))?;
    }
    Ok(())
}

fn has_method(link: &dyn EmulatorLink, method: &str) -> bool {
    link.capabilities().methods.iter().any(|m| m == method)
}

fn missing_methods(link: &dyn EmulatorLink, methods: &[&str]) -> Vec<String> {
    methods
        .iter()
        .copied()
        .filter(|m| !has_method(link, m))
        .map(str::to_string)
        .collect()
}

pub(crate) fn require_method(
    link: &mut dyn EmulatorLink,
    method: &str,
    context: &str,
) -> Result<(), LinkError> {
    ensure_capabilities_loaded(link)?;
    if has_method(link, method) {
        Ok(())
    } else {
        Err(LinkError::Protocol(format!(
            "{context} requires adapter method `{method}`; deterministic replay is unsupported for this adapter"
        )))
    }
}

fn load_state_replay_supported(link: &mut dyn EmulatorLink) -> Result<bool, String> {
    if !has_method(link, "status") {
        return Ok(true);
    }
    let status = link
        .call("status", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    let Some(state_restore) = status.get("state_restore") else {
        return Ok(true);
    };
    for key in [
        "deterministic_replay",
        "hidden_device_state",
        "post_restore_instruction_exact",
    ] {
        if state_restore.get(key).and_then(|v| v.as_bool()) == Some(false) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// broker 세션 포트 기본값. emu_port가 높아도 u16 wrap(→저포트)하지 않는다.
pub(crate) fn default_session_port(emu_port: u16) -> u16 {
    emu_port.saturating_add(100)
}

/// 무비 재생이 다룰 수 있는 최대 frame 번호(~60fps로 약 4.6시간). 회귀 재생엔 충분하고,
/// 그 이상은 신뢰할 수 없는 무비로 본다.
const MAX_REPLAY_FRAMES: u64 = 1_000_000;

/// 무비 프레임이 단조 증가하고 합리적 상한 안인지 검증한다 — 신뢰할 수 없는 무비의
/// 거대/역행 frame 번호가 무제한 step·네트워크 호출 루프를 일으키지 않게 한다.
fn validate_movie_frames(movie: &regression::Movie) -> Result<(), String> {
    for w in movie.frames.windows(2) {
        // 엄격 증가(중복 frame 거부): 같은 frame이 둘이면 재생 루프가 첫 버튼셋을 조용히 덮어쓰고
        // 프레임을 하나 더 소비해 회귀 판정이 틀린 시점에 일어난다 — 조용한 오류 대신 드러낸다.
        if w[1].frame <= w[0].frame {
            return Err(format!(
                "무비 frame이 엄격 증가가 아님(중복·역행): {} 다음에 {}",
                w[0].frame, w[1].frame
            ));
        }
    }
    if let Some(last) = movie.frames.last() {
        if last.frame > MAX_REPLAY_FRAMES {
            return Err(format!(
                "무비 최대 frame {}이 상한 {} 초과",
                last.frame, MAX_REPLAY_FRAMES
            ));
        }
    }
    Ok(())
}

/// 무비를 frozen 프레임별로 적용하고, 종점(또는 anchor 히트)에서 `observe`를 1회 호출한다.
/// regression(predicate read)·결정론 게이트(observe_hash)가 공유한다. anchor 미히트면 Ok(None).
fn replay_movie_observe<T>(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    anchor: Option<&Predicate>,
    observe: &mut dyn FnMut(&mut dyn EmulatorLink) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let to_e = |e: LinkError| e.to_string();
    validate_movie_frames(movie)?;
    link.call("pause", serde_json::json!({})).map_err(to_e)?;
    if let Some(a) = anchor {
        // arming 전에 직전 활동(예: 회귀 루프의 이전 케이스·verify_determinism 반복)이 남긴 stale BP·
        // 이벤트를 비운다 — 그러지 않으면 첫 poll_anchor_observe가 옛 이벤트를 소비해 엉뚱한 시점에
        // false-positive(조용히 틀린 PASS)를 낸다(best-effort — 미지원 어댑터는 무해).
        let _ = link.call("clear_all_breakpoints", serde_json::json!({}));
        let _ = link.call("poll_events", serde_json::json!({}));
        link.call(
            "set_breakpoint",
            serde_json::json!({
                "kind": "exec", "memory_type": a.memory_type,
                "start": a.address, "end": a.address, "pause_on_hit": true
            }),
        )
        .map_err(to_e)?;
        if let Some(v) = poll_anchor_observe(link, observe)? {
            return Ok(Some(v));
        }
    }
    let empty: Vec<String> = vec![];
    let mut next = 0u64;
    for mf in &movie.frames {
        while next < mf.frame {
            // 사이 프레임은 입력 없이 진행
            link.call("set_input", serde_json::json!({"buttons": empty}))
                .map_err(to_e)?;
            link.call("step", serde_json::json!({"frames": 1}))
                .map_err(to_e)?;
            next += 1;
            if anchor.is_some() {
                if let Some(v) = poll_anchor_observe(link, observe)? {
                    return Ok(Some(v));
                }
            }
        }
        link.call("set_input", serde_json::json!({"buttons": mf.buttons}))
            .map_err(to_e)?;
        link.call("step", serde_json::json!({"frames": 1}))
            .map_err(to_e)?;
        next += 1;
        if anchor.is_some() {
            if let Some(v) = poll_anchor_observe(link, observe)? {
                return Ok(Some(v));
            }
        }
    }
    link.call("clear_all_breakpoints", serde_json::json!({}))
        .ok();
    if anchor.is_some() {
        return Ok(None); // 앵커 미히트 = drift/측정 무효
    }
    Ok(Some(observe(link)?))
}

fn poll_anchor_observe<T>(
    link: &mut dyn EmulatorLink,
    observe: &mut dyn FnMut(&mut dyn EmulatorLink) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let ev = link
        .call("poll_events", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    if ev
        .get("events")
        .and_then(|e| e.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        let v = observe(link)?;
        link.call("clear_all_breakpoints", serde_json::json!({}))
            .ok();
        return Ok(Some(v));
    }
    Ok(None)
}

fn replay_movie_and_read(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    anchor: Option<&Predicate>,
    target: &Predicate,
) -> Result<Option<Vec<u8>>, String> {
    let mut obs = |l: &mut dyn EmulatorLink| read_target(l, target);
    replay_movie_observe(link, movie, anchor, &mut obs)
}

fn read_target(link: &mut dyn EmulatorLink, p: &Predicate) -> Result<Vec<u8>, String> {
    let r = link
        .call(
            "read_memory",
            serde_json::json!({
                "memory_type": p.memory_type, "address": p.address, "length": p.length
            }),
        )
        .map_err(|e| e.to_string())?;
    let hex = r
        .get("hex")
        .and_then(|h| h.as_str())
        .ok_or("read_memory 응답에 hex 없음")?;
    bisect::hex_to_bytes(hex)
}
