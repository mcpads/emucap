use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};

use super::link::{EmulatorLink, LinkError};

#[derive(Debug, PartialEq)]
pub enum ToolOutput {
    Json(Value),
    Image {
        png_base64: String,
        saved_path: Option<String>,
    },
}

pub fn read_memory(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    length: u64,
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "memory_type": memory_type, "address": address, "length": length });
    Ok(ToolOutput::Json(link.call("read_memory", params)?))
}

/// 세이브스테이트 복귀 → frame 진행 → 타깃 읽기를 adapter 안에서 한 단위로 수행한다.
/// bisect/regression뿐 아니라 에이전트가 직접 결정론적 probe를 호출할 때도 같은 경로를 쓴다.
pub fn probe(
    link: &mut dyn EmulatorLink,
    state: &str,
    frame: u64,
    memory_type: &str,
    address: u64,
    length: u64,
) -> Result<ToolOutput, LinkError> {
    let params = json!({
        "state": state, "frame": frame,
        "memory_type": memory_type, "address": address, "length": length,
    });
    Ok(ToolOutput::Json(link.call("probe", params)?))
}

/// 에뮬 메모리 영역을 어댑터 내부에서 스캔해 바이트열(hex) 패턴의 매칭 오프셋들만 회신한다.
/// 128KB를 와이어로 안 보내고 오프셋만 돌려줘 토큰·지연을 최소화한다(런타임 문자열/버퍼/테이블 특정).
pub fn find_pattern(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    hex: &str,
    start: u64,
    length: Option<u64>,
    max_matches: u64,
    align: u64,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({
        "memory_type": memory_type, "hex": hex,
        "start": start, "max_matches": max_matches, "align": align,
    });
    if let Some(l) = length {
        params["length"] = json!(l);
    }
    Ok(ToolOutput::Json(link.call("find_pattern", params)?))
}

pub fn get_state(link: &mut dyn EmulatorLink, groups: &[String]) -> Result<ToolOutput, LinkError> {
    let params = if groups.is_empty() {
        json!({})
    } else {
        json!({ "groups": groups })
    };
    Ok(ToolOutput::Json(link.call("get_state", params)?))
}

/// Saturn VDP2 비디오 상태를 per-NBG로 디코드해 반환한다(어댑터가 RawRegs를 렌더러 공식으로 디코드).
/// Saturn 전용 — 어댑터가 미지원 시 에러를 반환한다(가용성은 status.methods로 확인).
pub fn get_video_state(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("get_video_state", json!({}))?))
}

/// Saturn 화면좌표(NBGn, x, y) → 그 셀의 char 데이터 베이스 주소를 per-tile로 풀어 반환한다(어댑터가
/// 스크롤 가산·PLSZ 랩·PNT 엔트리 읽기·supplement→charno를 렌더러 권위 공식으로 접는다). 중간값
/// (nt_addr·raw PND·charno·cellbytes·palno·flip) 동봉. Saturn 전용 — 가용성은 status.methods로 확인.
pub fn resolve_tile(
    link: &mut dyn EmulatorLink,
    nbg: u32,
    x: u32,
    y: u32,
) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call(
        "resolve_tile",
        json!({ "nbg": nbg, "x": x, "y": y }),
    )?))
}

/// Mednafen 내장 레이어 enable 마스크를 토글한다(비파괴 VDP1/VDP2 라우팅 확정·클린플레이트용). 어댑터가
/// MDFNGameInfo->LayerNames를 파싱해 이름↔비트를 매핑하고, layers(이름 배열, 대소문자 무시 → 그것만 enable·
/// 나머지 disable) 또는 mask(raw)로 마스크를 조립해 적용한다. 둘 다 생략 시 변경 없이 조회만. PSX 등
/// LayerNames 없는 시스템은 미지원(가용성은 status.methods로 확인). 반환 {layer_names, mask, enabled}.
pub fn set_layer_enable(
    link: &mut dyn EmulatorLink,
    layers: &[String],
    mask: Option<u64>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    if !layers.is_empty() {
        params["layers"] = json!(layers);
    }
    if let Some(m) = mask {
        params["mask"] = json!(m);
    }
    Ok(ToolOutput::Json(link.call("set_layer_enable", params)?))
}

pub fn get_rom_info(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("get_rom_info", json!({}))?))
}

pub fn status(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("status", json!({}))?))
}

pub fn write_memory(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    hex: &str,
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "memory_type": memory_type, "address": address, "hex": hex });
    Ok(ToolOutput::Json(link.call("write_memory", params)?))
}

pub fn set_input(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "port": port, "buttons": buttons });
    Ok(ToolOutput::Json(link.call("set_input", params)?))
}

/// 버튼을 frames만큼 실시간으로 누르고 뗀다. raw press_buttons는 지연 명령이라 frozen에선 프레임이 안 흘러
/// no-op이던 것을, 어댑터 press_buttons 핸들러가 g_frozen=false로 *원자적으로* resume해 해결한다(run_frames와
/// 동일). 별도 ensure_running(resume)은 명령 도착 전 free-run으로 watch/BP를 조기 소진시키는 레이스라 쓰지
/// 않는다. frozen을 유지하며 결정론적 단발이 필요하면 tap을 쓴다.
pub fn press_buttons(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    frames: u64,
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "port": port, "buttons": buttons, "frames": frames });
    Ok(ToolOutput::Json(link.call("press_buttons", params)?))
}

/// 한 번의 frozen 탭: set_input→step(press_frames)→해제→해제에지. tap·tap_sequence 공용.
fn one_tap(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
) -> Result<(), LinkError> {
    let empty: [String; 0] = [];
    link.call("set_input", json!({ "port": port, "buttons": buttons }))?;
    link.call("step", json!({ "frames": press_frames.max(1) }))?;
    link.call("set_input", json!({ "port": port, "buttons": empty }))?; // 해제
    link.call("step", json!({ "frames": 1 }))?; // 해제 에지
    Ok(())
}

/// 프레임 단위 정밀 탭: freeze에서 정확히 press_frames만 입력을 주고 떼어, auto-repeat 없이
/// 결정론적 단일 입력(메뉴/타일 1칸)을 만든다. after_frames>0이면 떼고 그만큼 더 진행한다
/// (입력+관찰을 한 콜에 — frozen 유지). 호출 후 frozen 유지: 또 tap하거나 resume/run_frames.
pub fn tap(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    link.call("pause", json!({}))?; // 멱등
    one_tap(link, port, buttons, press_frames)?;
    if after_frames > 0 {
        link.call("step", json!({ "frames": after_frames }))?;
    }
    Ok(ToolOutput::Json(json!({
        "tapped": buttons, "press_frames": press_frames, "after_frames": after_frames, "state": "frozen"
    })))
}

/// 여러 탭을 한 콜에 순차로(메뉴 네비게이션 왕복 절감). steps의 각 원소가 한 탭의 버튼셋이다.
/// 예: [["down"],["down"],["a"]] = down·down·a 세 탭. 전부 frozen에서 결정론적. 호출 후 frozen 유지.
pub fn tap_sequence(
    link: &mut dyn EmulatorLink,
    port: u64,
    steps: &[Vec<String>],
    press_frames: u64,
) -> Result<ToolOutput, LinkError> {
    link.call("pause", json!({}))?; // 멱등
    for step in steps {
        one_tap(link, port, step, press_frames)?;
    }
    Ok(ToolOutput::Json(json!({
        "sequence_len": steps.len(), "press_frames": press_frames, "state": "frozen"
    })))
}

/// 버튼을 누른 채 frozen으로 프레임을 진행하며 watch 메모리를 보고, 값이 바뀌면 멈추고 뗀다
/// (실시간 타일 이동을 결정론적으로 — 좌표가 바뀔 때까지 방향 hold). max_frames까지 안 바뀌면
/// changed:false. 반환 {changed, frames, before, after}. 입력 효과 피드백·필드 이동에 쓴다.
#[allow(clippy::too_many_arguments)]
pub fn hold_until(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    memory_type: &str,
    address: u64,
    length: u64,
    max_frames: u64,
) -> Result<ToolOutput, LinkError> {
    let read = |link: &mut dyn EmulatorLink| -> Result<String, LinkError> {
        let r = link.call(
            "read_memory",
            json!({ "memory_type": memory_type, "address": address, "length": length }),
        )?;
        Ok(r.get("hex")
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string())
    };
    link.call("pause", json!({}))?; // 멱등
    link.call("set_input", json!({ "port": port, "buttons": buttons }))?;
    let before = read(link)?;
    let mut frames = 0u64;
    let mut after = before.clone();
    let mut changed = false;
    while frames < max_frames {
        link.call("step", json!({ "frames": 1 }))?;
        frames += 1;
        after = read(link)?;
        if after != before {
            changed = true;
            break;
        }
    }
    let empty: [String; 0] = [];
    link.call("set_input", json!({ "port": port, "buttons": empty }))?; // 해제
    link.call("step", json!({ "frames": 1 }))?;
    Ok(ToolOutput::Json(json!({
        "changed": changed, "frames": frames, "before": before, "after": after, "state": "frozen"
    })))
}

pub fn save_state(link: &mut dyn EmulatorLink, path: &str) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("save_state", json!({ "path": path }))?,
    ))
}

pub fn load_state(link: &mut dyn EmulatorLink, path: &str) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("load_state", json!({ "path": path }))?,
    ))
}

/// n프레임을 실시간 진행한다. frozen이면 어댑터 run_frames 핸들러가 원자적으로 resume+advance하므로,
/// 여기서 별도 resume을 보내지 않는다 — 별도 resume은 명령 도착 전 free-run으로 watch_register/BP를 조기
/// 소진(one-shot)시키는 레이스다. 원자 resume이면 derail이 run_frames 구간에서 발화해 interrupted로 반환된다.
pub fn run_frames(link: &mut dyn EmulatorLink, n: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("run_frames", json!({ "n": n }))?,
    ))
}

pub fn pause(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("pause", json!({}))?))
}

/// frozen에서 N프레임 진행 후 재정지.
pub fn step(link: &mut dyn EmulatorLink, frames: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("step", json!({ "frames": frames }))?,
    ))
}

/// frozen에서 N개 CPU 명령 진행 후 재정지(derail 직전을 1명령씩 좁히기). 프레임 step과 분리된 도구.
pub fn step_instructions(link: &mut dyn EmulatorLink, count: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call(
        "step",
        json!({ "frames": count, "unit": "instructions" }),
    )?))
}

pub fn resume(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("resume", json!({}))?))
}

pub fn reset(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("reset", json!({}))?))
}

/// 메모리 접근 브레이크포인트(kind=exec/read/write). pc_min/pc_max를 주면 그 접근을 일으킨 명령의
/// 24비트 pc가 [pc_min,pc_max]일 때만 break한다(정상 push 등 노이즈 제거).
#[allow(clippy::too_many_arguments)]
pub fn set_breakpoint(
    link: &mut dyn EmulatorLink,
    kind: &str,
    memory_type: &str,
    start: u64,
    end: u64,
    pause_on_hit: bool,
    auto_savestate: bool,
    pc_min: Option<u64>,
    pc_max: Option<u64>,
    value: Option<u64>,
    value_mask: Option<u64>,
    value_len: Option<u64>,
    snapshot: &[String],
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({
        "kind": kind, "memory_type": memory_type, "start": start, "end": end,
        "pause_on_hit": pause_on_hit, "auto_savestate": auto_savestate,
    });
    // 히트 순간 atomic 캡처할 메모리 스펙(mt:addr:len 리스트). 어댑터가 record_hit에서 레지스터와 함께 잡는다.
    if !snapshot.is_empty() {
        params["snapshot"] = json!(snapshot);
    }
    if let Some(v) = pc_min {
        params["pc_min"] = json!(v);
    }
    if let Some(v) = pc_max {
        params["pc_max"] = json!(v);
    }
    // 값-조건(read/write BP): 접근 값이 (value & value_mask)와 같을 때만 break. value_len=비교 바이트(1~4).
    if let Some(v) = value {
        params["value"] = json!(v);
    }
    if let Some(v) = value_mask {
        params["value_mask"] = json!(v);
    }
    if let Some(v) = value_len {
        params["value_len"] = json!(v);
    }
    Ok(ToolOutput::Json(link.call("set_breakpoint", params)?))
}

/// 디스어셈블: address부터 count개 명령을 명령 단위로 디코드한다(SH-2=Mednafen, 65816=Mesen).
/// 코어가 가변 길이 명령 경계를 정확히 맞추므로 raw 바이트 수동 디코드가 불필요하다. BP 히트 PC
/// 주변을 바로 읽어 "어떤 명령이 이 접근을 일으켰나"를 즉시 본다. 반환 [{addr, text}].
pub fn disassemble(
    link: &mut dyn EmulatorLink,
    address: u64,
    count: u64,
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "address": address, "count": count });
    Ok(ToolOutput::Json(link.call("disassemble", params)?))
}

/// 레지스터 범위 워치: register가 허용 범위 [min,max]를 벗어나는 명령에서 freeze한다(SP 폭주 등
/// derail을 그 명령에서 포착). register는 get_state의 cpu.* 이름(sp/pc/k/a/x/y/ps…). 매 명령 검사라
/// 느리니(실측 ~1fps) hunting 전용으로 쓰고 끝나면 clear한다.
pub fn watch_register(
    link: &mut dyn EmulatorLink,
    register: &str,
    min: u64,
    max: u64,
    pause_on_hit: bool,
) -> Result<ToolOutput, LinkError> {
    let params =
        json!({ "register": register, "min": min, "max": max, "pause_on_hit": pause_on_hit });
    Ok(ToolOutput::Json(link.call("watch_register", params)?))
}

pub fn clear_breakpoint(link: &mut dyn EmulatorLink, id: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("clear_breakpoint", json!({ "id": id }))?,
    ))
}

pub fn list_breakpoints(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("list_breakpoints", json!({}))?))
}

pub fn clear_all_breakpoints(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("clear_all_breakpoints", json!({}))?,
    ))
}

pub fn poll_events(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("poll_events", json!({}))?))
}

/// 실행추적 on/off. 켜면 매 명령 콜백이 콜스택·트레이스를 유지한다(느림 — 크래시 추적 hunting 전용).
pub fn set_trace(link: &mut dyn EmulatorLink, enabled: bool) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("set_trace", json!({ "enabled": enabled }))?,
    ))
}

/// 최근 count개 실행 명령을 시간순으로(트레이스 링버퍼). set_trace(true)가 선행돼야 함.
pub fn get_trace(link: &mut dyn EmulatorLink, count: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("get_trace", json!({ "count": count }))?,
    ))
}

/// 현재 콜스택(JSR/JSL 호출지 체인, 바깥→안)을 반환한다. set_trace(true)가 선행돼야 함.
pub fn call_stack(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("call_stack", json!({}))?))
}

/// break_on_reset: 게임이 리셋 핸들러($00:FFFC 벡터)를 실행하면 freeze(워치독 리셋·하드 크래시→리셋
/// 자동 감지). enabled로 on/off. 단일 주소 exec BP라 빠르다(per-instruction 아님).
pub fn break_on_reset(link: &mut dyn EmulatorLink, enabled: bool) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("break_on_reset", json!({ "enabled": enabled }))?,
    ))
}

pub fn dump_memory(link: &mut dyn EmulatorLink, dir: &str) -> Result<ToolOutput, LinkError> {
    let regions = link.call("dump_memory", json!({ "path": dir }))?;
    // 상태(레지스터/DMA/PPU) 스냅샷도 같은 디렉토리에 기록(교차-ROM 키-값 디프 입력).
    // 교차-ROM에서는 frozen 앵커 지점에서 덤프해야 두 호출이 일관된다.
    let state = link.call("get_state", json!({}))?;
    let state_map = state.get("state").cloned().unwrap_or(state.clone());
    let path = Path::new(dir).join("state.json");
    std::fs::write(&path, serde_json::to_string(&state_map).unwrap_or_default())
        .map_err(|e| LinkError::Protocol(format!("state.json 쓰기 실패: {e}")))?;
    Ok(ToolOutput::Json(regions))
}

pub fn screenshot(
    link: &mut dyn EmulatorLink,
    save_path: Option<&Path>,
) -> Result<ToolOutput, LinkError> {
    let result = link.call("screenshot", json!({}))?;
    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinkError::Protocol("screenshot 응답에 png_base64 없음".into()))?
        .to_string();

    let saved_path = match save_path {
        Some(p) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| LinkError::Protocol(format!("base64 디코드 실패: {e}")))?;
            std::fs::write(p, bytes)
                .map_err(|e| LinkError::Protocol(format!("스크린샷 저장 실패: {e}")))?;
            Some(p.to_string_lossy().to_string())
        }
        None => None,
    };
    Ok(ToolOutput::Image {
        png_base64: b64,
        saved_path,
    })
}
