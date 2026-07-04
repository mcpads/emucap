# desmume-nds — Nintendo DS adapter (headless DeSmuME + GDB-RSP bridge)

emucap가 Nintendo DS를 지원하는 어댑터. 상용 DeSmuME를 **headless로 포크·빌드**하고, DeSmuME가 내장한
**ARM9/ARM7 GDB 스텁**에 emucap **NDS 브리지**가 붙어 메모리·레지스터·스텝·브레이크포인트를 GDB-RSP로
제어한다. PC-98 어댑터와 같은 GDB-브리지 구도다.

## 아키텍처

```
emucap Core ──emucap 프로토콜(TCP)──▶ emucap-desmume-nds-bridge ──GDB-RSP──▶ desmume-cli (headless)
                                                              ├─ 127.0.0.1:PORT+1000 = ARM9 스텁
                                                              └─ 127.0.0.1:PORT+1001 = ARM7 스텁
```

- DeSmuME는 듀얼 CPU(ARM9 ~67MHz / ARM7 ~33MHz)라 **CPU마다 GDB 스텁·포트가 하나씩**이다. 브리지가
  둘 다에 붙고, 도구는 기본 ARM9에 라우팅되며 `arm7` memory_type(또는 `cpu` 파라미터)으로 ARM7을 지정한다.
- upstream `desmume-cli`는 X11(`XInitThreads`)과 OpenGL SDL 창을 무조건 만들어 디스플레이 없이는 못 뜬다.
  `patches/0001-headless-cli.patch`가 X11·창·렌더러 생성을 걷어내고 emulate 루프 + GDB 스텁만 남긴다
  (`#define HEADLESS_SPIKE` 게이트, 원본은 `#else`에 보존). 어댑터가 이 포크를 소유한다(Mednafen/Flycast 방식).

## 빌드

```sh
adapters/desmume-nds/build.sh
```

TASEmulators/desmume를 emucap 소유 work 트리(`adapters/desmume-nds/work`)에 clone하고, upstream HEAD로
리셋한 뒤 패치 스택(`0001` headless → `0002` screenshot/input → `0003` savestate/disasm)을 순서대로 적용하고
meson으로 `desmume-cli`를 빌드한다(`-Dfrontend-cli -Dgdb-stub`; gdb-stub은 JIT을 끄고 인터프리터로 돈다).
0003이 0002와 같은 gdbstub.cpp 영역을 확장하므로 build.sh는 매 빌드마다 트리를 리셋해 스택 전체를 재적용한다.
읽기 전용 upstream 체크아웃이 있으면 `EMUCAP_DESMUME_SRC=/path/to/desmume`로 지정한다.

- 의존: `meson` `ninja` `sdl2` `glib`(+ macOS 시스템 `libpcap`). `git`.
- macOS는 Apple clang(`/usr/bin/clang`)로 빌드한다 — build.sh가 homebrew LLVM 오염을 정규화한다.

## 실행

에이전트는 `launch(content_path=<rom.nds>, system="nds")`(MCP 도구)로 띄운다. 내부적으로:

```sh
adapters/desmume-nds/launch.sh <rom.nds> <EMUCAP_PORT> [EMUCAP_NAME]
```

이 launcher가 desmume-cli를 headless로(`--arm9gdb PORT+1000 --arm7gdb PORT+1001`) 띄우고 두 GDB 포트가 열리길
기다린 뒤 브리지를 붙인다. **NDS BIOS/펌웨어는 필요 없다** — DeSmuME HLE BIOS + direct-boot로 상용 롬이 부팅한다.

## 시스템·콘텐츠

- 시스템 이름: `nds`(별칭 `ds`, `nintendo-ds`, `desmume`). 콘텐츠 확장자: `.nds`.

## memory_types

`status.memory_types`가 정본. v1:

| memory_type | 라우팅 CPU | 뜻 |
|---|---|---|
| `main` | ARM9 | Main RAM `0x02000000`~ (4MB, ARM9/ARM7 공유). 게임 상태가 여기 있다. |
| `arm9` | ARM9 | ARM9 버스 전체(offset=절대주소). |
| `arm7` | ARM7 | ARM7 버스 전체(offset=절대주소). |

## 버튼

`a` `b` `x` `y` `l` `r` `start` `select` `up` `down` `left` `right`. 별칭 `enter/return→start`, `l1→l`, `r1→r`.
마이크는 이름으로 주입하지 않는다. **터치스크린은 별도 `touch` 도구(화면 좌표)로 주입**한다 — 아래 Tier 2.

## 도구 가용성 — Tier 1 / Tier 2

**Tier 1(GDB 스텁으로 동작 — 구현됨)**: `read_memory`·`write_memory`(RSP `m`/`M`)·`get_state`(ARM 레지스터
r0-r15·pc·sp·lr·cpsr, `g`)·`step`/`step_instructions`(`s`)·`set_breakpoint`(exec=`Z0`)·`clear_breakpoint`·
`pause`/`resume`(break/`c`)·`poll_events`.

**Tier 2(GDB 밖 — fork가 소유한 custom RSP hook, `patches/0002-emucap-hooks.patch`)**:
- `screenshot` — fork의 `qEmucap,ss`가 `GPU->GetDisplayInfo().masterNativeBuffer16`(두 화면 256×384)를
  PNG로 인코딩(zlib)·base64로 반환. 브리지가 `{png_base64, width:256, height:384}`로 준다. (headless는 draw를
  스킵하지만 GPU 렌더 결과는 버퍼에 있다 — backlight 스케일만 생략.)
- `set_input`/`press_buttons` — 버튼명→12비트 마스크→fork의 `QEmucap,input:<mask>[,<frames>]`. fork가
  `NDS_beginProcessingInput`에서 매 프레임 override를 folding(프론트엔드의 per-frame 리셋을 이김). ARM9로 주입.
  press_buttons의 프레임 카운트다운은 에뮬이 running일 때만 진행하므로 resume/continue로 프레임을 흘려보낸다.
- `touch` — 하단 터치스크린(256×192)을 `(x,y)`에서 터치. fork의 `QEmucap,touch:<hexX>,<hexY>[,<hexframes>]`
  (또는 `:release`) → `NDS_setEmucapTouchOverride`가 위 버튼 override와 대칭으로 `NDS_beginProcessingInput`에서
  매 프레임 fold(`NDS_applyFinalInput`이 적용). `frames`>0이면 그만큼 누른 뒤 자동으로 뗌(탭), `release:true`면
  뗌, 둘 다 없으면 다음 touch까지 hold. 버튼 `tap`(버튼 탭)과 구분되는 **화면 좌표 터치**다. Love Plus류 터치전용
  타이틀 진행에 필수. `patches/0005-emucap-touch.patch`. (검증: "Touch Start"→touch(128,96)→메인메뉴 진행.)

**Tier 3(fork custom RSP hook, `patches/0003-emucap-state-disasm.patch`; call_stack만 브리지 전용)**:
- `save_state`/`load_state` — fork의 `QEmucap,{save,load}state:<hexpath>`가 DeSmuME 네이티브
  `savestate_save`/`savestate_load`(saves.cpp)를 호출. path는 RSP-안전하게 hex 인코딩. 반환 `{path, status}`.
  savestate는 전역 상태(두 코어+PPU/SPU)라 ARM9 연결로 보낸다 — 정지 상태에서 호출한다.
- `disassemble` — fork의 `qEmucap,disasm:<addr>,<count>[,<mode>]`가 DeSmuME 디스어셈블러 테이블
  (`des_{arm,thumb}_instructions_set`)로 명령을 해독해 `<addr>|<opcode>|<text>` 행들을 base64로 반환. mode
  생략 시 해당 CPU의 CPSR T-bit로 ARM/Thumb 자동판별(`arm`/`thumb`로 강제 가능). 브리지가 `[{addr, bytes, text}]`
  로 파싱(`bytes`는 리틀엔디언 메모리 순서). CPU는 `cpu` 파라미터로 라우팅(ARM9 기본).
- `call_stack` — **fork 변경 없는 브리지 전용 best-effort**. `g`로 pc/lr/sp/r11을 읽고, frame0=pc,
  frame1=lr, 이후 ARM APCS r11 프레임포인터 체인(`[fp-4]`=saved lr, `[fp-12]`=saved fp)을 `m`으로 워크한다.
  게임이 r11을 프레임포인터로 쓰지 않으면 얕게 끝난다 — 반환에 `method:"lr+fp-walk (best-effort)"`·`note`,
  프레임마다 `in_code_region` 플래그를 붙여 표기한다.
- `reset` — fork의 `QEmucap,reset`이 DeSmuME `NDS_Reset`을 호출(파워사이클). 두 코어가 HLE direct-boot
  엔트리(PC=`0x02000800`)로 돌아가 halted 상태로 남는다(스텁 브레이크포인트는 리셋을 넘어 유지).

**아직 미지원(추가 fork hook 필요)**: `run_frames`(프레임 카운터)·`watch_register`·`set_trace`/`get_trace`·
`break_on_reset`. `status.capability_notes`가 정본(인터페이스에 캐비엇을 누적하지 않는다 —
이름은 공통, 가용성은 status).

## 듀얼 CPU 실행 모델 (중요)

DeSmuME는 ARM9/ARM7을 lockstep으로 돌리는데 GDB 스텁은 CPU마다 독립이다. **두 CPU를 동시에 continue하면
안 깨진 CPU가 깨진 CPU를 브레이크포인트 너머로 끌고 가는 레이스**가 실측으로 확인됐다. 그래서 `resume`는
기본적으로 **ARM9(주 CPU)만 continue**한다 — ARM9 브레이크포인트가 결정론적이고, ARM7은 frozen 상태로
남아 메모리·레지스터를 그대로 읽을 수 있다.

- `resume` (기본) → ARM9만 실행. exec BP가 정확한 주소에서 결정론적으로 히트.
- `resume(cpu:"arm7")` → ARM7만 실행.
- `resume(cpu:"both")` → 둘 다 실행(레이스 — 결정론적 BP 보장 안 됨). ARM7이 실제로 돌아야 하는 구간에만.

읽기·쓰기·레지스터·스텝은 두 CPU 모두 frozen 상태에서 동작한다. `memory_type`/`cpu`로 대상 CPU를 고른다.

## 운영 주의

- **halt-on-start**: 스텁은 정지 상태로 시작한다. 브리지가 붙은 뒤 `resume`/`step`으로 구동한다.
- **SIGTERM 무시**: desmume-cli는 `kill`(SIGTERM)에 안 죽는다 — launcher는 SIGKILL로 정리한다.
- **인터프리터**: gdb-stub 빌드는 JIT을 끈다(Flycast와 동형).
- **`M`(쓰기) 응답**: DeSmuME는 쓰기를 수행하되 `M`에 "OK"가 아닌 빈 패킷을 답한다 — 브리지가 빈 응답을
  성공으로, `E`-코드만 실패로 처리한다.
