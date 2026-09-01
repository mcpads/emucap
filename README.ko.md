# emucap — emulator monitor + HITL adaptor

> English: [README.md](README.md)

레트로 게임 패치 디버깅을 위한 MCP 인프라. 실행 중인 에뮬레이터의 메모리·상태·화면을 AI
에이전트가 읽고 제어해, 사람이 설명한 문제를 분석하도록 돕는다. 공통 Core + 어댑터로 여러
에뮬레이터를 지원한다 — Mesen2(SNES·Game Gear·Game Boy·GBC·GBA·NES), Mednafen 포크(Saturn·
PlayStation·PC Engine·PC-FX·Mega Drive/Genesis·WonderSwan/WSC·Neo Geo Pocket/Color), Flycast(Dreamcast), DeSmuME 포크(Nintendo DS),
PPSSPP 포크(PSP), PCSX2 포크(PlayStation 2), Dolphin 포크(GameCube·Wii), MAME과 선택형
NP2kai 호환 backend(PC-98), MAME(실험적 Neo Geo MVS/AES/CD), 실험적 Mupen64Plus frontend(Nintendo 64).
원본 Xbox는 고정된 xemu 포크로 실험 지원한다.
Stock openMSX 21.0과 별도 Rust XML bridge로 C-BIOS MSX2+ 및 실제 firmware
MSX1/MSX2/MSX2+ 카트리지 profile도 제공한다.

**v0.16.0 — 베타.** 이 저장소는 계속 활발히 개발 중이며 이후 릴리스에서 인터페이스와
동작이 바뀔 수 있다. 어댑터 가용성은 호스트 환경에 따라 다르며 `status`가 실제로 사용할 수
있는 기능을 보고한다.

GPL-2.0-or-later로 배포한다. [LICENSE](LICENSE)와 [NOTICE](NOTICE)를 참고한다.

## 플랫폼

Rust Core(두 MCP)와 Rust `launch` 도구는 크로스플랫폼이다(macOS Apple Silicon+Intel, Linux,
Windows). 에뮬레이터별 build/launch 요구사항은 OS마다 다르다 — 자동화가 모자라면 에이전트가
upstream 설치 절차로 에뮬레이터를 준비해 emucap에 연결하고, 호스트에서 실제로 쓸 수 있는 도구는
`status`가 보고한다. Windows에서는 Unix shell launcher보다 Rust `launch` 도구와 문서화된 env
override를 우선한다.

## Agent에게 설치를 맡기기

이 저장소는 **에이전트(Claude Code·Codex 등)가 설치를 직접 수행**하도록 만들어졌다. 비개발자는
저장소를 받은 뒤 에이전트에게 이렇게 말하면 된다:

> "이 저장소 README의 'Agent 설치 절차'대로 emucap을 빌드하고 MCP 서버로 등록해줘."

에이전트가 아래를 순서대로 실행한다. Core 설치는 가볍고, 에뮬레이터별 어댑터는 필요할 때만
설치한다.

**에이전트가 사용자의 인터페이스다.** 사용자가 터미널, 빌드 도구, 에뮬레이터 설정을 모른다고 가정한다.
명령은 에이전트가 직접 실행하고, GUI 클릭이 필요한 단계는 메뉴 위치와 버튼 이름을 짧게 안내한 뒤
확인하고 진행한다. 사용자의 OS에 맞춰 절차를 조정한다.

### 1. 사전 요건 (에이전트가 확인 후 없으면 설치)

- **Rust 1.88 이상** — `command -v cargo`와 `rustc --version`으로 확인. 없으면:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"`
- **C 컴파일러** (SQLite 번들 빌드용) — macOS: `xcode-select -p || xcode-select --install`.
  Linux: `cc --version || sudo apt-get install -y build-essential`. Windows: MSVC C++ build tools를
  설치한다(Rust installer가 설치를 제안하면 진행). 이후 일반 PowerShell에서 빌드한다.
- **git**.

### 2. Core 빌드

저장소 루트에서:

```sh
cargo build --release \
  --bin emucap --bin emucap-mcp --bin emucap-track-mcp --bin emucap-broker \
  --bin emucap-mame-pc98-bridge --bin emucap-mame-neogeo-bridge \
  --bin emucap-mupen64plus --bin emucap-desmume-nds-bridge \
  --bin emucap-ppsspp-bridge --bin emucap-pcsx2-bridge \
  --bin emucap-openmsx-bridge --bin emucap-np2kai \
  --bin emucap-xemu-bridge
```

산출물: `target/release/emucap-mcp`(**제어 MCP** — 에뮬레이터 조작), `emucap-track-mcp`(**추적
MCP** — 실험 원장, emulator-less), `emucap`(케이스 번들 CLI), `emucap-broker`(다중 세션 broker),
`emucap-mame-pc98-bridge`(PC-98 launch helper), `emucap-mame-neogeo-bridge`(Neo Geo MVS/AES/CD launch helper),
`emucap-mupen64plus`(N64 frontend·adapter), `emucap-desmume-nds-bridge`(NDS launch helper),
`emucap-ppsspp-bridge`(PSP launch helper), `emucap-pcsx2-bridge`(PS2 launch helper),
`emucap-openmsx-bridge`(stock openMSX XML-control helper), `emucap-np2kai`(PC-98 호환 frontend),
`emucap-xemu-bridge`(원본 Xbox QMP/GDB launch helper).
Source build의 의존성은 전부 crates.io이고
SQLite는 번들이라 **Rust와 C 컴파일러 외 시스템 패키지가 필요 없다**(깨끗한 체크아웃에서 그대로
빌드된다). 첫 빌드는 의존성을 내려받느라 더 걸리고, 이후는 빠르다.

### 3. MCP 서버 등록 (두 MCP)

emucap은 **두 MCP**로 나뉘어 있고 **둘 다 등록한다** — 에이전트가 둘을 조립한다(§3계층).

- **제어 MCP**(`emucap-mcp`) — 에뮬레이터 조작 엔진. 메모리·상태·화면을 읽고, 입력·세이브스테이트·
  브레이크포인트를 제어하고 선택적 분석 결과를 *반환*한다. 정적 도구 목록은 간결한 기본
  리모컨이다. 지속·기기별 입력은 `input_control(operation="describe")`, 복합·기기별 디버거 기능은
  `debug(operation="describe")`, 재현성 분석은 `analysis(operation="describe")`로 연다. 각 서랍은
  현재 runtime의 operation과 schema만 반환하며 실행도 같은 도구가 맡는다. 메모리 쓰기·disassembly·
  call stack·breakpoint·event polling은 디버거 기본 기능이라 direct tool로 유지한다. 실행 중 매체 교체도 direct이며
  frozen 상태와 `status.media_devices`의 device ID를 요구한다. 정확한 guest-time 전진은 frozen으로
  돌아오는 `step`을 사용한다. 어댑터의 free-running frame wait는 호환용 wire 동작으로만 남고 MCP
  에이전트에게는 노출하지 않는다.
  정확한 bounded 버튼 입력은 입력권을 반환하고 frozen으로 끝나는 direct `tap`을 쓴다. Guest가 계속
  실행되는 실시간 pulse는 runtime이 지원할 때만 입력 서랍의 `pulse_while_running`으로 명시적으로 연다.
- **추적 MCP**(`emucap-track-mcp`) — 실험 원장(`.emucap/`). run을 시작(`run_start`)·기록(`log_*`)·
  질의(`query_runs`/`compare_runs`/`summarize_runs`)한다. **에뮬레이터를 모른다**(emulator-less). 제어
  MCP에 *얹혀* 실험을 남기는 add-on이라, 켜지 않아도 제어 MCP는 그대로 동작한다.

**Claude Code:**

```sh
claude mcp add emucap-control -- "$(pwd)/target/release/emucap-mcp"
claude mcp add emucap-track   -- "$(pwd)/target/release/emucap-track-mcp"
```

**Codex**:

```sh
tools/register-codex-mcp.sh
```

Windows에서는 PowerShell에서 `tools/register-codex-mcp.ps1`을 실행한다. 스크립트는 source build의
`target/release/` 바이너리를 사용해 `emucap`과 `emucap-track`을 등록한다.

필요 시 환경변수로 조정한다: `EMUCAP_PORT`(제어 MCP, 기본 47800, 점유 중이면 자동으로 다음 포트),
`EMUCAP_TRACK_ROOT`(추적 MCP의 실험 원장 위치, 기본 작업 repo git root의 `.emucap`).

등록 후 에이전트 세션을 재연결(`/mcp`)한다. **두 MCP가 각자 `bootstrap`을 노출하므로** 제어 MCP의
`bootstrap`(에뮬 진입)과 추적 MCP의 `bootstrap`(원장 진입)이 모두 도구 목록에 보이면 성공이다. 안
보이면 release를 다시 빌드하고 재연결한다 — MCP 서버는 release 바이너리를 실행하므로 debug 빌드는
반영되지 않는다.

### 3b. 3계층과 에이전트 조립

세 계층이 조화를 이루되 서로 독립이다(비유: ②추적 MCP는 MLflow, ①제어 MCP는 TensorFlow):

1. **에뮬레이터 조작**(제어 MCP) — 도메인-무관 라이브 제어 엔진. 그 자체로 완결(추적 없이도 디버그 가능).
2. **실험 관리**(추적 MCP) — add-on. ①을 *몰라도 되고*, ①에 얹혀 실험을 기록·질의한다.
3. **응용/방법론**(예: 로컬라이제이션 패치 방법론 skill) — ①·②를 *조립*하는 최상층. **이 자리는
   교체 가능**하다(다국어 패치·팬게임·AI TAS 등 무엇이 들어와도 아래 두 계층을 그대로 쓴다).

두 MCP는 서로를 호출하지 않는다 — **에이전트가 조립한다**:

- **rom_sha1 전달**: 제어 MCP의 `get_rom_info`(`.rom_sha1`)로 불투명한 추적 식별자를 읽어 추적
  MCP의 `run_start(rom_sha1=…)`에 그대로 넘긴다. 관리형 descriptor media는 에뮬레이터 시작 전에
  진입 파일과 loader가 선언한 모든 파일을 pre-launch `content_identity`로 묶으며, 호환용 `rom_sha1`에는
  그 SHA-256 식별자를 담는다.
  복합 미디어의 설명 파일만 따로 해시해서는 안 된다.
  `connection_ref`(제어 MCP `status`의 연결 이름 또는 `"port:N"`)를 함께 넘기면 같은 연결의 직전
  미종료 run이 자동 마감된다.
- **반환된 식별자는 그대로 사용**: `rom_sha1`, `run_id`, `finding_id`를 경로나 별도 이름으로
  가공하지 않는다. 관리 식별자는 영문·숫자를 단일 하이픈으로만 연결하며 경로 문법, 점, underscore는
  물론 플랫폼 예약 장치명도 파일시스템 접근 전에 거부한다.
- **분석 verb는 반환만**: 필요할 때 `analysis(operation="describe")`를 호출하고 같은 도구로
  선택한 `regression_run`/`verify_determinism` operation을 실행한다. 제어 MCP가 에뮬을 구동해
  결과를 *반환*할 뿐 원장에 쓰지 않는다. 남기려면 그 결과를 추적 MCP의 `log_gate`(예:
  `determinism_replay`의 `kind=machine` 판정)·`log_metric`으로 기록한다.
- **프레임 경계 탐색은 debug `probe` operation을 조립**: 같은 베이스 상태에서 원자적 probe를 반복해 프레임
  범위를 이분한다. 각 호출이 상태 복원·진행·판정을 한 번에 수행하므로 호출 사이 지연은 결과를 바꾸지 않는다.
  유지보수 어댑터가 native transaction을 제공하거나, Control이 generation link를 잡은 채 검증된
  pause/resume/load/exact-step/read를 조합한다. 실제 호출 가능한 결과만 live `status.methods`에 나타난다.
- **개입은 명시 기록**: `write_memory`/`load_state`/`reset`/입력 같은 상태변경을 제어 MCP가 자동
  기록하지 않으므로, 재현 충실도(repro_status)를 위해 추적 MCP의 `log_intervention`으로 직접 남긴다.

### 4. 첫 동작 (에이전트가 bootstrap으로 시작)

모든 emucap 작업은 `bootstrap`으로 시작한다. 에이전트에게 "emucap `bootstrap`을 호출해줘"라고
하면, 기본 응답이 `listener.port`·system ID·catalog revision·그리고 무엇을 켤지 물어볼 질문을
간결하게 돌려준다. 전체 routing catalog는 `bootstrap(include=["systems"])`, build/runtime 경로는
`bootstrap(include=["installation"])`으로 명시적으로 요청한다. 기본 응답은 살아 있는 managed runtime
개수만 포함한다. 기존 generation을 이어갈 때만 `bootstrap(include=["runtimes"])`을 요청하면,
stable host session ID가 없는 client도 반환된 lease의 exact `launch_id`로 `reattach`할 수 있다. runtime
파일을 직접 편집할 필요가 없다. 이후
`launch_plan(content_path, system?)`이 검증된 MCP `launch` 도구 인자를 돌려준다.
에이전트는 adapter readiness까지 기다리는 `launch`를 호출한 뒤 `status`로 live identity와 runtime
identity를 확인한다. Launcher script는 개발자용 진입점이지 managed lifecycle의 대체 경로가 아니다.

관리형 CUE·GDI·CCD·TOC·M3U는 닫힌 미디어 그래프다. Descriptor를 선택하면 그 진입 파일 하나만
읽도록 허용된다. `launch_plan`은 간접 파일의 metadata·내용·해시를 읽기 전에 정확한 상대 경로를
`review_input`으로 반환한다. 각 이름이 선택한 미디어에 속하는지 검토한 뒤 서버가 만든
`indirect_media_approval`을 그대로 되돌려 보낸다. 중첩 M3U는 다음 descriptor frontier를 한 번 더
검토할 수 있다. 참조는 이식 가능한 상대 경로이고 진입 파일 디렉터리 아래의 symlink가 아닌 실제
일반 파일이어야 한다. 검토나 그래프 검증이 실패하면 에뮬레이터 시작 전에 거부된다.
`content_identity_binding: "prelaunch"`는 이 identity가 mutable source의 snapshot이나 실제 loader
소비 증명이 아님을 밝힌다.

일반 launch는 guest가 이미 실행 중인 상태로 반환할 수 있다. 첫 후속 동작에 네트워크·에이전트 지연이
guest time으로 섞이면 안 될 때는 `start_frozen: true`를 요청한다. 지원하는 launcher는 adapter가
frozen guest 경계에 연결된 뒤에만 성공한다. 이는 launch 이후 guest time을 닫는 계약이며 power-on RAM,
RTC, save 같은 초기 조건의 동일성을 뜻하지 않는다. 그러한 producer-owned 조건은 live capability가
광고하는 `execution_profile: "repeatable"`을 별도로 선택하며,
`record_window(require_repeatable: true)`는 선택한 recording origin이 해당 조건을 지원하지 않으면 reset,
input, guest advance 전에 거부한다.
Mesen SNES의 격리 runtime을 갱신할 때는 일반 profile의 battery save와 portable data를 보존한다.
Repeatable profile은 별도의 폐기 가능한 portable root를 사용하므로, 깨끗한 초기 조건을 만들기 위해
일반 profile이나 사용자의 표준 Mesen 저장을 삭제하지 않는다. Mednafen Mega Drive도 별도의 폐기 가능한
home과 같은 opt-in 협상 방식을 사용한다. 첫 guest 명령 전에 제한된 상태를 캡처하고, 허용된 각
`reset_release` 기록 전에 복원하므로 같은 프로세스에서 앞서 수행한 실행이나 debugger 쓰기가 선언되지
않은 기록 입력으로 섞이지 않는다. 일반 Mednafen profile과 저장 파일은 바뀌지 않는다.

`listener.base_port`는 direct mode에서 빈 포트를 찾기 시작하는 값일 뿐이며 다른 살아 있는 MCP 세션이
이미 사용 중일 수 있다. Launcher는 실제로 할당된 `listener.port` 또는 full `status`의
`listening_port`만 사용한다. Emulator generation을 `stop`해도 그 세션의 MCP listener는 닫히지 않으며,
MCP 세션이 끝날 때 반환된다.

연결된 첫 full `status`는 `capability_revision`을 반환한다. 반복 확인에서는 이를
`known_capability_revision`으로 보내면 capability catalog가 바뀌지 않았을 때 그 묶음만 생략하고,
현재 execution·continuity·generation·ownership 상태는 계속 반환한다.

전체 status에 `recording_capability`가 있을 때는 debug `record_window` operation이 에이전트·네트워크 지연을 guest
time에 섞지 않고 유한한 guest-frame 구간을 소유할 수 있다. 현재 workspace 안의 기존 absolute
`output_root`와 frame 수를 주면 emulator를 frozen으로 되돌리고 검증된 bundle path와 manifest hash를
반환한다. event class와 limit의 권위는 live capability다. 지원하지 않는 adapter는 hook을 설치하거나
guest를 진행하지 않고 거부한다. 선택적 origin·입력 무비·event stop도 그 exact capability가 광고할
때만 쓸 수 있으며, 생략하면 기존 next-frame bounded 동작을 유지한다.
`recording_capability.state_load`가 광고될 때 절대 경로와 `preserve_for_recording=true`를 준 frozen
`save_state` 응답은 producer가 관리하는 `snapshot_receipt`도 반환한다. 이후
`record_window(origin="state_load")`는 caller가 적은 path·digest·경계
주장이 아니라 그 receipt의 불투명한 `snapshot_id`만 받는다. 증명된 frame boundary에서 만든 receipt만
허용된다. Core는 관리 중인 byte와 runtime generation·hash를 다시 검증해 bundle에 복사하고, adapter에
load와 window를 한 transaction으로 보낸다. Adapter는 정확한 frame 좌표를 복원하고 dense input movie,
hook, sink를 모두 소유한 뒤에만 guest 실행을 풀며, 끝에는 다시 frozen을 반환한다. Instruction boundary
receipt는 성공한 save 사실은 나타내지만 frame window에는 쓸 수 없다. `preserve_for_recording`을 생략하거나
false로 두면 기존 save 동작은 바뀌지 않고, `state_load`를 사용하지 않는 load·recording 동작도 바뀌지 않는다.
발생률이 높은 event class는 정수 payload field별 filter capability를 추가로 광고할 수 있다. 선택적
class별 half-open range는 선언한 관측 범위만 좁히며, 제외된 callback은 drop으로 세지 않는다. 범위
안의 event는 기존 sequence·limit·integrity 규칙을 그대로 따르고, 광고되지 않은 field와 잘못된
범위는 guest mutation 전에 거절된다.
`recording_capability.warmup`이 있으면 `warmup_frames` 동안 producer 기본 transaction class는 계속
기록하고 observation event 발행은 정확한 guest frame 경계까지 미룬다.
`warmup.selectable_event_scopes`에 있는 class는 `event_arming_overrides`로 광고된 scope를 선택할 수 있고,
생략하면 producer 기본값을 유지한다. 선택한 scope는 stream과 event·byte·drop 회계 구간을 정하며 native
emulator hook의 설치 여부까지 약속하지 않는다. 입력 무비는 두 구간을 한 요청 안에서 모두 포함한다.
선택한 event가 `startable`이면 `start_on`으로 첫 occurrence에 observation 시작을 맞출 수 있다.
`initial_snapshots`는 capability가 광고한 callback-safe memory type과 상한을 추가로 요구하며, Core가
별도 인증 binary sink로 받아 manifest의 exact anchor event와 member hash를 묶는다. 선택적 terminal snapshot은
`recording_capability.terminal_snapshots`의 상한과 `status.memory_regions`의 정확한 유한 영역이 모두
있을 때만 쓸 수 있다. Core가 arming 전에 모든 범위를 검증하고 terminal frame이 frozen인 동안만
읽어 hash가 있는 bundle member로 게시한다. 이 필드를 생략하면 추가 memory read가 없다.
`recording_capability.terminal_state`가 광고한 profile 하나를 선택하면 같은 frozen 종점의 producer
정의 상태를 canonical JSON member 하나로 보존할 수 있다. 소비자 schema는 emucap에 들어오지 않는다.
Control MCP가 재시작되어도
`status.recording_capture`가 bounded
active/terminal capsule을 이어 보여주며 event bytes와 private staging path는 status에 넣지 않는다.

timeout이나 `connected: false`는 transport 상태이지 에뮬레이터 종료의 증거가 아니다.
재실행하기 전에 `status.continuity.runtime_binding`·`status.runtime_instance` 또는
`status.stale_runtime_instance`·`get_failure_context`를 확인한다.
자동 재부착은 같은 stable control identity로 한정한다. 그 밖에는 bootstrap runtime discovery에서
명시적으로 available인 exact generation을 골라 `reattach(launch_id=...)`하고, 의도적으로 교체할 때만 identity가 검증되는
`launch(..., replace: true)`를 쓴다. Flycast fatal quarantine에서는 먼저 보존 문맥을 읽고,
`status.methods`가 광고할 때만 debug `dismiss_failure` operation을 호출한다.

managed emulator를 종료할 때는 `status.runtime_instance.launch_id`를 읽고
`stop(launch_id=...)`을 호출한다. 제어 MCP가 current generation, 제어 lease,
process-start identity를 확인한 뒤 emulator와 기록된 bridge의 실제 종료까지 기다린다.
실패 증거는 보존하며, stale generation이나 broker 소유·unmanaged process는 추측해서
종료하지 않는다.

## 에뮬레이터별 어댑터 (필요할 때 에이전트가 설치)

하나만 먼저 골라 시작하면 된다. MesenCE는 guest를 전진시키지 않는 native debugger halt에서 요청을
처리해야 하므로 로컬 소스 빌드를 사용한다.

- **Mesen2 (SNES·Game Gear·Game Boy·GBC·GBA·NES)** — `adapters/mesen2/build.sh`(Windows:
  `build.ps1`)를 실행한다. 고정 MesenCE 2.2.1을 Git에서 제외된 빌드 디렉터리에 받고 GPLv3 patch stack을
  적용해 로컬에서 빌드하며 에뮬레이터 바이너리는 배포하지 않는다. 시스템별 Lua 엔트리로 처리한다
  (SNES는 65816, Game Gear/Master System은 Z80, Game Boy/GBC는 SM83, GBA는 ARM7, NES는 6502). GBA는
  실 BIOS(`gba_bios.bin`, 비커밋)가 필요하고 SNES/Game Gear/GB/GBC/NES는 필요 없다. 수정되지 않은
  Mesen 빌드는 native halt service와 안전한 savestate event가 없어 live control에서 명시적으로 거부한다.
  → `adapters/mesen2/README.md`
- **Mednafen (Saturn·PSX·PCE·PC-FX·MD·WonderSwan/WSC·Neo Geo Pocket/Color)** — `adapters/mednafen/build.sh`로 포크를 빌드한다(SDL 필요:
  macOS `brew install sdl2`, Linux `libsdl2-dev`). 소스 archive와 checksum을 고정하며 한 바이너리가
  일곱 시스템 계열을 처리한다. PSX·PCE-CD·PC-FX는 BIOS가 필요하다(저장소에 커밋하지 않음).
  PC-FX는 version 1.00 BIOS를 명시적으로 검증하고 emucap 소유 Mednafen profile로 실행한다.
  Neo Geo Pocket/Color는 patch된 `ngp` 모듈을 공유한다. 범위를 제한한 TLCS-900/H debugger가
  부작용 없는 RAM/ROM/BIOS view, RAM 쓰기, 정확한 명령 step, 안전한 disassemble과 exec-only
  breakpoint를 제공한다. Sound Z80 상태, read/write breakpoint, trace, call stack은 제외한다.
  → `adapters/mednafen/README.md`
- **Flycast (Dreamcast)** — `adapters/flycast/build.sh`로 빌드한다. 빌드는 emucap 소유 work tree에서
  수행하고 commit과 recursive submodule graph를 고정한다. `FLYCAST_SRC`가 있으면 읽기 전용 Git object
  source로만 쓴다. → `adapters/flycast/README.md`
- **DeSmuME (Nintendo DS)** — `adapters/desmume-nds/build.sh`로 headless 포크를 빌드한다(meson/ninja/
  SDL2/glib 필요). NDS BIOS는 필요 없다(HLE direct-boot). 듀얼 CPU(ARM9/ARM7)마다 GDB 스텁이 붙는
  PC-98 어댑터와 같은 구도다. → `adapters/desmume-nds/README.md`
- **PPSSPP (PSP)** — `adapters/ppsspp/build.sh`로 headless 포크를 빌드한다(CMake·C++ 툴체인 필요).
  PSP 펌웨어는 필요 없다. 어댑터는 PPSSPP 자체 디버거 프로토콜에 붙는 순수 WebSocket 클라이언트라
  GDB 스텁 없이 headless 프로세스 + 브리지 둘로만 뜬다. → `adapters/ppsspp/README.md`
- **PCSX2 (PlayStation 2)** — `adapters/pcsx2/build.sh`로 고정된 포크를 빌드하고,
  `EMUCAP_PCSX2_BIOS`에 사용자가 준비한 BIOS 덤프의 절대경로를 지정한다. 격리된 headless 실행에서
  EE 메모리·레지스터·패턴 검색·덤프, 프레임 스텝, 디스어셈블, frozen 세이브스테이트,
  스크린샷·컨트롤러 입력, 레지스터 스냅샷을 포함한 정지형 EE 브레이크포인트,
  best-effort 콜스택, 원자적 state/frame/memory probe와 동기식 리셋을 bounded PINE 브리지로 지원한다.
  → `adapters/pcsx2/README.md`
- **Dolphin (GameCube·Wii)** — `adapters/dolphin/build.sh`(Windows: `build.ps1`)로 고정된 native
  포크를 빌드한다. 기본 실행은 headless이고 GUI 빌드가 있으면 `display: true`로 DolphinQt 창을 연다.
  PowerPC 메모리·레지스터, 정확한 명령 스텝, 디스어셈블, best-effort 콜스택, 레지스터 스냅샷을
  포함한 실행 브레이크포인트, 시간 제한이 있는 스크린샷, 동기식 세이브스테이트를 지원한다.
  GameCube는 port-0 컨트롤러 입력, Wii는 Emulated Wii Remote 1의 core button 입력을 지원한다.
  Wii IR·motion·extension은 지원 범위가 아니다.
  → `adapters/dolphin/README.md`
- **MAME PC-98** — `adapters/mame-pc98/build.sh`로 MAME을 소스에서 빌드한다(시간이 오래
  걸리고 디스크를 많이 쓴다). 고정된 빌드는 키보드 입력과 상대 포인터 이동, frozen 클릭·드래그를
  제공하며 창에 연결된 네이티브 마우스 입력권을 지속적으로 차지하지 않는다.
  → `adapters/mame-pc98/README.md`
- **NP2kai PC-98 HDI 호환 backend(선택형)** — `adapters/np2kai/build.sh`를 실행하고
  `emucap-np2kai`를 빌드한 뒤 합법적으로 준비한 firmware 경로를
  `EMUCAP_NP2KAI_FIRMWARE`로 지정한다. `pc98_backend: "np2kai"`를 명시해야 선택되며,
  생략하면 계속 MAME을 사용한다. 패치된 core와 direct host는 MAME PC-98과 같은 Control/Debug
  method 전체를 제공한다. 제한된 memory 접근·dump, breakpoint·event, register state,
  instruction step, disassemble, trace·best-effort call stack, 정확한 frame, 입력, screenshot,
  native state, 검증된 `hdd0` 교체가 포함된다. 단, 기기 의미는 동일하다고 가장하지 않는다.
  NP2kai는 headless이며 host audio launch와 `hdd0` eject가 없고, disassemble은 현재 CPU mode로
  제한되며 breakpoint memory snapshot은 일시정지되는 hit에서만 캡처한다. 이 backend는 `.hdi`만
  받는다. read breakpoint는 권위 있는 접근 값을 제공하지 않으며, write breakpoint만 값 필터를
  지원한다.
  → `adapters/np2kai/README.md`
- **MAME Neo Geo MVS/AES/CD (실험적)** — `adapters/mame-neogeo/build.sh`로 전용 고정 MAME subset을
  빌드하고 `emucap-mame-neogeo-bridge`를 빌드한다. MVS는 사용자가 준비한 `neogeo.zip` BIOS와
  해당 MAME 버전에 맞는 게임 ROM set을 사용한다. AES는 `aes.zip`과 ZIP stem이 고정된 MAME
  Neo Geo software list의 AES 호환 항목을 가리키는 cartridge set을 사용한다. CD는 공식 BIOS가 든
  `neocdz.zip`과 모든 참조 track이 존재하는 CUE entry file을 사용하며 콘텐츠 identity는 전체
  CUE graph를 포함한다. 세 profile 모두 제한된 RAM, 68000 상태·명령 스텝, 프레임 제어,
  exec/read/write breakpoint와 hit-time 증거, disassemble, frozen-frame 스크린샷과 port-0
  입력을 제공한다. Native save/load는 MVS와 AES에서 광고하며 MAME 0.288이 unsupported로
  표시하는 CDZ에서는 제외한다. 파일 확장자만 보고 어느 Neo Geo profile로도 자동 판정하지 않는다.
  → `adapters/mame-neogeo/README.md`
- **Mupen64Plus Nintendo 64 (실험적, Unix)** — `adapters/mupen64plus/build.sh`를 실행하고
  `emucap-mupen64plus`를 빌드한다. 일반 카트리지 ROM은 BIOS가 필요 없다. 현재 pure interpreter로
  격리된 headless/창 실행, pause/resume, R4300 명령 스텝, CPU 상태, frozen RDRAM 제한 읽기·쓰기를
  지원한다. 두 모드 모두 port-0 입력 hold와 명시적인 native 입력권 반환을 제공한다. 창 실행은
  callback barrier를 이용한 정확한 rendered-frame 스텝, 제한된 입력 pulse, 현재 PNG 캡처,
  완료를 확인하는 native save/load도 제공한다. 두 모드는 모두 동기식 reset, R4300
  exec/read/write breakpoint와 hit-time 증거, event polling, disassemble을 제공한다.
  headless는 rendered-frame 기능을 노출하지 않는다.
  RSP 상태는 이 profile의 범위가 아니다.
  → `adapters/mupen64plus/README.md`
- **openMSX MSX 카트리지 profile (실험적)** — `adapters/openmsx/build.sh`를 실행하고
  `emucap-openmsx-bridge`를 빌드한다. 공식 launcher는 sidecar가 맞는 stock openMSX 21.0만
  받아 emucap 소유 per-port `HOME`에서 실행하며 openMSX를 patch하거나 사용자의 emulator profile을
  읽지 않는다. `msx`는 C-BIOS MSX2+, `msx1`·`msx2`·`msx2p`는 사용자가 제공한 실제 firmware
  profile이다. 카트리지 범위는 Z80 상태·명령 step, headless/visible exact frame step, 제한된
  CPU memory/main RAM/VRAM 접근, frozen save/load, keyboard-matrix와 2-port joystick 입력,
  exec/read/write breakpoint, event polling, disassemble을 제공한다. Screenshot은
  `display: true`에서만 제공한다. Disk/tape는 대표 runtime 증거가 없고 turboR/R800은 미구현이다.
  일반 `.rom` 파일은 MSX system ID를 명시한다. → `adapters/openmsx/README.md`
- **xemu 원본 Xbox (실험적)** — `adapters/xemu/build.sh`로 고정된 GPLv2 포크를 빌드하고
  `emucap-xemu-bridge`를 빌드한다. 사용자가 준비한 MCPX·flash ROM·HDD template 디렉터리를
  `EMUCAP_XEMU_FIRMWARE`로 지정하며 EEPROM은 선택 사항이다. 관리형 실행은 기기 입력을 세대별
  격리 디렉터리로 복사하고 사용자의 일반 xemu profile을 열지 않는다. 제어된 frozen 시작,
  CPU 상태·메모리, 정확한 frame·instruction step, 기본 무음이며 display와 독립적인 sound 선택,
  버튼·아날로그 입력과 native 입력권 반환, screenshot, reset,
  disc 교체, breakpoint, disassemble, best-effort call stack과 frozen save/load를 지원한다. State
  container는 내부 VM/HDD snapshot을 해당 generation의 EEPROM, exact disc, host build, controller
  topology와 함께 묶으며 같은 관리형 launch generation 안에서만 유효하다. 협상형 debug 기능은 같은
  generation 안에서 state load·정확한 frame 진행·frozen memory read를 한 요청으로 묶는 원자적 probe도
  제공한다.
  대표 game XISO smoke에서 실제 게임 메뉴 진입, confirm·방향 입력 소비와 `sound:true`의 가청
  출력을 확인했다. 이는 전체 게임 호환성을 보장한다는 뜻은 아니다.
  → `adapters/xemu/README.md`

## 더 보기

- 무엇을·왜 만드나, 그리고 바이너리 → `CLAUDE.md`
- 에뮬레이터별 메모리 타입·버튼 이름·브레이크포인트·실행 트러블슈팅 → 각 `adapters/*/README.md`
- 바이너리: `emucap`(케이스 번들 `finalize`/`inspect`), `emucap-mcp`(제어 MCP — 실행 중 에뮬레이터
  조작, stdio), `emucap-track-mcp`(추적 MCP — 실험 원장, emulator-less, stdio),
  `emucap-broker`(다중 세션 연결 공유), N64 frontend, 그리고 빌드 절에 적은 PC-98/Neo Geo/NDS/PSP/PS2/MSX/Xbox
  launch bridge.
