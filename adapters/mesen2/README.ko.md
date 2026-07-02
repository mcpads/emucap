# emucap — Mesen2 어댑터

> English: [README.md](README.md)

## 에이전트에게 — 당신이 사용자의 인터페이스다

사용자는 개발자가 아닐 가능성이 높고 이 파일을 읽지 않는다 — 당신이 이 파일을 읽고 각 단계를
안내한다. 터미널/기술 작업은 당신이 직접 한다. GUI 단계는 어느 메뉴를 클릭해야 하고 그것이 어디
있는지(예: "창 맨 위에 있는 메뉴 바")를 정확히 알려주고, 정확한 버튼/체크박스 라벨을 따옴표로
제시하며, 사용자가 실제로 했는지 확인한 뒤 다음으로 넘어간다. 아래 단계는 Windows를 가정한다 —
사용자의 시스템이 macOS/Linux일 때만 그 차이를 언급한다.

## 1. Mesen2 설치

- **Windows**: 현재 유지되는 MesenCE releases 페이지
  <https://github.com/nesdev-org/MesenCE/releases> 를 열고 `Mesen_<version>_Windows.zip` 을 내려받는다.
  압축을 풀면 안에 파일 하나, `Mesen.exe` 가 있다(따로 실행할 설치 프로그램은 없다). `Mesen.exe` 를
  더블클릭한다. 첫 실행 시 "Select Data Storage Folder" 대화창이 뜨는데, 어느 쪽을 골라도 무방하다 —
  그대로 진행한다.
- **macOS/Linux**: 같은 Releases 페이지에서 해당 플랫폼용 빌드를 내려받는다.

## 2. 한 번만 설정 — 어댑터 스크립트가 저장·접속하도록 허용

어댑터는 Mesen이 실행하는 Lua 스크립트다. Mesen은 기본적으로 스크립트의 디스크·네트워크 접근을
막으므로, 체크박스 두 개를 한 번 켜야 한다(Mesen이 기억하므로 처음 한 번만 필요하다):

1. SNES ROM 아무거나 로드한다: 상단 메뉴 바 → **File → Open**, ROM 파일을 고른다. (게임이
   로드되기 전엔 다음 단계가 회색으로 비활성화돼 있다.)
2. 상단 메뉴 바 → **Debug → Script Window** (단축키 Ctrl+N). 새 창이 열린다.
3. 그 Script Window 자체의 메뉴 바(창 안쪽)에서 → **Script → Settings**. "Debugger Settings"
   창이 "Script Window" 탭에 열린다.
4. **Restrictions** 항목 아래: 먼저 **"Allow access to I/O and OS functions"** 를 체크한다 —
   그러면 **"Allow network access"** 가 클릭 가능해지니 그것도 체크한다.
5. **OK** 를 클릭한다.

## 3. 어댑터 로드·실행

Script Window에서 → **File → Open** (Ctrl+O) → `adapters/mesen2/emucap-live.lua`(라이브 에이전트
제어) 또는 `emucap.lua`(회고 번들 캡처)를 고른다. 자동으로 실행되며, 하단 로그 창에 I/O 경고 없이
`emucap: ROM 경로 = …` 가 찍혀야 한다. (수동 재실행: **Script → Run Script**, F5.)

ROM 경로는 `getRomInfo`로 자동 추론된다. 추론이 빗나가면 `emucap.lua` 상단의 `ROM_PATH` 폴백을
고치거나, 확정 시 `emucap finalize --rom` 으로 덮어쓴다.

## 실행 내부 동작 · macOS 주의

### 띄우기 — 크래시 연쇄 주의

크래시·스턱 Mesen이 남아 있으면 새 Mesen이 연쇄 크래시할 수 있다(Avalonia RenderTimer -6661). 또
크래시 시 macOS "예기치 않게 종료" 대화창이 떠 있으면 닫히기 전엔 새 실행이 막힌다.

- ⚠ 잔류 인스턴스를 **광역 종료하지 말 것**(`pkill -i mesen`·`killall Mesen`은 다른 세션의 Mesen을
  죽인다). 정리는 `launch.sh`에 맡긴다 — 그 포트의 고아 인스턴스만 정리하고, 연결된 인스턴스가 있으면 거부한다.

권장 경로는 MCP `launch` 도구다. 이 경로는 Mesen을 emucap 소유 portable 디렉터리로 복사하고, 그 copy 옆의
`settings.json`만 쓴다. 사용자의 기본 Mesen 설정과 앱 상태는 건드리지 않는다. legacy
`adapters/mesen2/launch.sh <ROM> <EMUCAP_PORT> [EMUCAP_NAME]`도 같은 portable copy 규칙을 쓰며,
MCP launch 도구를 쓸 수 없을 때의 fallback이다.

## 회고 캡처 (emucap.lua)
- 플레이 중 **Ctrl+Shift+C** 를 누르면 직전 약 `DEPTH × INTERVAL` 프레임의
  슬라이스(세이브스테이트·스크린샷)와 입력 무비가 `bundles/<시각>-retrospective/`
  에 떨어진다. 화면에 "EMUCAP CAPTURED" 가 잠깐 표시된다.

## 확정 · 분석
```
emucap finalize bundles/<시각>-retrospective
emucap inspect  bundles/<시각>-retrospective
```
그 다음 `note.md` 에 문제를 적고, Claude Code 에게 번들 디렉토리를 분석시킨다.

## 튜닝
`INTERVAL`(샘플 간격), `DEPTH`(링 깊이), `TRIGGER_KEYS`(키 조합)를 스크립트
상단에서 조정한다.

## 라이브 MCP 모드 — 에이전트 운용

별도 스크립트 `emucap-live.lua`로 실행 중 게임을 에이전트가 읽고 제어한다. MCP 서버
`emucap-mcp`가 stdio로 뜨고, Lua가 그 서버의 TCP 포트(기본 47800)에 접속한다.

- 읽기: `read_memory`/`find_pattern`(바이트패턴 검색 — 영역 직접 스캔, 매칭 오프셋만)/`screenshot`/`get_state`/`get_rom_info`/`status`.
- 능동: `write_memory`/`set_input`/`press_buttons`/`tap`/`tap_sequence`/`hold_until`/`save_state`/`load_state`/
  `run_frames`/`pause`/`step`/`step_instructions`/`resume`/`reset`/`probe`.
  (⚠ save_state/load_state는 **running에서만**(frozen 거부 — exec 콜백 컨텍스트 필요). set_input 홀드는 빈
  set_input으로 명시 해제할 때까지 유지된다(resume/step은 해제 안 함).)
- 브레이크포인트·추적: `set_breakpoint`(kind **exec/read/write/nmi/irq/dma**; pc_min/pc_max 조건, **value/value_mask/
  value_len 값-조건**; write BP가 $2118/$2119→**vram_addr**·$2122→cgram_addr·$2104→oam_addr 목적지를 이벤트에 동봉)·
  `clear_breakpoint`/`list_breakpoints`/`clear_all_breakpoints`/`poll_events`·`watch_register`·
  `set_trace`/`get_trace`/`call_stack`·`break_on_reset`.
- 디스어셈블: `disassemble(address, count)` → `[{addr,text,bytes}]`. Mesen2 Lua엔 디스어셈블 API가
  없어 65816 디코더를 어댑터에 직접 구현(M/X 플래그는 `cpu.ps`에서 시작해 REP/SEP 추적).
- 분석: `dump_memory`/`bisect`/`regression_run`.
- `verify_determinism` — 재현 레시피 N회 재생 해시 일치로 재현성 측정(determinism_replay 게이트).
- **참고**: 위 목록 중 `tap`/`tap_sequence`/`hold_until`/`step_instructions`/`bisect`/`regression_run`/
  `verify_determinism`은 어댑터 네이티브가 아니라 MCP 서버(`emucap-mcp`)가 원시 도구(set_input·step·
  read_memory 등)로 합성한다. 어댑터가 직접 광고하는 네이티브 메서드는 `hello.methods`가 정본이다.

### 에이전트가 Mesen을 띄운다

포트는 `status`의 `listening_port`를 쓴다(47800 하드코딩 금지). 기본은 MCP `launch` 도구다:

```json
{"content_path": "/path/to/game.sfc", "system": "snes", "name": "snes_session"}
```

런처는 `EMUCAP_EMU_HOME` 또는 OS 기본 emucap 데이터 루트 아래에 portable Mesen copy를 만들고,
그 copy의 `settings.json`만 쓴다. fallback pidfile과 log도 `EMUCAP_LOG`를 따로 주지 않는 한 같은
포트별 디렉터리 아래에 둔다.

**macOS / Linux fallback** — MCP `launch` 도구를 쓸 수 없을 때만 `launch.sh`를 쓴다:

```bash
REPO=/path/to/emu-monitor-hitl-adaptor
"$REPO/adapters/mesen2/launch.sh" "/path/to/game.sfc" <listening_port> [name]
# launch.sh는 TCP 연결(ESTABLISHED + post-connect grace) 확인 후에야 "연결됨"을 출력·반환한다 — 별도 sleep 불필요.
```

`launch.sh`는 `MESEN_BIN`, macOS 기본 앱 경로, PATH(`Mesen`/`mesen`) 순서로 찾고, 찾은 원본을
그 자리에서 실행하지 않고 emucap 소유 portable copy로 실행한다.

**Windows fallback** — MCP `launch` 도구를 쓸 수 없을 때만 **`launch.ps1`**을 쓴다. 이 스크립트는
`Mesen.exe`를 `%LOCALAPPDATA%\emucap\mesen2\<port>\portable` 아래로 복사하고, 그 위치의 settings만 쓴다.
MCP listener가 `<listening_port>`에 없으면 시작하지 않고, 이미 에뮬레이터가 연결된 포트도 거부한다.
`mesen.pid`/`mesen.log`는 포트별 디렉터리에 쓰며, 실제 연결 확인 후 반환한다. 스크립트는
`MESEN_BIN`, 일반적인 사용자/program-files 설치 경로, PATH 순서로 찾는다. 필요하면 `MESEN_BIN`으로
경로를 준다. `EMUCAP_SESSION_TOKEN`이 없으면 OS temp의 세션 토큰 파일을 읽어
전달한다.

```powershell
$env:MESEN_BIN = "C:\path\to\Mesen.exe"
powershell -ExecutionPolicy Bypass -File "<repo>\adapters\mesen2\launch.ps1" "C:\path\to\game.sfc" <listening_port> [name]
```

- ROM 경로는 에이전트가 안다(사용자가 알려주거나 빌드 산출물 경로).
- `launch.sh`가 "MCP listener가 없다"고 하면 에뮬레이터를 다시 띄우지 말고 먼저 `status`를 다시 호출한다.
  렌더러/비디오 초기화 직후 종료처럼 보이는 로그도 launcher timeout이 SIGTERM으로 정리한 결과일 수 있다.
- macOS에서 신규 Mesen 창이 안 뜨거나 launch.sh가 "연결됨" 직후 실패하면 reopen/open dialog 또는 saved
  application state가 끼어든 사례나 디스플레이 슬립 렌더러 실패를 먼저 의심한다. fallback launcher는 portable
  copy를 direct 실행하고 가능한 경우 `caffeinate`를 쓴다. 그래도 반복되면 Mesen 창/대화창을 직접 확인한 뒤
  다시 띄운다.
- transient한 순간(스프라이트 팝업 등)을 사람이 그 자리서 얼리려면 Mesen 창에서 **freeze 핫키 `Home`**
  (`EMUCAP_FREEZE_KEY`로 변경; 같은 키로 resume 토글)을 누른다 — codeBreak freeze라 emucap이 응답을 유지한 채
  무기한 freeze된다(`status.reason="hotkey"`). ⚠ Mesen **GUI Pause는 쓰지 말 것** — 연결이 끊겨 'not connected'가
  되고 GUI에서 resume하기 전엔 복구 안 된다.
- 환경변수: `MESEN_BIN`(원본 Mesen 실행파일 또는 macOS 앱 번들 경로; 없으면 일반 설치 경로와 PATH를 확인),
  `EMUCAP_EMU_HOME`(portable copy 루트), `EMUCAP_LAUNCH_WAIT`(연결 대기 초, 기본 20), `EMUCAP_POST_CONNECT_GRACE`
  (연결 후 유예 초, 기본 2), `EMUCAP_LOG`(로그 경로), `EMUCAP_DEADMAN_MS`(스텝 간격이 길면 데드맨이 자동
  resume — 기본 30000, 0이면 끔), `EMUCAP_RECONNECT_GIVEUP_MS`(MCP 재연결 대기 상한, 기본 600000, 0이면 무기한).
- `EMUCAP_PREARM`은 콜드부팅 직후 DMA write BP를 사전무장한다(형식 `dma` | `dma:<dest>` |
  `dma:<dest>:<vmin>-<vmax>`). 부팅 중 한순간 사라지는 DMA write(예: 어트랙트 전 초기화)를 에이전트
  왕복으로는 못 잡을 때, launch 시점에 미리 걸어 첫 히트에서 freeze한다.

### 접속 확인
`status` 도구를 호출한다 → `{"connected":true,"frame":…,"state":"running"}` 이면 준비됨.
부팅 직후 첫 호출은 `emulator not connected`가 나올 수 있으니 몇 초 뒤 재시도한다. MCP 서버는
지연 바인드라, Mesen이 아직 없어도 도구 호출이 "not connected"로 graceful하게 응답한다.

### (대안) GUI로 로드
이미 Mesen이 떠 있으면 Debug → Script Window에서 `emucap-live.lua`를 로드해도 된다.

서버·클라이언트 모두 `EMUCAP_PORT`로 포트를 맞춘다.

## 교차-ROM 디프 (원본 vs 패치본)

패치가 무엇을 깨뜨렸는지 찾는다 — 두 ROM을 같은 논리적 순간까지 몰아 상태를 비교한다.

1. 두 emucap-mcp 세션을 띄우고 각 세션 `status`의 `listening_port`로 두 인스턴스를 launch.sh로 띄운다:
   - `launch.sh "<JP.sfc>" <portA> emucap-a`
   - `launch.sh "<KR.sfc>" <portB> emucap-b`
   - 포트는 세션마다 status가 알려주는 값을 쓴다(하드코딩 금지). 단일 세션 순차로 하려면 broker 모드.
2. **정렬**: 두 인스턴스의 *같은 게임-로직 주소*에 `set_breakpoint(..., pause_on_hit=true)`.
   양쪽을 진행시키면 그 이벤트에서 각자 freeze한다(프레임 카운트가 아니라 로직으로 정렬 —
   패치가 타이밍을 바꿔도 견고). 텍스트 패치는 로직 주소를 안 바꾸므로 둘 다 같은 BP를 친다.
3. **덤프**: frozen 상태에서 `dump_memory(dirA)`·`dump_memory(dirB)`(메모리 + state.json).
4. **비교**: `emucap diff dirA dirB`.
   - 패치가 의도적으로 바꾼 차이(번역 텍스트·폰트)는 곳곳에 뜬다. 이를 가르려면:
     - **기준선 빼기**: 정상 지점에서 `emucap diff A_good B_good --json > baseline.json`,
       버그 지점에서 `emucap diff A_bug B_bug --baseline baseline.json` → 새 차이만.
     - **상태 디프**: 레지스터/DMA/PPU는 텍스트 패치가 건드리면 안 됨 → 거기 차이는 버그
       신호. `--ignore-key`로 노이즈 키 추가 제외.

## 주의
`createSavestate` 의 호출 컨텍스트와 `getInput` 반환 키는 Mesen2 버전에 따라
다를 수 있다. 처음 사용 시 실측으로 먼저 확인한다.
