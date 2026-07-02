# emucap — Mednafen(Sega Saturn·Sony PlayStation·PC Engine·Mega Drive) 어댑터

> English: [README.md](README.md)

Mednafen에는 Mesen 같은 Lua가 없어, **Mednafen을 패치해 소켓 클라이언트(`emucap.cpp`)를
넣는다** — Mesen `emucap-live.lua`의 C++판. emucap-mcp에 접속해 같은 NDJSON 프로토콜을
서비스하므로 Rust 측(TcpLink·tools·MCP)은 그대로다.

**한 바이너리가 Saturn(ss)·PlayStation(psx)·PC Engine(pce)·Mega Drive(md)를 모두 처리한다.** Mednafen이 로드 디스크/ROM으로
시스템을 자동 판별하고, emucap이 런타임에 `CurGame->shortname`("ss"/"psx"/"pce"/"md")으로 시스템
특화 동작(주소공간 매핑·버튼 테이블·엔디안)을 분기한다. 공통 디버거 인터페이스는 무수정으로
각 시스템에 동작한다. PCE 분석은 정확도/디버거 우선의 `pce` 코어를 기본으로 삼는다. `pce_fast`도
빌드되지만 Mednafen 쪽 Debugger 포인터가 없어 memory/register/breakpoint 계열 도구는 `no_debugger`로
강등된다.

Mednafen은 GPL이라 통째 벤더링/재배포하지 않는다. 우리 추가분(`emucap.cpp`/`.h`)만 이
저장소에 두고, `build.sh`가 업스트림 Mednafen을 로컬에서 받아 패치·빌드한다.

## 전제

### 빌드 의존성 (에이전트가 설치한다 — 사용자가 여기서 할 일은 없다)
- brew: `flac libsndfile lzo musepack sdl2-compat zstd gettext`, `pkg-config`, clang.
- `build.sh`가 Mednafen 자체를 받아 빌드하므로, 사용자가 따로 설치할 에뮬레이터는 없다.

### BIOS 파일 — 사용자가 반드시 제공해야 한다 (에이전트: 이것을 체크리스트로 전달할 것)

BIOS 파일은 저작권이 있는 콘솔 펌웨어다. **emucap은 이를 포함할 수 없고 포함하지 않는다** — 사용자가
자기 콘솔이나 덤프에서 제공한다. **BIOS 파일을 저장소에 커밋하지 말 것.**

**어디에 두나.** macOS/Linux에서 폴더는 `~/.mednafen/firmware/`다. 없으면 먼저 만들고
(`mkdir -p ~/.mednafen/firmware`), 아래의 정확한 이름으로 파일을 넣는다.
**Windows**에서 Mednafen 어댑터는 BETA이고 소스 빌드가 만만치 않다 — 거기서 firmware 경로를
가정하지 말 것; 최상위 `README.md`의 **Platforms** 노트에서 에이전트 주도 fallback(업스트림에서
에뮬레이터를 설치하고 env override로 emucap을 그쪽으로 가리키기)을 보고, 경로를 추측하지 않는다.

**시스템별 — 사용자에게 정확한 파일명과 정확한 폴더를 알려줄 것:**

| 시스템 | BIOS 필요? | 정확한 파일 → 위치 |
|--------|-----------|-------------------|
| **PlayStation**(psx) | **필수** — 없으면 부팅 불가 | `scph5500.bin`(JP)·`scph5501.bin`(NA)·`scph5502.bin`(EU), 디스크 리전에 맞춰 → `~/.mednafen/firmware/` |
| **PC Engine CD**(pce, CD 타이틀) | CD 타이틀엔 **필수** | `syscard3.pce` → `~/.mednafen/firmware/`(기본; 다른 위치를 쓰려면 `pce.cdbios`/`pce_fast.cdbios` 설정) |
| **Saturn**(ss) | 권장 | `sega_101.bin`(JP) 등 → `~/.mednafen/firmware/`; `~/.mednafen/mednafen.cfg`의 `ss.bios_jp`로 가리킨다 |
| **PC Engine HuCard**(`.pce`) | **불필요** | BIOS 없이 부팅된다 |
| **Mega Drive / Genesis** | **불필요** | 카트리지 ROM(`.md`/`.gen`/`.smd`, 또는 header가 있는 `.bin`)은 BIOS 없이 부팅된다 |

이미 RetroArch를 쓰는 사용자면 같은 BIOS 파일을 복사해 재사용할 수 있지만, RetroArch system
directory는 호스트별 설정값이므로 문서나 스크립트에 로컬 경로를 박지 않는다.

**동작 확인 방법.** 파일을 둔 뒤 디스크를 띄운다: 정상 부팅은 BIOS/게임 화면에 도달한다. 전형적인
실패는 BIOS가 없거나 이름이 틀린 경우다 — 예로 PSX 부팅이 `Error opening scph5500.bin`으로 실패한다.
그게 보이면 파일이 없거나, 폴더가 틀렸거나, 철자가 틀린 것이다(정확한 이름과 리전 접미사를 확인).
Saturn과 달리 PSX는 BIOS 없이는 아예 부팅하지 못한다.

### 게임 ROM / 디스크 — 사용자가 반드시 제공해야 한다
emucap은 게임을 포함하지 않는다. 사용자가 ROM이나 디스크 이미지를 제공한다(CD 시스템은 `.cue`와 그
트랙 파일들; Mega Drive/HuCard는 단일 카트리지 ROM). 사용자에게 파일을 요청하고, 정확한 경로를
확인한 뒤, 그 경로를 `launch.sh`에 넘긴다(**사용** 참조).

## 빌드
```
./build.sh
```
- Mednafen 1.32.1 다운로드 → `emucap.cpp` 삽입 → 모든 emucap 훅을 fresh 소스에 perl 재주입 →
  `./configure --enable-ss --enable-psx --enable-pce --enable-pce-fast --enable-md --enable-debugger` → make.
  산출물: `work/mednafen/src/mednafen`.
- **`--enable-ss` 필수**: configure의 Saturn 자동탐지는 `host_cpu`가 `aarch64*`/`arm64*`일
  때만 켜는데, Apple Silicon이 `arm`으로 보고돼 빠진다. psx는 기본 on이나 `--enable-psx`로
  의도를 고정한다.
- **build.sh가 주입하는 훅(손편집 의존 없음·재현 가능)**: ①main.cpp 프레임 루프(`emucap_service`/
  `emucap_capture`, 공통 드라이버 경로), ②입력 주입 `emucap_apply_input(PortData[0])` —
  코어-비특이 `mednafen.cpp`의 Emulate 직전 + MidSync 두 위상(ss·psx·pce·md 공통), ③값-조건 BP 기록
  `emucap_bp_record` — Saturn은 `ss/debug.inc`(read/write 2함수), PSX는 `psx/debug.cpp`의
  `CheckCPUBPCallB`(단일 콜백), PCE는 `pce/debug.cpp`의 HuC6280 logical read/write match,
  MD는 `md/debug.cpp`의 68000 read/write match, ④입력 진단 `emucap_game_data_store` —
  `ss`·`psx`·`pce`·`pce_fast`·`md`의 gamepad update 경로.
  각 주입은 빌드 시 fixed-string으로 검증한다.
- automake 불필요: 생성된 Makefile의 OBJECTS에 직접 추가한다.
- `EMUCAP_MEDNAFEN_WORK=/path/to/build-dir`는 빈 디렉터리 또는 이전에 `build.sh`가 만든 디렉터리에만
  지정한다. 비어 있지 않은 custom work 디렉터리는 이 스크립트의 ownership marker가 없으면 거부한다.

## 사용
빌드된 바이너리는 `launch.sh`로 띄운다. 실제 포트는 MCP `status`의 `listening_port`가 정본이다
(예시는 47800). `status` 호출이 MCP listener를 세우므로 생략하지 않는다. `launch.sh`는 포트에
MCP listener가 없으면 에뮬레이터를 띄우지 않고 거부하고, 이미 연결된 에뮬레이터가 있으면 아무것도
죽이지 않으며, 자기 pidfile의 고아 Mednafen만 정리한다. emucap 연결이 확인되기 전에는 성공으로
반환하지 않는다. 연결 직후에도 기본 3초(`EMUCAP_POST_CONNECT_GRACE`) 동안 PID와 TCP 연결이 유지되는지
확인하므로, "연결됨" 출력은 즉시 사망 케이스를 통과한 뒤에만 나온다.
기본 포트별 바이너리 사본, pidfile, 로그는 OS별 emucap 데이터 루트 아래에 둔다(`EMUCAP_EMU_HOME`
override, 기본은 macOS `~/Library/Application Support/emucap`, Linux `${XDG_DATA_HOME:-~/.local/share}/emucap`,
Windows `%LOCALAPPDATA%\emucap`).
Codex 같은 transient PTY에서 부모 shell 종료가 SIGHUP을 전파하지 않도록, launcher는 `python3`가 있으면
Mednafen을 `start_new_session=True`로 새 세션에 띄우고 stdio를 로그/`/dev/null`로 분리한다(`python3`가
없을 때만 `nohup` fallback).
Codex처럼 shell command 실행 중 MCP tool call을 못 하는 환경에서는 launch 전 `status`가 background
accept/hello를 미리 준비한다. 이 절차를 생략하면 TCP만 붙고 프로토콜 왕복이 늦어져 Mednafen이 곧 끊길 수 있다.
```
# Saturn
./launch.sh "/path/to/saturn.cue" 47800
# PlayStation
./launch.sh "/path/to/psx.cue" 47800
# PC Engine CD-ROM2 / HuCard
MEDNAFEN_FORCE_MODULE=pce ./launch.sh "/path/to/pce.cue" 47800
# Mega Drive / Genesis
MEDNAFEN_FORCE_MODULE=md ./launch.sh "/path/to/game.md" 47800
```
(`launch.sh`는 기본으로 `SDL_VIDEODRIVER=dummy`·`-sound 0`을 사용한다. 화면이 필요하면
`EMUCAP_HEADLESS=0`, 소리가 필요하면 `MEDNAFEN_SOUND=1`처럼 환경변수로 조정한다. 그 밖의 환경변수:
`MEDNAFEN_BIN`(포크 바이너리 경로, 기본 `work/mednafen/src/mednafen`), `EMUCAP_LAUNCH_WAIT`(연결 대기
초, 기본 20), `EMUCAP_EMU_HOME`(emucap 데이터 루트), `EMUCAP_LOG`(로그 경로), `EMUCAP_SESSION_TOKEN`
(미지정 시 `runtime_paths.token_file`에 표시되는 포트별 토큰 파일에서 자동 로드). 빌드 버전은 `MEDNAFEN_VER`로 재정의한다.)
따라서 `launch.sh`를 썼다면 별도 `SDL_VIDEODRIVER=dummy` 재시도는 새 조치가 아니다.
로그가 `Initializing video...` 근처에서 끝나고 뒤에 `Signal has been caught ... SIGTERM`이 있으면
대개 비디오 크래시가 아니라 `launch.sh`가 연결 timeout 후 자기 프로세스를 정리한 것이다. launch 직전
`status` 재조회와 stale 포트 여부를 먼저 확인한다.
`Mednafen 연결됨` 이후 PID가 사라짐, `Broken pipe`, `CLOSE_WAIT` 같은 연결 증상은 Mednafen 로그 tail과
launch 직전 `status`/`listening_port` 재조회로 진단한다.

PCE 분석은 Debugger가 있는 exact `pce` 코어를 쓴다. 자동 판별이 `pce_fast`로 빠지거나 CUE가 애매하면
`-force_module pce`로 고정한다. PCE-CD는 파일명보다 CUE 트랙 레이아웃이 정본이다. 축약/다운로드용 CUE와
실제 원본 CUE가 공존할 수 있으므로, DATA track과 track count를 먼저 확인한다. Mednafen 로그의
unsupported `CATALOG`, missing `.sbi`는 대개 비치명 경고이며, `Using module: pce`, TOC 출력, MCP `status`
연결 여부로 성공/실패를 판정한다.

MD `.bin`은 다른 시스템 이미지와 확장자가 겹친다. header가 없거나 파일명이 애매하면
`MEDNAFEN_FORCE_MODULE=md` 또는 launch 4번째 인자 `md`로 고정한다. launcher는 `md` 모듈일 때
`-md.input.auto 0 -md.input.port1 gamepad6`을 추가해 6버튼 입력 버퍼를 고정한다.

## memory_type = 주소공간 (디버거 노출)
- **Saturn(14종)**: `workraml`(1MB)·`workramh`(1MB)·`vdp1vram`(512KB)·`vdp2vram`(512KB)·
  `vdp1fb0`/`vdp1fb1`·`scspram`(512KB)·`cram`(4KB, VDP2 팔레트 — raw 저장; CRAM_Mode로 index/색포맷
  해석)·`backup`(32KB)·`physical`(SH-2 외부 버스) 등.
- **PSX(4종)**: `cpu`(CPU 버스 32비트 — KUSEG/KSEG0/KSEG1 미러·스크래치패드·BIOS·HW를 자동
  디코드; exec BP·값읽기는 여기)·`ram`(메인 RAM 2MB 직접)·`spu`(SPU RAM 512KB)·`gpu`(VRAM 1MB).
  MIPS little-endian이라 다바이트 값 조립은 LE.
- **PCE exact(`pce`)**: `cpu`(HuC6280 16비트 logical — 현재 MPR 매핑 반영, exec/read/write BP·값읽기는 여기)·
  `physical`(21비트 물리)·`ram`(8KB, SGX는 32KB)·`vram0`(VDC VRAM, 바이트 주소)·`vram1`(SGX VDC-B VRAM, read/write BP)·`sat0`(VDC SAT)·
  `pram`(VCE palette)·`adpcm`(CD ADPCM RAM, CD 타이틀)·`acram`(Arcade Card)·`bram`·`psgram0..5`.
  HuC6280은 little-endian이다. `pce_fast`는 Debugger가 없어 이 주소공간을 노출하지 않는다.
- **MD/Mega Drive**: `cpu`(68000 24비트 CPU physical — exec/read/write BP·값읽기·disassemble은 여기)·
  `ram`(Work RAM 64KB, 공개 오프셋 0x0000 기준; read/write BP는 내부적으로 0xFF0000~0xFFFFFF CPU 주소로 매핑)·
  `zram`(Z80 RAM 8KB)·`vram`(VDP VRAM 64KB)·`cram`(VDP CRAM 128B, unpacked bus color word)·
  `vsram`(VDP VSRAM 128B)·`vdpreg`(VDP register 32B). 68000은 big-endian이다. Mednafen MD의
  `cpu` address space write는 no-op이라 `write_memory("cpu", ...)`는 어댑터가 거부한다. `vdpreg` write는
  화면 모드·IRQ·scroll table 기준을 바꿀 수 있으므로 분석 중 필요할 때만 쓰고, smoke는 read 중심으로 둔다.

버튼명: Saturn `a/b/c/x/y/z/l/r/start/방향`(`l`=`ls`·`r`=`rs` 별칭), PSX `cross/circle/triangle/square/l1/l2/r1/r2/
start/select/방향`(SNES식 `l`=`l1`·`r`=`r1` 별칭, DualShock 추가 `l3/r3`), PCE `i/ii/run/select/방향`(편의 별칭 `a/b/start`,
6버튼 `iii/iv/v/vi`), MD `a/b/c/x/y/z/mode/start/방향`. 모두 active-high다. Mednafen `IDIIS_Button*`의 세 번째 인자는
BitOffset이 아니라 ConfigOrder이며, 실제 raw bit는 코어 IDII 선언순서와 padding으로 정해진다.

## 구현 범위
- **동작·실증**: `hello`/`status`/`read_memory`/`write_memory`/`get_state`(레지스터)/
  `save_state`·`load_state`/`run_frames`/`pause`·`step`·`resume`(freeze 상태머신: frozen 시
  `emucap_service`가 스핀해 프레임 진행 차단)/`probe`(원자적 load→진행→읽기, 결정론)/
  `set_breakpoint`·`clear_breakpoint`·`clear_all_breakpoints`·`list_breakpoints`·`poll_events`
  (exec/read/write BP — `AddBreakPoint`+`SetCPUCallback`로 코어가 DebugMode 자동 전환, 히트 시
  콜백 내 스핀으로 명령 단위 freeze; read/write는 값-조건(`value`/`value_mask`/`value_len`)
  및 `pc_min`/`pc_max` 필터 지원. **write 값-조건은 어댑터가 *쓰는 값*을 주입해 전 시스템 동작한다**
  (SS=디코더 복제 21옵코드[RMW 포함]·PSX=GPR[rt] 콜백 스레딩·PCE=WriteHandler V·MD=클론버스; 폭마스킹).
  read 값-조건은 읽는 값=현재 메모리라 fallback. 보조(VDP/비디오 메모리) 주소공간 value-BP는 그 write
  경로에 값주입이 아직 없어 *조용한 무시 대신 정직 reject*(정공법은 후속). **SS write/read BP는
  memory_type(`workraml`/`workramh`/`scspram`/`vdp1vram`/`vdp2vram`/`cram`)을 SH-2 외부버스 주소로
  자동변환**해 발화한다(변환 불가 type은 `unsupported` — accept-but-never-fire 금지; cache-through 0x2x
  미러로만 가는 접근은 미커버). PCE는 `cpu` logical,
  `physical` 21비트 물리, `vram0/vram1` VDC AUX BP 지원. MD access BP는 `cpu`/`ram`/`zram`에 더해
  VDP write hook으로 `vram`/`cram`/`vsram`/`vdpreg` write BP를 지원한다. VDP read BP는 아직 미지원.
  MD `ram` BP는 0xFF0000 CPU 주소로 매핑)/
  `disassemble`(SH-2/MIPS/HuC6280/68000)/
  `find_pattern`(디버거 address space 내부 128KB 단위 패턴 검색)/
  `reset`/`set_input`·`press_buttons`(컨트롤러 주입 — `mednafen.cpp`의 PortData[0]을 버튼
  마스크로 덮어씀, active-high; tap/tap_sequence/hold_until은 Rust가 set_input+step으로 조립)/
  `screenshot`(MDFNI_Emulate 직후 espec.surface를 `PNGWrite`로 PNG 인코딩→base64)/
  `dump_memory`(디버거 AddressSpace를 `.bin`+`regions.json`으로 벌크 export — 합성 full-bus[PSX cpu 4GB·
  SS physical 128MB]는 64MB cap으로 skip[`reply.skipped` 보고], 전용 RAM/VRAM은 export)/ **Saturn 전용**:
  `get_video_state`(VDP2 per-NBG 디코드 — 유효 charno 비트폭·셀크기·bpp·도출 cellbytes·플레인(네임테이블)
  베이스 절대주소·스크롤·BGON, 필드별 raw+reg_offset; 게임 char-base 보정상수 미적용=에이전트 RE 몫)·
  `resolve_tile(nbg,x,y)`(화면좌표→char 데이터 주소 per-tile 해소, 중간값 charno·nt_addr·raw PND 동봉)/
  `set_layer_enable`(레이어 토글 — Mednafen 내장 `MDFNI_SetLayerEnableMask` 노출. `layers`[이름 배열·
  대소문자 무시]만 enable·나머지 disable, 또는 `mask`[raw], 생략 시 조회. `MDFNGameInfo->LayerNames`로
  per-system 노출: SS `NBG0/NBG1/NBG2/NBG3/RBG0/RBG1/Sprite`, MD/PCE 각자; PSX는 LayerNames 없어
  `unsupported`. 알 수 없는 이름→`bad_params`, `layers:[]`=조회(전부 disable은 `mask:0`). 마스크는 바꿀
  때까지 유지 — 분석 후 전체 enable로 복원. VDP1/VDP2 라우팅·클린플레이트 판정용).
- **지원(과거 미지원에서 추가)**: `dump_memory`(전용 RAM/VRAM은 cap 없이, 합성 full-bus는 64MB cap-skip),
  `get_rom_info`(EMUCAP_CONTENT에서 name/path/size/media_type + **content_md5**(`MDFNGameInfo->MD5` — 디스크
  레이아웃 인지·경로독립, rom_sha1로 권장) + sha1(파일)), `step_instructions`(포크 per-instruction CPU
  콜백으로 명령 단위 진행; SS는 active CPU 1명령).
- **미지원(Mesen 전용 — 이 어댑터엔 없음, error kind `unknown_method`)**:
  `watch_register`·`set_trace`/`get_trace`/`call_stack`·`break_on_reset`, set_breakpoint의 kind `nmi`/`irq`/`dma`.
  **동작 차이**: `get_state`의 `groups` 필터 무시(항상 전체 반환), 입력 `port` 무시(항상 port0), `save_state`/
  `load_state`는 frozen·running 둘 다 가능(Mesen은 running만), `poll_events`는 최소 `{pc}`를 반환하고 access
  BP는 가능한 경우 `{kind,address,length,value}`도 싣는다. MD VDP write BP 이벤트는 `memory_type`,
  `source`(`data_port`/`dma_vbus`/`dma_fill`/`dma_copy`/`control_port`), DMA 계열의 `source_address`도 싣는다
  (Mesen의 type/channels/dma snapshot 등은 없음).
- **PCE 상태**: `pce` 코어 빌드/분기/버튼/값-조건 BP 기록 경로를 추가했다. 합성 HuCard smoke는
  `cargo run --example mednafen_pce_smoke`로 검증한다. 실제 게임 입력 확인은
  `cargo run --example mednafen_pce_input_visual -- <game.cue|game.pce>`로 한다. 기본 확인 순서:
  `status.system=="pce"` → `get_state`에 HuC6280/VDC 그룹 노출 → `read_memory("cpu", 0xE060, ...)`
  → `disassemble(0xE060)` → `tap(["run"])` 또는 `tap(["start"])`.
- **MD 상태**: `md` 코어 빌드/분기/버튼/값-조건 BP 기록 경로를 추가했다. 기본 확인 순서:
  `status.system=="md"` → `read_memory("cpu", 0x100, ...)`로 SEGA header 확인 →
  reset vector를 읽어 `disassemble(reset_pc)` → `write_memory("ram", ...)` 왕복 →
  `read_memory("vram"/"cram"/"vsram"/"vdpreg"/"zram", ...)`로 runtime surface 확인 →
  `write_memory("zram"/"vram"/"cram"/"vsram", ...)` 왕복·원복 →
  `set_breakpoint(kind="write", memory_type="vram"/"vdpreg", ...)`로 VDP port/DMA write 이벤트 확인 →
  `tap(["start"])`/`press_buttons(["start"])` 후 `status.last_game_buttons`와 화면 변화를 확인한다.
- **PSX 실증(Waku Puyo Dungeon, JP)**: status·get_state(CPU/SPU/타이머)·disassemble(MIPS, raw
  바이트와 교차검증)·read_memory(cpu/ram, KSEG 미러 폴딩·LE)·write_memory·save/load_state 왕복·
  screenshot·입력 주입(title→start→DATA SELECT→처음하기→캐릭터 선택까지 메뉴 완주)·exec/write BP
  freeze까지 전부 확인.
- **입력 주입 위치**: 드라이버 `Input_Update`가 아니라 코어-비특이 `mednafen.cpp`의 Emulate
  직전(movie/netplay와 동일 위상)·MidSync에 주입한다. Input_Update 주입은 게임이 입력 스냅샷을
  읽는 위상과 어긋날 수 있다(Saturn SMPC INTBACK 경로). PSX는 SMPC가 없어 게임이 PortData를 매
  프레임 직접 읽으므로 이 주입이 그대로 메뉴를 구동한다.
- **입력 가시성**: 주입 상태(engaged·mask)는 `emucap_service` 스레드가 쓰고 적용 훅(메인
  스레드)이 읽으므로 `std::atomic`이라야 한다 — 평범한 변수면 가시성이 없어 입력이
  무입력↔입력으로 진동한다. status의 `last_game_input`은 패드 latch에 도달한 active-high 비트를
  노출한다. Saturn은 그 다음 단계가 SMPC이므로 `last_smpc_read_addr`/`last_smpc_read_value`/
  `smpc_read_mask`/`last_smpc_oreg`도 함께 본다. OREG `0x10..0x2f` 읽기와 direct-port `0x3a/0x3b`
  읽기를 구분해 "패드 latch"와 "게임-visible read"를 분리한다.
- **ROM 리로드**: 리빌드한 디스크는 **포크를 재시작**한다(kill 후 새 디스크로 재실행 →
  emucap-mcp에 자동 재접속). in-process 리로드는 드라이버 스레딩 이유로 비목표.
- **후속**: 정식 JSON 파서(현재 최소 추출).

## broker 다중 인스턴스 주의

한 broker에 같은 포크를 여러 개 붙이려면(다중 세션 격리), Mednafen은 같은 base directory로
동시 실행을 lockfile로 막으므로 `MEDNAFEN_ALLOWMULTI=1`이 필요하다(또는 인스턴스별 base dir 분리).
`launch.sh`는 이미 기본으로 `MEDNAFEN_ALLOWMULTI=1`을 설정하므로 launch.sh 경로에선 자동이고, 아래는
`<fork>`를 직접 실행할 때다. 세션 구분은 `EMUCAP_NAME`으로 한다. 예:
```
MEDNAFEN_ALLOWMULTI=1 EMUCAP_PORT=47800 EMUCAP_NAME=g1 <fork> -sound 0 <game>
MEDNAFEN_ALLOWMULTI=1 EMUCAP_PORT=47800 EMUCAP_NAME=g2 <fork> -sound 0 <game>
```
