use schemars::JsonSchema;
use serde::Deserialize;

use emucap::live::tools::StepUnit;

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
    // 공통 파서는 lib(emucap::numparse)에 둔다 — MCP와 CLI가 같은 규칙으로 0x/$ 16진을 받게 한다(#45).
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
    /// 메모리 타입 식별자. 유효한 이름은 연결된 시스템마다 다르니 `status.memory_types`를 확인한다. 시스템별 이름과 의미는 각 `adapters/*/README.md`에 설명되어 있다.
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
    /// 디스어셈블 시작 주소(10진 또는 '0x'/'$' 16진). CPU/ISA와 지원 여부는 연결된 시스템의 `status.methods`와 어댑터 README에서 확인한다. 반환 [{addr,text,bytes}].
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

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct WriteMemoryFileArgs {
    /// 읽을 raw binary 파일의 절대 경로. 에뮬레이터 어댑터가 아니라 제어 MCP가 읽는다.
    pub(crate) path: String,
    /// 파일 안에서 읽기 시작할 바이트 오프셋(기본 0).
    #[serde(default)]
    pub(crate) offset: Option<Num>,
    /// 파일에서 읽을 바이트 수. `status.contracts.constraints["memory.write.max_bytes"]` 이하여야 한다.
    pub(crate) length: Num,
    /// 선택적 SHA-256 전제조건. 실제 slice의 hash가 다르면 메모리를 바꾸기 전에 거부한다.
    #[serde(default)]
    pub(crate) sha256: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct WriteMemoryArgs {
    pub(crate) memory_type: String,
    /// 시작 주소(10진 또는 '0x'/'$' 16진)
    pub(crate) address: Num,
    /// 직접 쓸 바이트(hex 문자열). `input_file`과 정확히 하나만 지정한다.
    #[serde(default)]
    pub(crate) hex: Option<String>,
    /// raw binary 파일 slice. `hex`와 정확히 하나만 지정한다.
    #[serde(default)]
    pub(crate) input_file: Option<WriteMemoryFileArgs>,
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
    /// 누른 채 진행할 프레임 수(탭). 지정하면 프레임 효과와 release가 끝난 뒤 terminal 응답하고,
    /// 생략하면 다음 touch까지 hold를 설정한 뒤 즉시 반환한다.
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

/// Common bound for one synchronous frame or instruction advance. At 60 fps, 5,000 frames take
/// about 83 seconds, leaving cleanup time before the 300-second deferred deadline. Bridges that
/// step one instruction at a time also receive a finite work bound. Callers split longer travel
/// into terminally acknowledged requests.
pub(crate) const MAX_SYNC_ADVANCE_COUNT: u64 = emucap::live::temporal::MAX_SYNC_ADVANCE_COUNT;

/// Reject a supplied frame or instruction count above the common synchronous bound. Serde defaults
/// bypass this function when a field is absent; every default remains within the bound.
fn deser_frame_count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_SYNC_ADVANCE_COUNT {
        return Err(serde::de::Error::custom(format!(
            "frame/instruction count {n} exceeds the synchronous limit {MAX_SYNC_ADVANCE_COUNT}; split the request and verify each terminal response"
        )));
    }
    Ok(n)
}

/// Frame bound for deferred input operations such as press, tap, and hold. An oversized request
/// could outlive the link deadline and leave a button override active after the MCP has given up.
/// Keep this equal to the common advance limit so a composed operation is not rejected only when it
/// reaches its internal step.
pub(crate) const MAX_INPUT_HOLD_FRAMES: u64 = MAX_SYNC_ADVANCE_COUNT;

fn deser_input_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_INPUT_HOLD_FRAMES {
        return Err(serde::de::Error::custom(format!(
            "입력 hold 프레임 {n}이 상한 {MAX_INPUT_HOLD_FRAMES} 초과 — 링크 deadline을 넘겨 MCP 포기 후에도 입력이 눌린 채 남는다"
        )));
    }
    Ok(n)
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
    /// 진행할 단위 수. 이전 `frames` 이름도 입력 호환을 위해 받는다.
    #[serde(
        default = "one",
        alias = "frames",
        deserialize_with = "deser_frame_count"
    )]
    pub(crate) count: u64,
    /// 진행 단위. 기본값은 frames. status.contracts.constraints의 execution.step.units를 확인한다.
    #[serde(default)]
    pub(crate) unit: StepUnit,
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
    /// 현재 연결에서 허용되는 값은 `status.breakpoint_kinds`를 확인한다. 각 항목은 kind와 함께
    /// start/end의 단위, memory_type 사용 여부, snapshot 지원 여부를 설명한다.
    pub(crate) kind: String,
    /// 메모리 타입(read_memory 참조). 선택한 kind가 사용하는지는 `status.breakpoint_kinds`를 확인한다.
    pub(crate) memory_type: String,
    /// 포함 범위의 시작(10진 또는 '0x'/'$' 16진). 단위는 `status.breakpoint_kinds`를 확인한다.
    pub(crate) start: Num,
    /// 포함 범위의 끝(단일 값이면 start와 같게). 단위는 `status.breakpoint_kinds`를 확인한다.
    pub(crate) end: Num,
    /// 히트 시 freeze(status가 frozen이 됨). 교차-ROM 정렬 앵커에 사용
    #[serde(default)]
    pub(crate) pause_on_hit: bool,
    #[serde(default)]
    pub(crate) auto_savestate: bool,
    /// 선택 pc 조건(read/write/exec): 이 접근을 일으킨 명령의 pc가 [pc_min,pc_max]일 때만 break(정상 push 등 노이즈 제거). pc의 폭·의미는 연결된 시스템 CPU에 따르며 get_state의 `cpu.pc`와 같은 기준이다. 자세한 주소 규칙은 어댑터 README에서 확인한다. pc_max와 함께. (kind=dma에선 vram_addr 범위 필터로 재사용 — vmin/vmax)
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
    /// 시점 상태가 보존된다(drift·deadman 무관). 지원 여부는 `status.breakpoint_kinds`를 확인한다.
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
    /// 명시 시스템. snes|saturn|ss|psx|pce|md|pc98 등을 수용한다. CUE/CHD/BIN처럼 애매한 media는 이 값을 지정한다.
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
    /// 게임을 본다. Mednafen은 네이티브 SDL 창을, PC-98은 MAME의 실제 video/keyboard provider를,
    /// NDS는 desmume-cli를 EMUCAP_NDS_DISPLAY=1로, PSP는 헤드리스 대신 PPSSPPSDL(GUI) 빌드를
    /// 실행한다. macOS는 창이 사는 동안 caffeinate로 디스플레이를 깨워둔다. 기본 false(헤드리스).
    /// HITL 창을 지원하지 않는 어댑터는 무시한다.
    #[serde(default)]
    pub(crate) display: Option<bool>,
    /// 오디오 출력을 켠다. 현재 Mednafen(Saturn/PSX/PCE/MD/WonderSwan)에서만 지원하며 기본은
    /// false다. display와 독립적이고, 지원하지 않는 어댑터에서 true를 주면 조용히 무시하지 않고 거부한다.
    #[serde(default)]
    pub(crate) sound: Option<bool>,
    /// current capsule의 동일 프로세스가 살아 있을 때 명시적으로 교체한다. PID와 process start identity가
    /// 모두 일치할 때만 해당 generation의 프로세스를 종료하며, 불명확하면 안전하게 거부한다.
    #[serde(default)]
    pub(crate) replace: bool,
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
