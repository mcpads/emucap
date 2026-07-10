use std::path::{Path, PathBuf};

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

/// Insert an optional `cpu` selector into a params object. Single-core adapters ignore it;
/// the NDS bridge routes it to the ARM9/ARM7 connection (`arm9`/`arm7`, or `both` for resume).
fn with_cpu(params: &mut serde_json::Value, cpu: Option<&str>) {
    if let (Some(cpu), Some(obj)) = (cpu, params.as_object_mut()) {
        obj.insert("cpu".into(), json!(cpu));
    }
}

pub fn get_state(
    link: &mut dyn EmulatorLink,
    groups: &[String],
    cpu: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let mut params = if groups.is_empty() {
        json!({})
    } else {
        json!({ "groups": groups })
    };
    with_cpu(&mut params, cpu);
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

pub fn dismiss_failure(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("dismiss_failure", json!({}))?,
    ))
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

/// 하단 터치스크린(256×192)을 (x,y)에서 터치한다 — release면 뗀다, frames>0이면 그만큼 누른 뒤 자동으로 뗀다(탭),
/// 둘 다 없으면 다음 touch까지 hold. 어댑터에 그대로 포워딩(터치스크린 있는 시스템만 동작; status.methods 정본).
pub fn touch(
    link: &mut dyn EmulatorLink,
    port: u64,
    x: Option<u64>,
    y: Option<u64>,
    frames: Option<u64>,
    release: bool,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "port": port });
    if release {
        params["release"] = json!(true);
    } else {
        if let Some(x) = x {
            params["x"] = json!(x);
        }
        if let Some(y) = y {
            params["y"] = json!(y);
        }
        if let Some(f) = frames {
            params["frames"] = json!(f);
        }
    }
    Ok(ToolOutput::Json(link.call("touch", params)?))
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
    // press step이 실패(timeout 등)하면 `?`로 바로 반환돼 버튼이 눌린 채 남는다 — 에러를 전파하기 전에
    // best-effort로 입력을 해제한다(에뮬 상태가 눌린 채 계속 진행하는 것 방지).
    if let Err(e) = link.call("step", json!({ "frames": press_frames.max(1) })) {
        let _ = link.call("set_input", json!({ "port": port, "buttons": empty }));
        return Err(e);
    }
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

/// tap_sequence 총 프레임 상한. per-field cap(steps 4096 × press_frames 1M)을 각각 통과한 유효 요청이
/// 곱으로 팽창해 SharedLink 뮤텍스를 쥔 채 수십억 프레임을 도는 것을 막는 집계 상한(args.rs MAX_FRAME_ARG
/// 동취지).
const MAX_TAP_SEQUENCE_FRAMES: u64 = 1_000_000;

/// 여러 탭을 한 콜에 순차로(메뉴 네비게이션 왕복 절감). steps의 각 원소가 한 탭의 버튼셋이다.
/// 예: [["down"],["down"],["a"]] = down·down·a 세 탭. 전부 frozen에서 결정론적. 호출 후 frozen 유지.
pub fn tap_sequence(
    link: &mut dyn EmulatorLink,
    port: u64,
    steps: &[Vec<String>],
    press_frames: u64,
) -> Result<ToolOutput, LinkError> {
    // 탭 하나 = press_frames + 해제 1 + 해제에지 1. 집계가 상한을 넘으면 실행 전에 거부한다(뮤텍스 점유 폭주 방지).
    let per_tap = press_frames.saturating_add(2);
    let total = (steps.len() as u64).saturating_mul(per_tap);
    if total > MAX_TAP_SEQUENCE_FRAMES {
        return Err(LinkError::Emulator {
            kind: "bad_params".into(),
            message: format!(
                "tap_sequence 총 프레임 {total}(steps {} × {per_tap})이 상한 {MAX_TAP_SEQUENCE_FRAMES} 초과 — 나눠 호출하라",
                steps.len()
            ),
        });
    }
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
    // 코어 루프를 돌리되 성패 무관하게 입력을 해제한다 — step/read 에러로 중간 종료해도 버튼이 눌린 채
    // 남지 않게(에뮬 상태가 입력받은 채 진행 방지).
    let outcome: Result<(bool, u64, String, String), LinkError> = (|| {
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
        Ok((changed, frames, before, after))
    })();
    let empty: [String; 0] = [];
    let _ = link.call("set_input", json!({ "port": port, "buttons": empty })); // best-effort 해제
    let (changed, frames, before, after) = outcome?;
    link.call("step", json!({ "frames": 1 }))?; // 해제 에지
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

pub fn pause(link: &mut dyn EmulatorLink, cpu: Option<&str>) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    with_cpu(&mut params, cpu);
    Ok(ToolOutput::Json(link.call("pause", params)?))
}

/// frozen에서 N프레임 진행 후 재정지.
pub fn step(
    link: &mut dyn EmulatorLink,
    frames: u64,
    cpu: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "frames": frames });
    with_cpu(&mut params, cpu);
    Ok(ToolOutput::Json(link.call("step", params)?))
}

/// frozen에서 N개 CPU 명령 진행 후 재정지(derail 직전을 1명령씩 좁히기). 프레임 step과 분리된 도구.
pub fn step_instructions(
    link: &mut dyn EmulatorLink,
    count: u64,
    cpu: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "frames": count, "unit": "instructions" });
    with_cpu(&mut params, cpu);
    Ok(ToolOutput::Json(link.call("step", params)?))
}

pub fn resume(link: &mut dyn EmulatorLink, cpu: Option<&str>) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    with_cpu(&mut params, cpu);
    Ok(ToolOutput::Json(link.call("resume", params)?))
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
/// watch_register 자동해제 예산 상한(명령 수). 이보다 크면 거부한다 — 무기한에 가까운 예산은 매 명령
/// getState 플러드로 emu 스레드를 오래 굶긴다. 기본(어댑터의 1M)의 여러 배까지 확장은 허용한다.
const MAX_WATCH_INSTRUCTIONS: u64 = 50_000_000;

pub fn watch_register(
    link: &mut dyn EmulatorLink,
    register: &str,
    min: u64,
    max: u64,
    pause_on_hit: bool,
    max_instructions: Option<u64>,
) -> Result<ToolOutput, LinkError> {
    let mut params =
        json!({ "register": register, "min": min, "max": max, "pause_on_hit": pause_on_hit });
    if let Some(budget) = max_instructions {
        if budget > MAX_WATCH_INSTRUCTIONS {
            return Err(LinkError::Emulator {
                kind: "bad_params".into(),
                message: format!(
                    "watch_register max_instructions {budget}이 상한 {MAX_WATCH_INSTRUCTIONS} 초과"
                ),
            });
        }
        params["max_instructions"] = json!(budget);
    }
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

/// 표준 메모리 리전(.bin+regions.json)과 상태 스냅샷(state.json)을 `dir`에 원자적으로 배치한다.
///
/// 브리지 덤프(리전 파일)와 호스트가 쓰는 state.json은 두 단계라, 예전처럼 `dir`에 바로 쓰면
/// 리전 파일을 배치한 뒤 state.json 쓰기가 실패할 때 직전의 온전한 덤프가 파괴되고(롤백 불가)
/// state.json 없는 덤프가 남는다. 그래서 리전 파일 + state.json 전부를 형제(sibling) 스테이징
/// 디렉토리에 모은 뒤, 둘 다 성공했을 때만 `dir`로 원자 스왑한다 — 어느 단계가 실패하든 직전 덤프는
/// 바이트 그대로 보존되고 스테이징 잔재도 남기지 않는다(모든 어댑터 공통, 어댑터 무관).
pub fn dump_memory(link: &mut dyn EmulatorLink, dir: &str) -> Result<ToolOutput, LinkError> {
    let dest = Path::new(dir);
    // 요청 경로에 이미 심링크나 (디렉토리가 아닌) 일반 파일이 있으면 스테이징·브리지 덤프 전에
    // 거부한다 — 원자 스왑/폴백이 그 파일을 숨은 이름으로 밀어내 요청 경로에서 사라지게 하는 것을
    // 막는다(fail-fast, replace_dir와 동일 가드).
    ensure_replaceable_dir(dest).map_err(|e| {
        LinkError::Protocol(format!("덤프 경로가 교체 가능한 디렉토리가 아님: {e}"))
    })?;
    let staging = dump_sibling(dest, "dump-staging")
        .map_err(|e| LinkError::Protocol(format!("덤프 스테이징 경로 실패: {e}")))?;
    let staging_str = staging
        .to_str()
        .ok_or_else(|| LinkError::Protocol("덤프 스테이징 경로가 UTF-8이 아님".into()))?
        .to_string();

    // 스테이징에 리전 파일 + state.json을 모은다. 실패하면 스테이징을 버리고 `dir`은 건드리지 않는다.
    let build = (|| -> Result<Value, LinkError> {
        std::fs::create_dir_all(&staging)
            .map_err(|e| LinkError::Protocol(format!("스테이징 디렉토리 생성 실패: {e}")))?;
        let regions = link.call("dump_memory", json!({ "path": staging_str }))?;
        // 상태(레지스터/DMA/PPU) 스냅샷도 같은 디렉토리에 기록(교차-ROM 키-값 디프 입력).
        // 교차-ROM에서는 frozen 앵커 지점에서 덤프해야 두 호출이 일관된다.
        let state = link.call("get_state", json!({}))?;
        let state_map = state.get("state").cloned().unwrap_or(state.clone());
        std::fs::write(
            staging.join("state.json"),
            serde_json::to_string(&state_map).unwrap_or_default(),
        )
        .map_err(|e| LinkError::Protocol(format!("state.json 쓰기 실패: {e}")))?;
        Ok(regions)
    })();

    let regions = match build {
        Ok(regions) => regions,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    // 완성된 스테이징을 `dir`로 원자 스왑(직전 덤프는 스왑 성공 시에만 교체·실패 시 롤백).
    if let Err(e) = replace_dir(&staging, dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(LinkError::Protocol(format!("덤프 배치(스왑) 실패: {e}")));
    }

    // 브리지가 돌려준 path는 스테이징 경로이므로, 호출자가 요청한 `dir`로 정정해 보고한다.
    let mut regions = regions;
    if let Some(obj) = regions.as_object_mut() {
        obj.insert("path".into(), json!(dir));
    }
    Ok(ToolOutput::Json(regions))
}

/// `dst`의 형제 경로(같은 부모라 이후 `rename`이 한 파일시스템 내라 원자적)를 `label`·PID·나노초로
/// 고유하게 만든다. 부모 디렉토리가 없으면 에러.
fn dump_sibling(dst: &Path, label: &str) -> std::io::Result<PathBuf> {
    let parent = dst.parent().ok_or_else(|| {
        std::io::Error::other(format!(
            "dump path {} has no parent directory to stage under",
            dst.display()
        ))
    })?;
    let name = dst.file_name().and_then(|n| n.to_str()).unwrap_or("dump");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.{label}.{}.{nanos}", std::process::id())))
}

/// `dst`가 원자 스왑으로 안전히 교체 가능한 대상인지 확인한다 — 없으면(새로 생성) 또는 디렉토리면 OK,
/// 심링크거나 (디렉토리가 아닌) 일반 파일 등 기존 항목이면 거부한다. src/launch의 copy_dir_replace와
/// 같은 가드로, 사용자의 파일이 덤프 경로로 밀려나 요청 경로에서 사라지는 것을 막는다.
fn ensure_replaceable_dir(dst: &Path) -> std::io::Result<()> {
    if crate::launch::is_symlink(dst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "destination is a symlink, refusing to replace: {}",
                dst.display()
            ),
        ));
    }
    if dst.exists() && !dst.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination is not a directory: {}", dst.display()),
        ));
    }
    Ok(())
}

/// 완성된 스테이징 덤프 `staging`을 `dst`로 배치한다.
/// - `dst`가 없으면 단일 `rename`(같은 파일시스템이라 원자적).
/// - `dst`가 있고 OS가 단일-syscall 교환을 지원하면(Linux `renameat2(RENAME_EXCHANGE)`,
///   macOS `renamex_np(RENAME_SWAP)`) `staging`↔`dst`를 한 syscall로 맞바꾼 뒤, 이제 구 덤프를
///   담은 `staging`을 제거한다 — 어느 순간에 죽어도 `dst`는 항상 온전한 덤프(구본 또는 신본)를 가리킨다.
/// - 교환 프리미티브가 없거나 파일시스템이 거부하면 2-rename 폴백(백업→rename→성공 시 백업 삭제,
///   실패 시 롤백). 폴백은 두 rename 사이 크래시에 `dst`가 잠깐 없을 수 있다(구 덤프는 백업에 보존).
fn replace_dir(staging: &Path, dst: &Path) -> std::io::Result<()> {
    // dst가 심링크거나 (디렉토리가 아닌) 기존 항목이면 거부한다 — 그렇지 않으면 원자 스왑/폴백이
    // 사용자의 파일을 요청 경로에서 밀어내(숨은 이름으로 이동) 조용히 사라지게 한다. copy_dir_replace와
    // 같은 가드로 어느 호출자가 부르든(dump_memory 등) 파일 대상을 절대 밀어내지 않게 한다.
    ensure_replaceable_dir(dst)?;
    if !dst.exists() {
        return std::fs::rename(staging, dst);
    }
    // 지원 OS: 단일 syscall 원자 교환. 성공 후 `staging`은 구 덤프를 담으므로 제거한다.
    if try_exchange(staging, dst)? {
        let _ = std::fs::remove_dir_all(staging);
        return Ok(());
    }
    replace_dir_fallback(staging, dst)
}

/// 교환 프리미티브가 없는 플랫폼/파일시스템용 2-rename 폴백. 두 rename 사이 크래시에 `dst`가 잠깐
/// 비는 창이 있으나(구 덤프는 백업에 있음), 직전의 온전한 덤프가 반쯤 교체된 채 남지는 않는다.
fn replace_dir_fallback(staging: &Path, dst: &Path) -> std::io::Result<()> {
    let backup = dump_sibling(dst, "dump-old")?;
    std::fs::rename(dst, &backup)?;
    match std::fs::rename(staging, dst) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&backup, dst);
            Err(e)
        }
    }
}

/// 교환 프리미티브 syscall의 errno가 "이 커널/파일시스템이 지원 안 함"이라 2-rename 폴백으로 강등해야
/// 하는지. 커널 미구현(ENOSYS)·플래그 거부(EINVAL/ENOTSUP)를 폴백으로 본다(그 외 errno는 경로 소멸 등
/// 진짜 I/O 실패). macOS·Linux가 같은 errno 계열을 폴백해 어느 한 플랫폼만 좁게 하드-실패하지 않게 한다.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_unsupported_exchange_errno(raw: Option<i32>) -> bool {
    matches!(raw, Some(libc::ENOSYS | libc::EINVAL | libc::ENOTSUP))
}

/// 두 경로 `a`·`b`(둘 다 존재)를 단일 syscall로 원자 교환한다. 성공하면 `Ok(true)`, 이
/// 플랫폼/파일시스템에 교환 프리미티브가 없으면 `Ok(false)`(호출자 폴백), 그 외 I/O 실패는 `Err`.
#[cfg(target_os = "macos")]
fn try_exchange(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    // RENAME_SWAP: a↔b를 원자적으로 맞바꾼다(둘 다 존재해야). 성공하면 a는 옛 b, b는 옛 a를 담는다.
    let rc = unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    // 파일시스템/커널이 RENAME_SWAP 미지원 → 폴백. 그 외(경로 소멸 등)는 진짜 실패.
    if is_unsupported_exchange_errno(err.raw_os_error()) {
        Ok(false)
    } else {
        Err(err)
    }
}

#[cfg(target_os = "linux")]
fn try_exchange(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    // RENAME_EXCHANGE: a↔b를 원자적으로 맞바꾼다(둘 다 존재해야).
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    // 커널(구커널 ENOSYS)·파일시스템(플래그 거부 EINVAL/ENOTSUP)이 미지원 → 폴백. 그 외는 진짜 실패.
    if is_unsupported_exchange_errno(err.raw_os_error()) {
        Ok(false)
    } else {
        Err(err)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn try_exchange(_a: &Path, _b: &Path) -> std::io::Result<bool> {
    Ok(false) // 교환 프리미티브 없음 → 호출자가 2-rename 폴백
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

#[cfg(test)]
mod tests {
    use super::{tap_sequence, watch_register, LinkError};
    use crate::live::link::FakeLink;
    use serde_json::json;

    #[test]
    fn watch_register_rejects_over_budget() {
        // 과대 max_instructions는 매 명령 getState 플러드를 오래 돌려 emu 스레드를 굶긴다 — 실행 전 거부.
        let mut link = FakeLink::ok(json!({ "id": 1 }));
        let r = watch_register(&mut link, "sp", 0, 0xffff, true, Some(u64::MAX));
        assert!(
            matches!(r, Err(LinkError::Emulator { ref kind, .. }) if kind == "bad_params"),
            "과대 max_instructions는 bad_params로 거부해야: {r:?}"
        );
        let mut link2 = FakeLink::ok(json!({ "id": 1 }));
        assert!(
            watch_register(&mut link2, "sp", 0, 0xffff, true, Some(1000)).is_ok(),
            "상한 이내 예산은 통과해야"
        );
    }

    #[test]
    fn tap_sequence_rejects_over_aggregate_budget() {
        // per-field cap을 통과해도(steps ≤ 4096, press_frames ≤ 1M) 곱이 상한(1M)을 넘으면 실행 전에
        // 거부해야 한다 — 유효 요청이 뮤텍스를 쥔 채 수십억 프레임으로 팽창하는 것 방지.
        let mut link = FakeLink::ok(json!({}));
        let steps: Vec<Vec<String>> = vec![vec!["a".to_string()]; 4000];
        let r = tap_sequence(&mut link, 0, &steps, 1000); // 4000 × 1002 ≈ 4M > 1M
        assert!(
            matches!(r, Err(LinkError::Emulator { ref kind, .. }) if kind == "bad_params"),
            "집계 예산 초과는 bad_params로 거부해야: {r:?}"
        );
        assert!(
            link.last_method.is_none(),
            "예산 초과는 어떤 링크 호출(pause 포함)도 하기 전에 거부해야"
        );
    }

    #[test]
    fn tap_sequence_accepts_within_budget() {
        let mut link = FakeLink::ok(json!({}));
        let steps: Vec<Vec<String>> = vec![vec!["a".to_string()]; 10];
        assert!(
            tap_sequence(&mut link, 0, &steps, 2).is_ok(),
            "예산 이내는 통과해야"
        );
    }

    fn dir_with(path: &std::path::Path, marker: &[u8]) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("marker"), marker).unwrap();
    }

    #[test]
    fn replace_dir_into_absent_dst_is_single_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dump");
        let staging = tmp.path().join(".dump.staging");
        dir_with(&staging, b"new");
        super::replace_dir(&staging, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
        assert!(!staging.exists(), "이동 후 staging은 사라져야");
    }

    #[test]
    fn replace_dir_publishes_new_dump_over_existing() {
        // end-to-end(교환 또는 폴백): 기존 덤프 위에 새 스테이징 배치 → dst엔 신본, staging 제거.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dump");
        dir_with(&dst, b"old");
        let staging = tmp.path().join(".dump.staging");
        dir_with(&staging, b"new");
        super::replace_dir(&staging, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
        assert!(!staging.exists(), "스왑/이동 후 staging은 제거되어야");
        // 백업 잔재(.dump.dump-old.*)가 남지 않아야.
        let leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("dump-old"));
        assert!(!leftovers, "성공 시 백업/구덤프 잔재가 없어야");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unsupported_exchange_errnos_fall_back_not_hard_fail() {
        // 교환 프리미티브를 파일시스템/커널이 거부하는 errno 계열은 2-rename 폴백으로 강등해야 한다.
        // macOS 아암이 ENOTSUP만 폴백하던 회귀: EINVAL을 내는 파일시스템이면 덤프 publish가 하드-실패했다.
        use super::is_unsupported_exchange_errno as f;
        for e in [libc::ENOSYS, libc::EINVAL, libc::ENOTSUP] {
            assert!(f(Some(e)), "미지원 errno {e}는 폴백해야");
        }
        assert!(!f(Some(libc::ENOENT)), "경로 소멸(ENOENT)은 진짜 실패");
        assert!(!f(None), "errno 없음은 진짜 실패");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn try_exchange_swaps_contents_atomically() {
        // 이 플랫폼의 원자 교환 프리미티브가 두 디렉토리 내용을 한 번에 맞바꾼다(교환 경로 검증).
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        dir_with(&a, b"A");
        dir_with(&b, b"B");
        assert!(
            super::try_exchange(&a, &b).unwrap(),
            "지원 플랫폼(linux/macos)은 교환에 성공해야"
        );
        assert_eq!(std::fs::read(a.join("marker")).unwrap(), b"B");
        assert_eq!(std::fs::read(b.join("marker")).unwrap(), b"A");
    }

    #[test]
    fn replace_dir_refuses_file_destination() {
        // 요청 경로에 사용자의 일반 파일이 있으면 원자 스왑/폴백이 그 파일을 숨은 이름으로 밀어내
        // (요청 경로에서 사라지게) 하면 안 된다 — 거부하고 파일을 바이트 그대로 둔다.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dump");
        std::fs::write(&dst, b"user-file").unwrap();
        let staging = tmp.path().join(".dump.staging");
        dir_with(&staging, b"new");
        let err = super::replace_dir(&staging, &dst).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"user-file",
            "거부 시 사용자 파일은 그대로여야"
        );
        assert!(staging.exists(), "거부 시 staging은 밀려나지 않아야");
    }

    #[cfg(unix)]
    #[test]
    fn replace_dir_refuses_symlink_destination() {
        // dst가 심링크면(파일이든 디렉토리든) 거부한다 — 스왑/폴백이 링크를 교체하거나 대상을 밀어내지
        // 않게. copy_dir_replace와 같은 가드.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        dir_with(&real, b"real");
        let dst = tmp.path().join("dump");
        std::os::unix::fs::symlink(&real, &dst).unwrap();
        let staging = tmp.path().join(".dump.staging");
        dir_with(&staging, b"new");
        let err = super::replace_dir(&staging, &dst).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            std::fs::symlink_metadata(&dst)
                .unwrap()
                .file_type()
                .is_symlink(),
            "심링크는 보존되어야"
        );
        assert_eq!(
            std::fs::read(real.join("marker")).unwrap(),
            b"real",
            "심링크 대상 디렉토리는 보존되어야"
        );
    }

    #[test]
    fn dump_memory_refuses_file_destination() {
        // 요청 경로가 일반 파일이면 브리지 덤프·스테이징 전에 거부하고 파일을 보존한다(fail-fast) —
        // 어댑터를 호출하지 않는다.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dump");
        std::fs::write(&dst, b"user-file").unwrap();
        let mut link = FakeLink::ok(json!({}));
        let err = super::dump_memory(&mut link, dst.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(err, LinkError::Protocol(_)),
            "가드는 Protocol 에러"
        );
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"user-file",
            "거부 시 사용자 파일은 그대로여야"
        );
        assert!(
            link.last_method.is_none(),
            "가드가 브리지 dump_memory 호출 전에 거부해야"
        );
    }

    #[test]
    fn replace_dir_fallback_swaps_over_existing() {
        // 폴백(2-rename) 경로를 직접 검증 — 교환 미지원 플랫폼/파일시스템의 동작.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dump");
        dir_with(&dst, b"old");
        let staging = tmp.path().join(".dump.staging");
        dir_with(&staging, b"new");
        super::replace_dir_fallback(&staging, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
        assert!(!staging.exists(), "폴백 성공 후 staging 제거");
        let leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("dump-old"));
        assert!(!leftovers, "폴백 성공 시 백업이 제거되어야");
    }
}
