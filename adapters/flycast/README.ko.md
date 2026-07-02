# emucap — Flycast(Dreamcast) 어댑터

> English: [README.md](README.md)

Dreamcast(SH-4)를 emucap으로 라이브 디버깅한다. SNES(Mesen)·Saturn/PSX(Mednafen)에 이어 셋째 플랫폼.

## 사용자가 제공하는 것 (에이전트: 이름 그대로 전달)

빌드(`build.sh`)와 launch는 에이전트(나) 스스로 한다. 세 가지 입력은 **사용자**에게서 온다 —
정확한 파일명/경로로 하나씩 안내하고 진행 전 확인받는다:

1. **Flycast 소스 체크아웃, 선택** — `build.sh`는 `FLYCAST_SRC`를 읽기 전용 입력으로만 쓴다.
   실제 `emucap.cpp` 주입과 `build/` 생성은 emucap 소유 빌드 트리에서 수행하므로, 사용자의 체크아웃이나
   그 안의 `build/`를 패치/삭제하지 않는다. 보통은 에이전트가 처리한다: Flycast 소스가 없으면 스크립트가
   emucap build cache 아래로 재귀 클론한다 —
   ```bash
   adapters/flycast/build.sh
   ```
   — 또는 `FLYCAST_SRC=<기존 재귀 체크아웃 경로>`를 지정해 그 checkout을 입력으로 재사용한다.
   `EMUCAP_FLYCAST_BUILD_HOME`은 빈 디렉터리 또는 이전에 `build.sh`가 만든 디렉터리에만 지정한다.
   GitHub에 닿지 못하거나 위치를 골라야 할 때만 사용자를 끌어들인다.

2. **Dreamcast BIOS `dc_boot.bin`** — 사용자 제공. 저작권 있는 Dreamcast 펌웨어라 Flycast·emucap에
   **포함되지 않으며** repo에 **커밋하면 안 된다**; 사용자 본인의 Dreamcast 콘솔/자체 덤프에서 온다.
   `dc_boot.bin`을 한 폴더에 두고 그 폴더(파일 자체가 아니라 **디렉터리**)를 `emu.cfg`의
   `Dreamcast.BiosPath`로 지정한다(사용 절 참고). Flycast는 **BIOS 없이도 많은 게임을 HLE-부팅**할 수
   있어 이건 대개 선택이다 — 게임이 BIOS 없이는 부팅을 거부할 때만 사용자에게 `dc_boot.bin`을 요청한다.

3. **게임 디스크** — 사용자가 제공하는 `.gdi`·`.cdi`·`.chd` 이미지. 그 경로를 MCP `launch` 도구에
   넘긴다. `launch.sh`는 legacy fallback이다.

**OS 현실:** macOS(arm64)가 검증된 런타임 경로다; Linux는 실험적; Windows는 **BETA**다. Rust launcher는
Windows Flycast의 executable-directory config 모델에 맞춰 `Flycast.exe`를 emucap 소유 portable 디렉터리로
복사하고, 그 copy 옆에 `emu.cfg`를 쓴다. Windows에서 Flycast 자체 빌드는 아직 여기서 미검증이다.

## 현 상태: Phase 1 완료 (포크 emucap.cpp — 캡처/제어 전 메서드 라이브 검증)

`emucap.cpp`/`emucap.h`를 Flycast 트리에 주입해 빌드하는 네이티브 어댑터(GDB 브리지 불필요). emucap-mcp에
NDJSON으로 직접 접속, `vblank()`에 주입한 `emucap_service()`로 서비스. **라이브 검증 메서드**(2026-06-27,
Puyo Puyo 4): status·read_memory·write_memory·get_state(SH-4 레지스터)·**save_state·load_state**
(레지스터 정확 복원 확인)·**run_frames**(keepalive로 긴 진행도 무타임아웃)·**screenshot**(running+frozen)·
**set_input·tap·tap_sequence**(타이틀→모드선택 전환)·pause·resume·step(frame)·reset·**set_breakpoint·
clear_breakpoint·clear_all_breakpoints·list_breakpoints·poll_events**(exec BP, 명령-정밀 정지 검증: BP 주소서
pc 정확히 멈춤)·**find_pattern**(addrspace 스캔)·**disassemble**(SH4, OpDesc 디코드)·**get_rom_info**(gameId
HDR-0014 등). 서버-조합(tap/bisect/hold_until/regression)은 이 프리미티브 위에서 동작.

미구현(우아한 거부/GDB-브리지): read/write 워치포인트·step_instructions(freeze 모델상)·dump_memory(평면주소
16MB 덤프는 read8 루프라 느림)·watch_register/get_trace/call_stack(Mesen 특화 일부).

**exec breakpoint는 인터프리터 Run() 루프 훅으로 명령-정밀**이다 — build.sh가 sh4_interpreter.cpp에
`if (g_emucap_bp_armed && emucap_exec_bp_check(pc)) emucap_bp_spin(pc);`를 주입(armed가 false면 bool 한 번만
봐서 핫루프 비용 0). 히트 시 그 명령 실행 전 emucap_bp_spin이 정지·소켓 서비스. read/write 워치포인트와
명령 단위 step은 GDB-브리지(아래 Phase 0)를 쓴다. step_instructions는 vblank-프레임 freeze 모델로 불가라 거부.

음소거: `EMUCAP_MUTE=0`(기본 1=음소거)으로 소리를 켤 수 있다. 런처는 emucap 소유 config copy에만
`aica.Volume`을 쓴다.

⚠ **screenshot은 연속 버퍼 방식이다.** GetLastFrame은 GL 컨텍스트(UI 스레드)가 필요한데 freeze(vblank-스핀)는
UI 렌더를 막아 gui_runOnUiThread/지연 방식은 데드락이다. 그래서 mainui_rend_frame에서 매 렌더마다
`emucap_capture_latest()`로 최신 프레임 raw를 버퍼에 떠두고, screenshot 요청 시 emu 스레드가 그 버퍼를
PNG 인코딩(GL 불필요)한다 → frozen서도 동작(버퍼=freeze 직전=frozen 프레임). ⚠ frozen 중 load_state 후엔
화면 버퍼가 갱신 안 되므로(UI 렌더 정지) `step 1`로 한 프레임 진행해 버퍼를 새로 떠야 로드된 화면이 보인다.

⚠ **입력 주입은 `kcode[]`가 아니라 게임 소비 지점에서 한다.** Flycast 입력의 원천은 `kcode[4]`(Lua
`pressButtons`도 여기에 씀)지만, `kcode[]` 전역에 쓰면 `os_UpdateInputState`(UI 스레드)가 매 프레임 리셋해
emu 스레드 maple 폴링과 경합 → 입력 드롭. 그래서 build.sh가 **`MapleConfigMap::GetInput`(emu 스레드 maple
DMA, 게임이 실제 읽는 지점)에서 `pjs->kcode`를 emucap 주입값으로 override** 한다 — 경합 없이 결정론적.
(`mapleInputState` 직접 쓰기는 kcode→mapleInputState 복사에 덮여 실패한다.)

빌드/실행:
```bash
adapters/flycast/build.sh                  # 소스를 emucap build tree로 동기화하고 그 안에서 훅 주입 + 빌드
# 권장: MCP launch {"content_path": "<disc.gdi>", "system": "dc"}
# fallback: adapters/flycast/launch.sh "<disc.gdi>" <listening_port>
```
fallback launcher는 현재 `status.listening_port`를 요구하며 더 이상 `47800`을 기본값으로 쓰지 않는다.
포트별 config copy, pidfile, log는 emucap 데이터 루트 아래에 둔다(`EMUCAP_EMU_HOME` override, 기본은 아래
OS별 경로).
기본 빌드 산출물:
- macOS: `~/Library/Application Support/emucap/flycast-build/work/build/Flycast.app/Contents/MacOS/Flycast`
- Linux: `${XDG_DATA_HOME:-~/.local/share}/emucap/flycast-build/work/build/flycast`
- Windows BETA: `%LOCALAPPDATA%\emucap\flycast-build\work\build\Flycast.exe`
`FLYCAST_APP`은 실행파일 경로나 macOS `Flycast.app` 번들 경로를 모두 받을 수 있다.

⚠ macOS arm64: 재빌드 .app은 JIT 서명이 없어 **dynarec가 크래시** → 런처가 인터프리터(Dynarec.Enabled=no)를
강제한다. 디버깅엔 충분하다.

## 과거: Phase 0 (GDB-스텁 브리지 PoC)

`emucap-gdb-bridge.py` — Flycast **내장 GDB 스텁**(SH-4)을 emucap NDJSON으로 중계하는 PoC.
포크/빌드 없이 Dreamcast에서 emucap 루프를 증명한다. 라이브 검증 완료(2026-06-27, Puyo Puyo 4).

**지원(광고) 메서드**: `read_memory`·`write_memory`·`get_state`(SH-4 레지스터)·`status`·`pause`·`resume`·
`step`(1명령)·`set_breakpoint`(exec/SW만)·`clear_breakpoint`·`list_breakpoints`·`poll_events`.
**미지원(GDB 스텁 한계 — 우아한 강등)**: screenshot·set_input·save/load_state·run_frames·HW watchpoint.
→ Phase 1(Flycast 포크 + emucap.cpp 소켓 훅)에서 채운다.

⚠ GDB 스텁 부착 시 dynarec이 꺼져 느려진다(Flycast 구조). 명령 단위 추적엔 무방.

## 사용

전제: Flycast가 `ENABLE_GDB_SERVER=ON`으로 빌드돼 있어야 한다(emucap build가 이 옵션을 켠다).
런처는 emucap 소유 runtime copy에서 Flycast를 실행하고 `EMUCAP_EMU_HOME/flycast/<port>/` 아래 isolated
`emu.cfg`를 seed한다. 기존 사용자 `emu.cfg`가 있으면 입력으로 복사한 뒤 필요한 설정만 조정한다.
seed된 `[config]`에는 다음이 들어간다.
```ini
Debug.GDBEnabled = yes
Debug.GDBPort = 3263
Debug.GDBWaitForConnection = no
Dreamcast.BiosPath = <dc_boot.bin 있는 디렉터리>
```
`Dreamcast.BiosPath`는 사용자 제공 `dc_boot.bin`이 들어 있는 **디렉터리**다(“사용자가 제공하는 것” 참고); HLE-부팅이면 생략한다.

절차:
```bash
# 1) emucap-mcp bootstrap/status를 호출하고 반환된 listening_port를 사용한다.
# 2) 권장 경로는 MCP launch 도구다. runtime copy, config, Flycast, bridge를 함께 준비한다.
# 3) MCP launch 도구 밖에서 실행해야 할 때만 legacy fallback을 쓴다.
adapters/flycast/launch.sh "<disc.gdi>" <listening_port> [name]
# 4) emucap MCP 도구로 제어: status → {adapter:"flycast-gdb"} 확인 후 pause/get_state/read_memory/step/set_breakpoint
```

주소는 전 SH-4 주소(메인 RAM `0x8C......`, 1ST_READ.BIN `0x8C010000`~). hex 문자열 수용.
정확한 스냅샷은 `pause` 후 읽는다(emucap 결정론 컨벤션).

## Phase 1 계획 (포크)

Flycast 포크 진입점으로 emucap.cpp 소켓 훅을 더해 10개 메서드
전부 + 풀스피드(dynarec 유지)를 제공한다: `addrspace::read/write*`·`Sh4cntx`·`dc_savestate/loadstate`·
`renderer->GetLastFrame`·`mapleInputState[]`·`Emulator::run/step/stop/start`. GdbServer의 asio·emu-thread
stop/start 핸드셰이크가 스레딩 템플릿.
