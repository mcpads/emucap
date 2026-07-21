# emucap — emulator monitor + HITL adaptor

> English: [README.md](README.md)

레트로 게임 패치 디버깅을 위한 MCP 인프라. 실행 중인 에뮬레이터의 메모리·상태·화면을 AI
에이전트가 읽고 제어해, 사람이 설명한 문제를 분석하도록 돕는다. 공통 Core + 어댑터로 여러
에뮬레이터를 지원한다 — Mesen2(SNES·Game Gear·Game Boy·GBC·GBA·NES), Mednafen 포크(Saturn·
PlayStation·PC Engine·Mega Drive/Genesis·WonderSwan/WSC), Flycast(Dreamcast), DeSmuME 포크(Nintendo DS),
PPSSPP 포크(PSP), PCSX2 포크(PlayStation 2), Dolphin 포크(GameCube·Wii), MAME PC-98.

**v0.10.0 — 베타.** 이 저장소는 계속 활발히 개발 중이며 이후 릴리스에서 인터페이스와
동작이 바뀔 수 있다. 어댑터 가용성은 호스트 환경에 따라 다르며 `status`가 실제로 사용할 수
있는 기능을 보고한다.

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

- **Rust** — `command -v cargo` 로 확인. 없으면:
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
  --bin emucap-mame-pc98-bridge --bin emucap-desmume-nds-bridge \
  --bin emucap-ppsspp-bridge --bin emucap-pcsx2-bridge
```

산출물: `target/release/emucap-mcp`(**제어 MCP** — 에뮬레이터 조작), `emucap-track-mcp`(**추적
MCP** — 실험 원장, emulator-less), `emucap`(케이스 번들 CLI), `emucap-broker`(다중 세션 broker),
`emucap-mame-pc98-bridge`(PC-98 launch helper), `emucap-desmume-nds-bridge`(NDS launch helper),
`emucap-ppsspp-bridge`(PSP launch helper), `emucap-pcsx2-bridge`(PS2 launch helper).
Source build의 의존성은 전부 crates.io이고
SQLite는 번들이라 **Rust와 C 컴파일러 외 시스템 패키지가 필요 없다**(깨끗한 체크아웃에서 그대로
빌드된다). 첫 빌드는 의존성을 내려받느라 더 걸리고, 이후는 빠르다.

### 3. MCP 서버 등록 (두 MCP)

emucap은 **두 MCP**로 나뉘어 있고 **둘 다 등록한다** — 에이전트가 둘을 조립한다(§3계층).

- **제어 MCP**(`emucap-mcp`) — 에뮬레이터 조작 엔진. 메모리·상태·화면을 읽고, 입력·세이브스테이트·
  브레이크포인트를 제어하고, 분석 verb(`regression_run`/`verify_determinism`)로 결과를 *반환*한다.
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

- **rom_sha1 전달**: 제어 MCP의 `get_rom_info`(`.sha1`)로 ROM 식별자를 읽어 추적 MCP의
  `run_start(rom_sha1=…)`에 넘긴다(어댑터가 `get_rom_info` 미지원이면 `shasum -a1 <content>`).
  `connection_ref`(제어 MCP `status`의 연결 이름 또는 `"port:N"`)를 함께 넘기면 같은 연결의 직전
  미종료 run이 자동 마감된다.
- **분석 verb는 반환만**: `regression_run`/`verify_determinism`은 제어 MCP가 에뮬을 구동해
  결과를 *반환*할 뿐 원장에 쓰지 않는다. 남기려면 그 결과를 추적 MCP의 `log_gate`(예:
  `determinism_replay`의 `kind=machine` 판정)·`log_metric`으로 기록한다.
- **프레임 경계 탐색은 `probe`를 조립**: 같은 베이스 상태에서 원자적 `probe`를 반복 호출해 프레임
  범위를 이분한다. 각 호출이 상태 복원·진행·판정을 한 번에 수행하므로 호출 사이 지연은 결과를 바꾸지 않는다.
- **개입은 명시 기록**: `write_memory`/`load_state`/`reset`/입력 같은 상태변경을 제어 MCP가 자동
  기록하지 않으므로, 재현 충실도(repro_status)를 위해 추적 MCP의 `log_intervention`으로 직접 남긴다.

### 4. 첫 동작 (에이전트가 bootstrap으로 시작)

모든 emucap 작업은 `bootstrap`으로 시작한다. 에이전트에게 "emucap `bootstrap`을 호출해줘"라고
하면, `bootstrap`이 `listening_port`·`runtime_paths`(각 어댑터의 build 경로와 legacy fallback launcher)·
지원 시스템·그리고 무엇을 켤지 물어볼 질문을 돌려준다. 이후 `launch_plan(content_path, system?)`이
MCP `launch` 도구 인자를 돌려주고, 에이전트가 `launch`를 호출한 뒤 몇 초 뒤 `status`를 확인한다.
즉 **어댑터 설치 경로와 fallback도 bootstrap이 알려주므로**, 에이전트가 로컬을 헤맬 필요가 없다.

timeout이나 `connected: false`는 transport 상태이지 에뮬레이터 종료의 증거가 아니다.
재실행하기 전에 `status.continuity.runtime_binding`·`status.runtime_instance` 또는
`status.stale_runtime_instance`·`get_failure_context`를 확인한다.
살아 있는 소유 generation에는 재부착하고, 의도적으로 교체할 때만 identity가 검증되는
`launch(..., replace: true)`를 쓴다. Flycast fatal quarantine에서는 먼저 보존 문맥을 읽고,
`status.methods`가 광고할 때만 `dismiss_failure`를 호출한다.

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
- **Mednafen (Saturn·PSX·PCE·MD·WonderSwan/WSC)** — `adapters/mednafen/build.sh`로 포크를 빌드한다(SDL 필요:
  macOS `brew install sdl2`, Linux `libsdl2-dev`). 소스 archive와 checksum을 고정하며 한 바이너리가 다섯 시스템을 모두 처리한다.
  PSX·PCE-CD는 BIOS가 필요하다(저장소에 커밋하지 않음). → `adapters/mednafen/README.md`
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
  best-effort 콜스택과 동기식 리셋을 bounded PINE 브리지로 지원한다.
  → `adapters/pcsx2/README.md`
- **Dolphin (GameCube·Wii)** — `adapters/dolphin/build.sh`(Windows: `build.ps1`)로 고정된 native
  포크를 빌드한다. 기본 실행은 headless이고 GUI 빌드가 있으면 `display: true`로 DolphinQt 창을 연다.
  PowerPC 메모리·레지스터, 정확한 명령 스텝, 디스어셈블, best-effort 콜스택, 레지스터 스냅샷을
  포함한 실행 브레이크포인트, 시간 제한이 있는 스크린샷, 동기식 세이브스테이트를 지원한다.
  GameCube에서는 컨트롤러 입력도 주입할 수 있다.
  → `adapters/dolphin/README.md`
- **MAME PC-98** — `adapters/mame-pc98/build.sh`로 MAME을 소스에서 빌드한다(시간이 오래
  걸리고 디스크를 많이 쓴다). → `adapters/mame-pc98/README.md`

## 더 보기

- 무엇을·왜 만드나, 그리고 바이너리 → `CLAUDE.md`
- 에뮬레이터별 메모리 타입·버튼 이름·브레이크포인트·실행 트러블슈팅 → 각 `adapters/*/README.md`
- 바이너리: `emucap`(케이스 번들 `finalize`/`inspect`), `emucap-mcp`(제어 MCP — 실행 중 에뮬레이터
  조작, stdio), `emucap-track-mcp`(추적 MCP — 실험 원장, emulator-less, stdio),
  `emucap-broker`(다중 세션 연결 공유), 그리고 빌드 절에 적은 PC-98/NDS/PSP/PS2 launch bridge.
