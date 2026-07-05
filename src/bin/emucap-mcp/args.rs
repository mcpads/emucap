use schemars::JsonSchema;
use serde::Deserialize;

/// 숫자 입력 — JSON 정수 또는 16진 문자열을 모두 받는다. 주소·값을 16진으로 생각하는 ROM 해킹에서
/// 10진 변환 실수를 없애려 도입. 허용: 정수(8471), "0x2117"/"0X2117", "$2117", "8471", 밑줄("0x80_420b").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Num(pub(crate) u64);
impl Num {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

fn parse_num_str(s: &str) -> Result<u64, String> {
    // 정본은 lib(emucap::numparse) — MCP와 CLI가 같은 규칙으로 0x/$ 16진을 받게 한다(#45).
    emucap::numparse::parse_num_str(s)
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Num;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("정수 또는 '0x'/'$' 접두 16진 문자열")
            }
            fn visit_u64<E>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Num, E> {
                u64::try_from(v)
                    .map(Num)
                    .map_err(|_| E::custom("음수는 불가"))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Num, E> {
                if v >= 0.0 && v.fract() == 0.0 {
                    Ok(Num(v as u64))
                } else {
                    Err(E::custom("정수만 허용"))
                }
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Num, E> {
                parse_num_str(s).map(Num).map_err(E::custom)
            }
            fn visit_string<E: serde::de::Error>(self, s: String) -> Result<Num, E> {
                self.visit_str(&s)
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for Num {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Num".into()
    }
    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "10진 정수 또는 '0x'/'$' 접두 16진 문자열(예: 8471, \"0x2117\", \"$2117\")",
            "anyOf": [ { "type": "integer", "minimum": 0 }, { "type": "string" } ]
        })
    }
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ReadMemoryArgs {
    /// 메모리 타입 식별자. 유효한 이름은 연결된 시스템마다 다르니 `status.memory_types`(정본)를 본다(per-system 이름·의미는 각 `adapters/*/README.md`).
    pub(crate) memory_type: String,
    /// 시작 주소(10진 또는 '0x'/'$' 16진, 해당 메모리 타입의 오프셋)
    pub(crate) address: Num,
    /// 읽을 바이트 수(10진 또는 16진)
    pub(crate) length: Num,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ProbeArgs {
    /// 베이스 세이브스테이트 경로. 어댑터가 load_state한 뒤 frame만큼 진행하고 타깃을 읽는다.
    pub(crate) state: String,
    /// 베이스 상태에서 진행할 프레임 수(deferred — 상한 적용). 과대값이 프로브를 딴 세계로 진행시켜
    /// 링크를 데드라인까지 붙잡는 것을 막는다.
    #[serde(deserialize_with = "deser_frame_count")]
    pub(crate) frame: u64,
    /// 읽을 메모리 타입(read_memory와 동일 식별자).
    pub(crate) memory_type: String,
    /// 읽을 시작 주소(10진 또는 '0x'/'$' 16진).
    pub(crate) address: Num,
    /// 읽을 바이트 수(10진 또는 16진).
    pub(crate) length: Num,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DisassembleArgs {
    /// 디스어셈블 시작 주소(10진 또는 '0x'/'$' 16진). CPU/ISA와 disassemble 지원 여부는 연결된 시스템에 따르며 `status.methods`·어댑터 README가 정본. 반환 [{addr,text,bytes}].
    pub(crate) address: Num,
    /// 디코드할 명령 개수(기본 8, 최대 256)
    #[serde(default = "default_disas_count")]
    pub(crate) count: u64,
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
fn default_disas_count() -> u64 {
    8
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct WriteMemoryArgs {
    pub(crate) memory_type: String,
    /// 시작 주소(10진 또는 '0x'/'$' 16진)
    pub(crate) address: Num,
    /// 쓸 바이트(hex 문자열)
    pub(crate) hex: String,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct FindPatternArgs {
    /// 검색할 메모리 타입(read_memory와 동일 식별자). 선형 영역 권장: snesWorkRam, MD ram/vram/cram, PCE ram/vram0, PC-98 tvram/ram 등.
    pub(crate) memory_type: String,
    /// 찾을 바이트열(hex 문자열, 짝수 길이). 예: "4901" = 바이트 0x49,0x01
    pub(crate) hex: String,
    /// 검색 시작 오프셋(기본 0, 10진/'0x'/'$' 16진)
    #[serde(default)]
    pub(crate) start: Option<Num>,
    /// 검색 길이(바이트, 10진/16진). 생략 시 region 끝까지. 한 호출 스캔 상한은 백엔드별(빠른 read는 region 전체를 1콜로 스캔; 초과 시 truncated=true → start를 옮겨 청크). 거대 매치 목록은 output_path로 파일에 받는다.
    #[serde(default)]
    pub(crate) length: Option<Num>,
    /// 반환 매칭 최대 개수(기본 256)
    #[serde(default = "default_max_matches")]
    pub(crate) max_matches: u64,
    /// 정렬: 이 배수 오프셋의 매칭만(기본 1). 테이블/고정엔트리 검색에 유용
    #[serde(default = "one")]
    pub(crate) align: u64,
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
fn default_max_matches() -> u64 {
    256
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct InputArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct PressArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// 누른 채 진행할 프레임 수(입력 hold — 작은 상한). 과대값은 링크 deadline을 넘겨 MCP 포기 후에도
    /// 버튼이 눌린 채 남아 상태를 오염시키므로 deadline 안에 드는 상한을 건다.
    #[serde(deserialize_with = "deser_input_frames")]
    pub(crate) frames: u64,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct TouchArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// 하단 터치스크린 X(0-255). release가 아니면 필수.
    #[serde(default)]
    pub(crate) x: Option<u64>,
    /// 하단 터치스크린 Y(0-191). release가 아니면 필수.
    #[serde(default)]
    pub(crate) y: Option<u64>,
    /// 누른 채 진행할 프레임 수(탭); 생략 시 다음 touch까지 hold. 즉시 반환(입력 오버라이드 설정만).
    #[serde(default)]
    pub(crate) frames: Option<u64>,
    /// true면 터치를 뗀다(x,y 무시).
    #[serde(default)]
    pub(crate) release: bool,
}

fn two() -> u64 {
    2
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct TapArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// 누를 프레임 수(기본 2 — auto-repeat 미만의 짧은 탭으로 정확히 1칸/1회 이동). 입력 hold라 작은 상한.
    #[serde(default = "two", deserialize_with = "deser_input_frames")]
    pub(crate) press_frames: u64,
    /// 떼고 더 진행할 프레임 수(기본 0). >0이면 입력+관찰을 한 콜에(frozen 유지). 해제 후라 입력 hold 아님.
    #[serde(default, deserialize_with = "deser_frame_count")]
    pub(crate) after_frames: u64,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct TapSequenceArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// 각 원소가 한 탭의 버튼셋. 예: [["down"],["down"],["a"]] = 세 탭을 순차로
    #[serde(deserialize_with = "deser_tap_steps")]
    pub(crate) steps: Vec<Vec<String>>,
    #[serde(default = "two", deserialize_with = "deser_input_frames")]
    pub(crate) press_frames: u64,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct HoldUntilArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// 변화를 지켜볼 메모리(read_memory와 동일 식별자)
    pub(crate) memory_type: String,
    /// 주소(10진 또는 '0x'/'$' 16진)
    pub(crate) address: Num,
    pub(crate) length: Num,
    /// 안 바뀌면 멈출 상한 프레임(기본 300). 입력을 누른 채 진행하므로 입력 hold 상한(작게).
    #[serde(default = "three_hundred", deserialize_with = "deser_input_frames")]
    pub(crate) max_frames: u64,
}
fn three_hundred() -> u64 {
    300
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct PathArgs {
    pub(crate) path: String,
}

/// 프레임/명령 진행 인자의 상한(~60fps로 약 4.6시간분). 상한 없는 n·frames·count·max_frames는
/// deferred 명령을 사실상 무한 루프시켜 어댑터를 붙잡고, raw_call(및 그것이 쥔 SharedLink mutex)을
/// wedge한다. regression.rs의 MAX_REPLAY_FRAMES와 같은 취지의 상한 — 초과는 조용히 자르지
/// 않고(silent-wrong 금지) 에러로 드러낸다.
pub(crate) const MAX_FRAME_ARG: u64 = 1_000_000;

/// 프레임/명령 수 필드용 디시리얼라이저 — MAX_FRAME_ARG 초과를 거부한다. 값이 존재할 때만 호출되고
/// (serde default는 필드 부재 시 우회하므로 기본값은 상한 검사 없이 통과 — 모두 상한 이내).
fn deser_frame_count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_FRAME_ARG {
        return Err(serde::de::Error::custom(format!(
            "프레임/명령 수 {n}이 상한 {MAX_FRAME_ARG} 초과 — 더 작게 나눠 호출하라"
        )));
    }
    Ok(n)
}

/// tap 시퀀스 최대 스텝 수 — 한 MCP 콜이 무한 탭 시리즈로 팽창해 deferred 실행을 붙잡는 것을 막는다.
pub(crate) const MAX_TAP_STEPS: usize = 4096;

/// 입력을 누른 채 진행하는 deferred 명령(press/tap/hold)의 프레임 상한. run_frames/step(입력 없음)과 달리
/// 큰 값은 링크 deadline(300s)을 넘겨 — MCP가 포기해 timeout/drop한 뒤에도 어댑터가 버튼을 계속 눌러
/// 게임 상태를 오염시킨다(취소 경로도 없다). deadline 안에 드는 작은 상한으로 둔다(어떤 정상 입력 hold도
/// 이보다 훨씬 짧다 — 60fps에서 ~166초).
pub(crate) const MAX_INPUT_HOLD_FRAMES: u64 = 10_000;

fn deser_input_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_INPUT_HOLD_FRAMES {
        return Err(serde::de::Error::custom(format!(
            "입력 hold 프레임 {n}이 상한 {MAX_INPUT_HOLD_FRAMES} 초과 — 링크 deadline을 넘겨 MCP 포기 후에도 입력이 눌린 채 남는다"
        )));
    }
    Ok(n)
}

/// tap_sequence의 steps 길이 상한. deferred 데드라인이 총 벽시계를 이미 유한하게 하지만, 여기서도 초과를
/// 조용히 자르지 않고 에러로 드러낸다.
fn deser_tap_steps<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<String>>, D::Error> {
    let v = Vec::<Vec<String>>::deserialize(d)?;
    if v.len() > MAX_TAP_STEPS {
        return Err(serde::de::Error::custom(format!(
            "tap 시퀀스 스텝 수 {}가 상한 {MAX_TAP_STEPS} 초과 — 나눠 호출하라",
            v.len()
        )));
    }
    Ok(v)
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RunFramesArgs {
    #[serde(deserialize_with = "deser_frame_count")]
    pub(crate) n: u64,
}
fn one() -> u64 {
    1
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct StepArgs {
    #[serde(default = "one", deserialize_with = "deser_frame_count")]
    pub(crate) frames: u64,
    /// 멀티코어 백엔드에서 대상 CPU(예: NDS `arm9`/`arm7`). 생략 시 기본 코어. 단일코어는 무시.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct StepInstructionsArgs {
    /// 진행할 CPU 명령 수
    #[serde(default = "one", deserialize_with = "deser_frame_count")]
    pub(crate) count: u64,
    /// 멀티코어 백엔드에서 대상 CPU(예: NDS `arm9`/`arm7`). 생략 시 기본 코어. 단일코어는 무시.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}
/// pause/resume용 — 대상 CPU만.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct CpuArgs {
    /// 멀티코어 백엔드에서 대상 CPU. NDS: `arm9`(기본)·`arm7`·`both`(resume 전용, 레이스 프리런). 단일코어는 무시.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct BreakpointArgs {
    /// 종류: exec | read | write | nmi | irq | dma (nmi/irq/dma는 Mesen 전용). exec/read/write는 메모리 접근,
    /// nmi/irq는 인터럽트 진입 이벤트, dma는 $420B(MDMAEN) write 시 DMA 채널 스냅샷. SERVER_INSTRUCTIONS 참조
    pub(crate) kind: String,
    /// 메모리 타입(read_memory 참조). exec는 snesMemory(24비트 CPU버스), read/write는 보통 snesWorkRam
    pub(crate) memory_type: String,
    /// 범위 시작 주소(10진 또는 '0x'/'$' 16진). exec는 24비트(뱅크 포함) — 16비트 PC가 아님
    pub(crate) start: Num,
    /// 범위 끝 주소(단일 주소면 start와 같게)
    pub(crate) end: Num,
    /// 히트 시 freeze(status가 frozen이 됨). 교차-ROM 정렬 앵커에 사용
    #[serde(default)]
    pub(crate) pause_on_hit: bool,
    #[serde(default)]
    pub(crate) auto_savestate: bool,
    /// 선택 pc 조건(read/write/exec): 이 접근을 일으킨 명령의 pc가 [pc_min,pc_max]일 때만 break(정상 push 등 노이즈 제거). pc의 폭·의미는 연결된 시스템 CPU에 따르며 get_state의 `cpu.pc`와 같은 기준(어댑터 README가 정본). pc_max와 함께. (kind=dma에선 vram_addr 범위 필터로 재사용 — vmin/vmax)
    #[serde(default)]
    pub(crate) pc_min: Option<Num>,
    /// 선택 pc 조건 상한(pc_min과 함께). kind=dma에선 vram_addr 상한
    #[serde(default)]
    pub(crate) pc_max: Option<Num>,
    /// 선택 값 조건(read/write BP): 접근 값이 (value & value_mask)와 같을 때만 break(한 주소를 모든 코드가 거쳐갈 때 특정 값만 격리). kind=dma에선 dest 필터(0x18/0x19 VRAM·0x04 OAM·0x22 CGRAM)로 재사용
    #[serde(default)]
    pub(crate) value: Option<Num>,
    /// 값 조건 마스크(기본 전 비트). 특정 비트만 비교할 때
    #[serde(default)]
    pub(crate) value_mask: Option<Num>,
    /// 값 비교 바이트 수(1~4, 기본 1). 16비트 워드면 2
    #[serde(default)]
    pub(crate) value_len: Option<Num>,
    /// 히트 순간 atomic 캡처할 메모리 리스트("memory_type:address:length", 예 "snesWorkRam:0x68:3").
    /// freeze 후 read 사이 명령단위 드리프트로 ZP 등을 못 잡는 문제 해결 — 이벤트의 snapshot/regs에 히트
    /// 시점 상태가 보존된다(drift·deadman 무관). exec/read/write BP에 적용(Mesen)
    #[serde(default)]
    pub(crate) snapshot: Vec<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct WatchRegisterArgs {
    /// 감시할 레지스터 — get_state의 cpu.* 이름(sp/pc/k/a/x/y/ps/d/dbr). 보통 sp
    #[serde(default = "sp_reg")]
    pub(crate) register: String,
    /// 허용 범위 하한(포함, 10진 또는 16진). register가 이보다 작아지면 그 명령에서 break
    #[serde(default)]
    pub(crate) min: Num,
    /// 허용 범위 상한(포함). register가 이보다 커지면 break. 예: SP 정상 $0000~$1FFF → min=0 max=0x1fff
    #[serde(default = "u16_max")]
    pub(crate) max: Num,
    /// 벗어난 명령에서 freeze
    #[serde(default = "default_true")]
    pub(crate) pause_on_hit: bool,
    /// 자동해제 예산(명령 수). watch_register는 매 명령 getState라 무기한이면 emu 스레드를 굶긴다 —
    /// 이 명령 수만큼 실행 후 자동해제하고 watch_disarmed 이벤트를 남긴다. 미지정 시 어댑터 기본(1M).
    /// 드문 후발 derail을 더 오래 감시하려면 키운다(상한 있음).
    #[serde(default)]
    pub(crate) max_instructions: Option<u64>,
}
fn sp_reg() -> String {
    "sp".into()
}
fn u16_max() -> Num {
    Num(0xffff)
}
fn default_true() -> bool {
    true
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct SetTraceArgs {
    /// 실행추적 켜기/끄기
    pub(crate) enabled: bool,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct GetTraceArgs {
    /// 가져올 최근 명령 수(최대 256)
    #[serde(default = "two_fifty_six")]
    pub(crate) count: u64,
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
fn two_fifty_six() -> u64 {
    256
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct PollEventsArgs {
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct BreakOnResetArgs {
    /// 리셋 감지 켜기/끄기
    pub(crate) enabled: bool,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ClearBpArgs {
    pub(crate) id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ScreenshotArgs {
    /// 지정하면 PNG를 이 경로에도 저장한다
    #[serde(default)]
    pub(crate) save_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct StateArgs {
    /// 필터할 그룹(생략 시 전체): cpu·ppu·dmaController·spc·internalRegisters·memoryManager 등
    #[serde(default)]
    pub(crate) groups: Vec<String>,
    /// 멀티코어 백엔드에서 대상 CPU(예: NDS `arm9`/`arm7`). 생략 시 기본 코어. 단일코어는 무시.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ResolveTileArgs {
    /// NBG 레이어 번호 0..3(NBG0~3). 회전배경 RBG는 범위 밖.
    pub(crate) nbg: u32,
    /// 화면 X 픽셀 좌표.
    pub(crate) x: u32,
    /// 화면 Y 픽셀 좌표.
    pub(crate) y: u32,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct SetLayerEnableArgs {
    /// enable할 레이어 이름 목록(대소문자 무시). 준 이름만 enable, 나머지는 disable. 레이어 이름은 응답의
    /// layer_names(또는 layers·mask 둘 다 생략한 조회)로 확인한다. 생략 시 변경 없이 현재 상태만 조회.
    #[serde(default)]
    pub(crate) layers: Option<Vec<String>>,
    /// raw enable 비트마스크(비트0=첫 레이어…). 비트 의미가 per-system이라 이름(layers) 사용 권장. 0이면 전부 disable.
    #[serde(default)]
    pub(crate) mask: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct BisectArgs {
    /// 베이스 세이브스테이트 경로(매 프로브마다 여기로 복귀)
    pub(crate) state: String,
    /// good(낮은) 프레임 경계(deferred — 상한 적용)
    #[serde(deserialize_with = "deser_frame_count")]
    pub(crate) lo: u64,
    /// bad(높은) 프레임 경계(deferred — 상한 적용)
    #[serde(deserialize_with = "deser_frame_count")]
    pub(crate) hi: u64,
    pub(crate) memory_type: String,
    /// 주소(10진 또는 '0x'/'$' 16진)
    pub(crate) address: Num,
    #[serde(default = "one")]
    pub(crate) length: u64,
    /// eq | ne | lt | gt | ge | le
    pub(crate) op: String,
    /// 비교 값(10진 또는 '0x'/'$' 16진)
    pub(crate) value: Num,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct RegressionRunArgs {
    /// 회귀 스위트 디렉토리(하위 케이스 폴더들)
    pub(crate) suite_dir: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct LaunchPlanArgs {
    /// 실행할 ROM/disc/disk 경로. 모르면 생략하고 supported_systems/required_user_input을 받아라.
    #[serde(default)]
    pub(crate) content_path: Option<String>,
    /// 명시 시스템. snes|saturn|ss|psx|pce|md|pc98 등을 수용한다. CUE/CHD/BIN처럼 애매한 media는 이 값을 주는 것이 정본.
    #[serde(default)]
    pub(crate) system: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct LaunchArgs {
    /// 실행할 ROM/disc/disk 경로(필수 — launch는 계획이 아니라 실제 실행이다).
    pub(crate) content_path: String,
    /// 2번째 디스크/디스켓(선택 — 여러 매체를 동시에 물려야 부팅되는 타이틀용). 현재 PC-98 2-드라이브
    /// 게임(System+Sampling 2장 동시 마운트)에서 쓰인다 — 1장만이면 검정 hang. 다른 어댑터는 무시한다.
    #[serde(default)]
    pub(crate) content_path2: Option<String>,
    /// 명시 시스템(snes 등). 미디어가 애매하면 지정한다.
    #[serde(default)]
    pub(crate) system: Option<String>,
    /// 연결 이름(선택 — status.emulator_identity.name에 반영).
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// HITL 창(사람이 보고 직접 플레이)을 띄운다 — 에이전트가 디버거로 붙는 동안 사람이 네이티브 창에서
    /// 게임을 본다. NDS는 desmume-cli를 EMUCAP_NDS_DISPLAY=1로, PSP는 헤드리스 대신 PPSSPPSDL(GUI) 빌드를
    /// 실행한다. macOS는 창이 사는 동안 caffeinate로 디스플레이를 깨워둔다. 기본 false(헤드리스). HITL 창을
    /// 지원하지 않는 어댑터는 무시한다.
    #[serde(default)]
    pub(crate) display: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct VerifyDeterminismArgs {
    /// regression 케이스 디렉토리(case.json + movie/mss). repro·rom만 쓰고 predicate는 무시.
    pub(crate) case_dir: String,
    /// 관측치 종류: auto(기본)|memory|screenshot|state
    #[serde(default)]
    pub(crate) observe: Option<String>,
    /// observe=memory일 때 메모리 타입(read_memory와 동일 식별자)
    #[serde(default)]
    pub(crate) memory_type: Option<String>,
    /// observe=memory일 때 시작 주소
    #[serde(default)]
    pub(crate) address: Option<Num>,
    /// observe=memory일 때 길이
    #[serde(default)]
    pub(crate) length: Option<Num>,
    /// 재생 횟수(2~5, 기본 2)
    #[serde(default)]
    pub(crate) replays: Option<u32>,
}
