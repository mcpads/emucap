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
    /// 메모리 타입 식별자. SNES/MD/PCE/PSX/Saturn/PC-98별 이름은 서버 instructions와 status.memory_types 참조.
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
    /// 베이스 상태에서 진행할 프레임 수.
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
    /// 디스어셈블 시작 주소(10진 또는 '0x'/'$' 16진). SNES=65816, MD=68000, Saturn=SH-2, PSX=MIPS R3000A, PCE=HuC6280. 반환 [{addr,text,bytes}].
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
    pub(crate) frames: u64,
}
fn two() -> u64 {
    2
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct TapArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// 누를 프레임 수(기본 2 — auto-repeat 미만의 짧은 탭으로 정확히 1칸/1회 이동)
    #[serde(default = "two")]
    pub(crate) press_frames: u64,
    /// 떼고 더 진행할 프레임 수(기본 0). >0이면 입력+관찰을 한 콜에(frozen 유지)
    #[serde(default)]
    pub(crate) after_frames: u64,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct TapSequenceArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// 각 원소가 한 탭의 버튼셋. 예: [["down"],["down"],["a"]] = 세 탭을 순차로
    pub(crate) steps: Vec<Vec<String>>,
    #[serde(default = "two")]
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
    /// 안 바뀌면 멈출 상한 프레임(기본 300)
    #[serde(default = "three_hundred")]
    pub(crate) max_frames: u64,
}
fn three_hundred() -> u64 {
    300
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct PathArgs {
    pub(crate) path: String,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct RunFramesArgs {
    pub(crate) n: u64,
}
fn one() -> u64 {
    1
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct StepArgs {
    #[serde(default = "one")]
    pub(crate) frames: u64,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct StepInstructionsArgs {
    /// 진행할 CPU 명령 수
    #[serde(default = "one")]
    pub(crate) count: u64,
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
    /// 선택 pc 조건(read/write/exec): 이 접근을 일으킨 명령의 pc가 [pc_min,pc_max]일 때만 break(정상 push 등 노이즈 제거). SNES는 24비트 CPU버스 PC, MD/Mednafen은 68000 PC, PC-98은 i386 cpu.pc/MAME debugger pc 기준. pc_max와 함께. (kind=dma에선 vram_addr 범위 필터로 재사용 — vmin/vmax)
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
    /// good(낮은) 프레임 경계
    pub(crate) lo: u64,
    /// bad(높은) 프레임 경계
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
    /// 명시 시스템(snes 등). 미디어가 애매하면 지정한다.
    #[serde(default)]
    pub(crate) system: Option<String>,
    /// 연결 이름(선택 — status.emulator_identity.name에 반영).
    #[serde(default)]
    pub(crate) name: Option<String>,
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
