#!/bin/bash
# Flycast(Dreamcast)에 emucap 어댑터를 주입해 빌드한다.
# FLYCAST_SRC는 읽기 전용 입력으로만 쓰고, 실제 패치와 build/는 emucap 소유 작업 트리에서 수행한다.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

emucap_data_root() {
  if [ -n "${EMUCAP_EMU_HOME:-}" ]; then
    echo "$EMUCAP_EMU_HOME"
    return
  fi
  case "$(uname -s 2>/dev/null || echo unknown)" in
    Darwin)
      echo "${HOME:-/tmp}/Library/Application Support/emucap"
      ;;
    MINGW*|MSYS*|CYGWIN*)
      if [ -n "${LOCALAPPDATA:-}" ]; then
        echo "$LOCALAPPDATA/emucap"
      elif [ -n "${APPDATA:-}" ]; then
        echo "$APPDATA/emucap"
      else
        echo "${HOME:-/tmp}/AppData/Local/emucap"
      fi
      ;;
    *)
      if [ -n "${XDG_DATA_HOME:-}" ]; then
        echo "$XDG_DATA_HOME/emucap"
      else
        echo "${HOME:-/tmp}/.local/share/emucap"
      fi
      ;;
  esac
}

DEFAULT_BUILD_HOME="$(emucap_data_root)/flycast-build"
BUILD_HOME_INPUT="${EMUCAP_FLYCAST_BUILD_HOME:-$DEFAULT_BUILD_HOME}"
CUSTOM_BUILD_HOME=0
if [ -n "${EMUCAP_FLYCAST_BUILD_HOME:-}" ]; then
  CUSTOM_BUILD_HOME=1
fi
BUILD_HOME_CREATED=0
if [ ! -d "$BUILD_HOME_INPUT" ]; then
  BUILD_HOME_CREATED=1
fi
mkdir -p "$BUILD_HOME_INPUT"
BUILD_HOME="$(cd "$BUILD_HOME_INPUT" && pwd -P)"
OWNER_FILE="$BUILD_HOME/.emucap-flycast-build-home"
UPSTREAM_CACHE="$BUILD_HOME/upstream"
SRC="${EMUCAP_FLYCAST_WORK_SRC:-$BUILD_HOME/work}"
BUILD_HOME_ABS="$BUILD_HOME"

build_home_has_entries() {
  [ -n "$(find "$BUILD_HOME" -mindepth 1 -maxdepth 1 -print -quit)" ]
}

if [ "$CUSTOM_BUILD_HOME" = "1" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ "$BUILD_HOME_CREATED" != "1" ] && build_home_has_entries; then
    echo "ERROR: EMUCAP_FLYCAST_BUILD_HOME is not empty or emucap-owned: $BUILD_HOME" >&2
    echo "       Use an empty build directory or one previously created by this script." >&2
    exit 2
  fi
fi
: >"$OWNER_FILE"

guard_path() {
  local target="$1"
  local parent base
  if [ -d "$target" ]; then
    (cd "$target" && pwd -P)
    return
  fi
  parent="$(dirname "$target")"
  base="$(basename "$target")"
  if [ ! -d "$parent" ]; then
    echo "ERROR: parent directory does not exist for Flycast work path: $target" >&2
    exit 2
  fi
  (cd "$parent" && printf '%s/%s\n' "$(pwd -P)" "$base")
}

ensure_under_build_home() {
  local target_abs
  target_abs="$(guard_path "$1")"
  case "$target_abs" in
    "$BUILD_HOME_ABS"/*) ;;
    *) echo "ERROR: emucap Flycast work path must stay under $BUILD_HOME_ABS: $target_abs" >&2; exit 1 ;;
  esac
}

safe_rm_rf() {
  ensure_under_build_home "$1"
  rm -rf -- "$1"
}

if [ -n "${FLYCAST_SRC:-}" ]; then
  UPSTREAM="$FLYCAST_SRC"
elif [ -d "${HOME:-}/flycast/core" ]; then
  UPSTREAM="$HOME/flycast"
else
  UPSTREAM="$UPSTREAM_CACHE"
  if [ ! -d "$UPSTREAM/core" ]; then
    echo "→ Flycast 소스 캐시 생성: $UPSTREAM"
    git clone --recursive https://github.com/flyinghead/flycast "$UPSTREAM"
  fi
fi
[ -d "$UPSTREAM/core" ] || { echo "ERROR: Flycast 소스 없음: $UPSTREAM (FLYCAST_SRC로 지정하거나 네트워크 clone 허용)"; exit 1; }

ensure_under_build_home "$SRC"
mkdir -p "$SRC"
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete --exclude '.git' --exclude '/build/' --exclude '/.emucap_build.lock' "$UPSTREAM"/ "$SRC"/
else
  safe_rm_rf "$SRC"
  mkdir -p "$SRC"
  (cd "$UPSTREAM" && tar --exclude './.git' --exclude './build' --exclude './.emucap_build.lock' -cf - .) | (cd "$SRC" && tar -xf -)
fi
find "$SRC" -name .git -prune -exec rm -rf {} +
echo "→ Flycast work tree 준비: $SRC (source: $UPSTREAM)"

# 공용 빌드 env 정규화(macOS homebrew LLVM 오염 걷어내기 — Apple clang/Cocoa 빌드가 깨지지 않도록).
. "$HERE/../_common/build-env.sh"
emucap_scrub_build_env

# 동시-빌드 직렬화: 다중 세션이 같은 $SRC/build에서 동시에 cmake --build하면 오브젝트가 clobber돼 레이스가
# 난다(Mednafen과 동형 이슈). macOS엔 flock이 없어 mkdir 원자 락으로 직렬화한다(stale 30분 초과는 회수).
LOCKDIR="${EMUCAP_BUILD_LOCK:-$BUILD_HOME/.build.lock}"
LOCK_PARENT="$(dirname "$LOCKDIR")"
if [ ! -d "$LOCK_PARENT" ]; then
  echo "ERROR: build lock parent directory does not exist: $LOCK_PARENT" >&2
  exit 2
fi
if [ -e "$LOCKDIR" ] && [ ! -d "$LOCKDIR" ]; then
  echo "ERROR: build lock path exists but is not a directory: $LOCKDIR" >&2
  exit 2
fi
LOCK_MARKER="$LOCKDIR/.emucap-flycast-build-lock"
_bl_waited=0
while ! mkdir "$LOCKDIR" 2>/dev/null; do
  if [ -f "$LOCK_MARKER" ] && [ -n "$(find "$LOCKDIR" -maxdepth 0 -mmin +30 2>/dev/null)" ]; then
    echo "→ stale 빌드 락 회수(30분 초과): $LOCKDIR" >&2
    rm -f "$LOCK_MARKER"
    rmdir "$LOCKDIR" 2>/dev/null || true
    continue
  fi
  [ "$_bl_waited" = 0 ] && echo "→ 다른 Flycast build.sh 진행 중 — 직렬화 대기(동시 cmake clobber 방지)…" >&2
  _bl_waited=1
  sleep 5
done
: >"$LOCK_MARKER"
trap 'rm -f "$LOCK_MARKER"; rmdir "$LOCKDIR" 2>/dev/null || true' EXIT

inject_check() {  # 주입이 실제로 들어갔는지 검증(조용한 실패 금지)
  grep -q "$1" "$2" || { echo "ERROR: 주입 실패 [$1] in $2"; exit 1; }
}

# 1. 어댑터 소스 복사
cp "$HERE/emucap.cpp" "$HERE/emucap.h" "$HERE/emucap_input.h" "$HERE/emucap_failure.cpp" "$HERE/emucap_failure.h" "$SRC/core/"
echo "→ emucap.cpp/.h + input ownership + failure serializer 복사: $SRC/core/"
# 빌드 hash: 이 .app이 어느 emucap 커밋에서 빌드됐는지 hello/status.emulator_build로 알린다(사용자가 git
# HEAD와 대조해 재빌드 필요 여부 확인 — build-time 임베드라 재빌드 안 하면 옛 hash 그대로). 미커밋이면 -dirty.
BUILD_HASH="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$HERE" diff --quiet HEAD -- emucap.cpp emucap.h emucap_input.h emucap_failure.cpp emucap_failure.h 2>/dev/null || BUILD_HASH="${BUILD_HASH}-dirty"
printf '#define EMUCAP_BUILD_HASH "%s"\n' "$BUILD_HASH" > "$SRC/core/emucap_build.h"

# 1b. 줄끝 정규화(LF). 아래 perl 앵커는 `..."\n`처럼 LF를 가정하는데, Windows에서 core.autocrlf=true로
#     클론한 트리는 CRLF라 `\r`이 앵커 매치를 깨뜨려 주입이 실패한다(inject_check가 잡지만 빌드 중단).
#     패치 대상 파일만 CRLF→LF로 바꿔 모든 앵커를 한 번에 방어한다(컴파일러는 CRLF도 받으므로 대상만).
for f in \
  core/emulator.cpp \
  core/hw/maple/maple_cfg.cpp \
  core/hw/sh4/interpr/sh4_interpreter.cpp \
  core/hw/sh4/sh4_interrupts.cpp \
  core/ui/gui.cpp \
  core/ui/mainui.cpp \
  core/cfg/option.h \
  shell/apple/emulator-osx/emulator-osx/osx-main.mm \
  CMakeLists.txt; do
  [ -f "$SRC/$f" ] && perl -i -pe 's/\r\n/\n/g' "$SRC/$f"
done

# 독립 serializer gate: upstream 헤더 없이 128 KiB 상한·R0-R15·원자 파일 교체를 먼저 검증한다.
"$HERE/test-failure.sh"
echo "→ Flycast fatal serializer 단독 회귀 테스트 통과"

# 2. emulator.cpp 훅: emucap.h include + vblank()에 emucap_service() 호출(Event::VBlank 직후).
#    근거: vblank()는 emu 스레드에서 프레임당 1회.
perl -0777 -pi -e 's/(#include "emulator\.h"\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/core/emulator.cpp"
perl -0777 -pi -e 's/(EventManager::event\(Event::VBlank\);)/${1}\n\temucap_service();/ unless m{emucap_service}' \
  "$SRC/core/emulator.cpp"
inject_check 'emucap.h' "$SRC/core/emulator.cpp"
inject_check 'emucap_service' "$SRC/core/emulator.cpp"
echo "→ emulator.cpp 훅 주입(include + vblank emucap_service)"

# Runtime `Dynarec.Enabled=no` selects the interpreter, but upstream initializes the SH4 recompiler
# first anyway. Unsigned macOS builds can SIGTRAP there before emucap connects. Defer initialization
# until the option is enabled; launch.sh also supplies a transient command-line override so the
# per-instance config suffix cannot restore the default.
# Normalize either supported injected form before inserting the hook so repeated builds stay idempotent.
perl -0777 -pi -e 's{recompiler = Get_Sh4Recompiler\(\);\n\t// emucap: do not initialize unsigned JIT when the interpreter was selected\.\n\tif\(config::DynarecEnabled\)\n\t\{\n\t\trecompiler->Init\(\);\n\t\tINFO_LOG\(DYNAREC, "Using Recompiler"\);\n\t\}}{recompiler = Get_Sh4Recompiler();\n\trecompiler->Init();\n\tif(config::DynarecEnabled)\n\t\tINFO_LOG(DYNAREC, "Using Recompiler");}' \
  "$SRC/core/emulator.cpp"
perl -0777 -pi -e 's{recompiler = Get_Sh4Recompiler\(\);\n\trecompiler->Init\(\);\n\tif\(config::DynarecEnabled\)\n\t\tINFO_LOG\(DYNAREC, "Using Recompiler"\);}{// emucap: instantiate the SH4 recompiler only when it will be initialized.\n\tif(config::DynarecEnabled)\n\t{\n\t\trecompiler = Get_Sh4Recompiler();\n\t\trecompiler->Init();\n\t\tINFO_LOG(DYNAREC, "Using Recompiler");\n\t}} unless m{emucap: instantiate the SH4 recompiler only when it will be initialized}' \
  "$SRC/core/emulator.cpp"
inject_check 'emucap: instantiate the SH4 recompiler only when it will be initialized' "$SRC/core/emulator.cpp"
echo "→ emulator.cpp interpreter 선택 시 unsigned SH4 JIT 초기화 생략"

# 2b. maple_cfg.cpp 입력 주입: 게임이 실제 입력을 읽는 소비 지점(MapleConfigMap::GetInput, emu 스레드
#    maple DMA)에서 pjs->kcode를 emucap 주입값으로 override. emu 스레드 동기라 UI 스레드 os_UpdateInputState
#    리셋과 경합 없음(결정론적 입력 — kcode[] 전역 쓰기는 경합/드롭이 났다).
perl -0777 -pi -e 's/(#include [^\n]*\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/core/hw/maple/maple_cfg.cpp"
perl -0777 -pi -e 's/(pjs->kcode = inputState\.kcode;)/${1}\n\t\tif (emucap_input_engaged()) pjs->kcode = emucap_kcode();/ unless m{emucap_input_engaged}' \
  "$SRC/core/hw/maple/maple_cfg.cpp"
inject_check 'emucap_input_engaged' "$SRC/core/hw/maple/maple_cfg.cpp"
inject_check 'emucap.h' "$SRC/core/hw/maple/maple_cfg.cpp"
echo "→ maple_cfg.cpp 입력 주입(GetInput에서 pjs->kcode override — 소비 지점, 경합 없음)"

# 2c. gui.cpp screenshot 헬퍼 2개: capture_raw(UI/GL 스레드 — renderer->GetLastFrame로 raw RGB) +
#    encode_png(GL 불필요 — stbi). 같은 TU의 renderer/appendVectorData 재사용. emucap은 매 렌더마다
#    capture_raw로 버퍼를 채우고(UI 스레드), screenshot 요청 시 encode_png로 인코딩한다(emu 스레드) →
#    freeze 중에도 동작(gui_runOnUiThread는 freeze 데드락이라 쓰지 않음).
perl -0777 -pi -e 's{(stbi_write_png_to_func\(appendVectorData, &data, width, height, 3, &rawData\[0\], 0\);\n\})}{${1}\n\n// emucap: raw 캡처(UI/GL 스레드) + PNG 인코딩(GL 불필요)\nvoid emucap_capture_raw(std::vector<u8>& out, int& w, int& h) {\n\tout.clear(); w = 0; h = 0;\n\tif (renderer != nullptr) renderer->GetLastFrame(out, w, h);\n}\nvoid emucap_encode_png(const u8* raw, int w, int h, std::vector<u8>& png) {\n\tpng.clear();\n\tstbi_flip_vertically_on_write(0);\n\tstbi_write_png_to_func(appendVectorData, &png, w, h, 3, raw, 0);\n}} unless m{emucap_capture_raw}' \
  "$SRC/core/ui/gui.cpp"
inject_check 'emucap_capture_raw' "$SRC/core/ui/gui.cpp"
echo "→ gui.cpp screenshot 헬퍼 주입(emucap_capture_raw + emucap_encode_png)"

# 2d. mainui.cpp 캡처 훅: 매 렌더(UI/GL 스레드)마다 emucap_capture_latest()로 최신 프레임 raw를 버퍼에 떠둔다.
#    os_UpdateInputState() 직후에 건다(렌더 스레드 진입점). emucap.h include도 주입.
perl -0777 -pi -e 's/(#include [^\n]*\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/core/ui/mainui.cpp"
perl -0777 -pi -e 's/(os_UpdateInputState\(\);)/${1}\n\temucap_capture_latest();/ unless m{emucap_capture_latest}' \
  "$SRC/core/ui/mainui.cpp"
inject_check 'emucap_capture_latest' "$SRC/core/ui/mainui.cpp"
inject_check 'emucap.h' "$SRC/core/ui/mainui.cpp"
perl -0777 -pi -e 's/(void mainui_stop\(\)\n\{\n\tmainui_enabled = false;)/${1}\n\temucap_notify_shutdown();/ unless m{emucap_notify_shutdown}' \
  "$SRC/core/ui/mainui.cpp"
inject_check 'emucap_notify_shutdown' "$SRC/core/ui/mainui.cpp"
echo "→ mainui.cpp 캡처 + fatal quarantine 창 닫기 훅 주입"

# 2e. sh4_interpreter always-on fatal PC ring: 매 명령 실행 직전 PC 하나만 고정 배열에 기록한다.
#    allocation/decode/lock 없이 inline store+mask라 set_trace와 독립적으로 실패 직전 512개를 보존한다.
perl -0777 -pi -e 's/(\t+)(u32 op = ReadNexOp\(\);\n\n\t+ExecuteOpcode\(op\);\n\t+\} while \(ctx->cycle_counter > 0\);)/${1}emucap_crash_pc_hook(ctx->pc);\n${1}${2}/ unless m{emucap_crash_pc_hook}' \
  "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
inject_check 'emucap_crash_pc_hook' "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
echo "→ sh4_interpreter.cpp always-on fatal PC ring 훅 주입"

# 2f. sh4_interpreter Run() 루프 exec breakpoint 훅: 매 명령 실행 전 pc가 BP면 그 자리에서 정지(명령-정밀).
#    armed(전역 bool)가 false면 bool 한 번만 봐서 핫루프 비용 0. Run()의 inner do{}만 노린다(delay slot 제외).
perl -0777 -pi -e 's/(#include [^\n]*\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
perl -0777 -pi -e 's/(\t+)(u32 op = ReadNexOp\(\);\n\n\t+ExecuteOpcode\(op\);\n\t+\} while \(ctx->cycle_counter > 0\);)/${1}if (g_emucap_bp_armed \&\& emucap_exec_bp_check(ctx->pc)) emucap_bp_spin(ctx->pc);\n${1}${2}/ unless m{emucap_exec_bp_check}' \
  "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
inject_check 'emucap_exec_bp_check' "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
inject_check 'emucap.h' "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
echo "→ sh4_interpreter.cpp exec BP 훅 주입(Run 루프 명령-정밀 정지)"

# 2g. sh4_interpreter Run() 루프 크래시경로 관측 훅: exec BP와 같은 자리에 매 명령 전 trace 훅을 추가 주입한다.
#    armed(전역 bool)가 false면 bool 한 번만 봐서 핫루프 비용 0(set_trace/watch 셋 다 off면 무회귀). 같은 원본
#    패턴(u32 op = ReadNexOp())을 노려 BP 라인 뒤에 붙는다(BP 주입은 prepend라 이 패턴은 그대로 남아 매칭).
perl -0777 -pi -e 's/(\t+)(u32 op = ReadNexOp\(\);\n\n\t+ExecuteOpcode\(op\);\n\t+\} while \(ctx->cycle_counter > 0\);)/${1}if (g_emucap_trace_armed) emucap_trace_hook(ctx->pc);\n${1}${2}/ unless m{emucap_trace_hook}' \
  "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
inject_check 'emucap_trace_hook' "$SRC/core/hw/sh4/interpr/sh4_interpreter.cpp"
echo "→ sh4_interpreter.cpp 크래시경로 관측 훅 주입(Run 루프 매 명령 trace/watch/callstack)"

# 2h. blocked SH4 exception exact capture: sr/ccn/spc/pc를 upstream이 바꾸거나 FlycastException을
#     던지기 전에 incoming EPC/event + full registers + always-on ring을 durable failure file로 쓴다.
perl -0777 -pi -e 's/(#include "types\.h"\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/core/hw/sh4/sh4_interrupts.cpp"
perl -0777 -pi -e 's{if \(Sh4cntx\.sr\.BL != 0\)\n(?:\t\{\n\t\temucap_capture_fatal_sh4\([^\n]*\);\n\t\tthrow FlycastException\("Fatal: SH4 exception when blocked"\);\n\t\}|\t\tthrow FlycastException\("Fatal: SH4 exception when blocked"\);)}{if (Sh4cntx.sr.BL != 0)\n\t{\n\t\temucap_capture_fatal_sh4("Fatal: SH4 exception when blocked", epc, (uint32_t)expEvn,\n\t\t\t(uint32_t)CCN_EXPEVT, (uint32_t)CCN_INTEVT, (uint32_t)CCN_TEA);\n\t\tthrow FlycastException("Fatal: SH4 exception when blocked");\n\t}}g' \
  "$SRC/core/hw/sh4/sh4_interrupts.cpp"
inject_check 'emucap.h' "$SRC/core/hw/sh4/sh4_interrupts.cpp"
inject_check 'emucap_capture_fatal_sh4' "$SRC/core/hw/sh4/sh4_interrupts.cpp"
[ "$(grep -c 'emucap_capture_fatal_sh4' "$SRC/core/hw/sh4/sh4_interrupts.cpp")" -eq 1 ] || {
  echo "ERROR: blocked-exception fatal hook must occur exactly once" >&2; exit 1;
}
grep -Fq '(uint32_t)CCN_EXPEVT, (uint32_t)CCN_INTEVT, (uint32_t)CCN_TEA' \
  "$SRC/core/hw/sh4/sh4_interrupts.cpp" || {
  echo "ERROR: blocked-exception hook does not preserve pre-mutation CCN context" >&2; exit 1;
}
echo "→ sh4_interrupts.cpp blocked exception exact-capture/quarantine 훅 주입"

# 3. CMakeLists에 emucap sources 추가(core 'main' target_sources 블록의 nullDC.cpp 뒤).
perl -0777 -pi -e 's{(\n\t\tcore/nullDC\.cpp\n)}{$1\t\tcore/emucap.cpp\n} unless m{core/emucap\.cpp}' \
  "$SRC/CMakeLists.txt"
perl -0777 -pi -e 's{(\n\t\tcore/emucap\.cpp\n)}{$1\t\tcore/emucap_failure.cpp\n} unless m{core/emucap_failure\.cpp}' \
  "$SRC/CMakeLists.txt"
if [ "${EMUCAP_FLYCAST_DISABLE_CRASH_RING:-0}" = "1" ]; then
  perl -0777 -pi -e 's{(\n\t\tcore/debug/gdb_server\.h\)\n)}{$1\ntarget_compile_definitions(flycast PRIVATE EMUCAP_DISABLE_CRASH_PC_RING)\n} unless m{EMUCAP_DISABLE_CRASH_PC_RING}' \
    "$SRC/CMakeLists.txt"
  inject_check 'target_compile_definitions(flycast PRIVATE EMUCAP_DISABLE_CRASH_PC_RING)' "$SRC/CMakeLists.txt"
  echo "→ benchmark-only crash PC ring 비활성 빌드"
fi
inject_check 'core/emucap.cpp' "$SRC/CMakeLists.txt"
inject_check 'core/emucap_failure.cpp' "$SRC/CMakeLists.txt"
echo "→ CMakeLists.txt target_sources에 emucap + fatal serializer 추가"

# 3b. macOS 필수 빌드 픽스(클린 upstream엔 없음 — 텍스처 디버그 mods와 무관한 빌드 자체 픽스).
#     (1) enable_language(OBJC): macOS는 .mm(Objective-C)라 OBJC 언어 활성 없으면 generate가
#         'CMAKE_OBJC_COMPILE_OBJECT 미설정'으로 실패한다. (2) zlib: -lz → CommandLineTools libz.tbd(여러 SDK).
#     APPLE 블록(set(ZLIB_LIBRARY ...)) 안에 주입하므로 macOS에서만 적용된다.
perl -0777 -pi -e 's{set\(ZLIB_LIBRARY "-lz"}{set(ZLIB_LIBRARY "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/lib/libz.tbd"} unless m{libz\.tbd}' \
  "$SRC/CMakeLists.txt"
perl -0777 -pi -e 's/(set\(ZLIB_LIBRARY [^\n]*\n)/${1}\tenable_language(OBJC)\n/ unless m{enable_language\(OBJC\)}' \
  "$SRC/CMakeLists.txt"
# 두 주입은 앵커가 다르니 각각 검증한다(공유 토큰 하나로 묶으면 한쪽 앵커 드리프트가 조용히 통과).
inject_check 'libz.tbd' "$SRC/CMakeLists.txt"
inject_check 'enable_language(OBJC)' "$SRC/CMakeLists.txt"
echo "→ CMakeLists.txt macOS 빌드 픽스(enable_language OBJC + zlib)"

# 3c. Syphon(macOS 비디오라우팅) 비활성: emucap에 불필요 + enable_language(OBJC) 시 Syphon PCH(C17)와
#     충돌해 빌드 실패. CMake 블록은 if(FALSE)로 래핑, osx-main.mm의 무가드 Syphon GL 블록은 #if 0로.
#     (Vk 블록 242~276은 이미 #ifdef USE_VULKAN — USE_VULKAN=OFF라 자동 제외.)
perl -0777 -pi -e 's/(add_subdirectory\(core\/deps\/Syphon\))/if(FALSE) # emucap: Syphon off\n\t\t\t$1/ unless m{emucap: Syphon off}' \
  "$SRC/CMakeLists.txt"
perl -0777 -pi -e 's/(target_compile_definitions\(\$\{PROJECT_NAME\} PRIVATE VIDEO_ROUTING\)\n)(\s*\n\s*target_sources)/${1}\t\t\tendif() # emucap: Syphon off\n${2}/ unless m{endif\(\) # emucap: Syphon off}' \
  "$SRC/CMakeLists.txt"
# if(FALSE)/endif() 짝을 각각 검증한다 — 공유 토큰 'emucap: Syphon off' 하나면 endif 앵커가
# 드리프트해 unbalanced if(FALSE)가 돼도 첫 코멘트에 토큰이 남아 조용히 통과한다(나중에 CMake 에러).
inject_check 'if(FALSE) # emucap: Syphon off' "$SRC/CMakeLists.txt"
inject_check 'endif() # emucap: Syphon off' "$SRC/CMakeLists.txt"
OSXMAIN="$SRC/shell/apple/emulator-osx/emulator-osx/osx-main.mm"
perl -0777 -pi -e 's/(#import <Syphon\/Syphon\.h>)/#if 0 \/\/ emucap: Syphon 비활성\n$1/ unless m{emucap: Syphon 비활성}' "$OSXMAIN"
perl -0777 -pi -e 's/(syphonGLServer = NULL;\n\})/$1\n#endif \/\/ emucap: Syphon 비활성/ unless m{#endif \/\/ emucap: Syphon 비활성}' "$OSXMAIN"
# #if 0/#endif 짝을 각각 검증한다 — 공유 토큰 하나면 #endif 앵커 드리프트가 unbalanced #if 0를
# 남겨도 첫 코멘트에 토큰이 있어 조용히 통과한다(나중에 컴파일 에러).
inject_check '#if 0 // emucap: Syphon 비활성' "$OSXMAIN"
inject_check '#endif // emucap: Syphon 비활성' "$OSXMAIN"
echo "→ Syphon 비활성(CMake if(FALSE) + osx-main.mm #if 0)"

# 3d. upstream(dev) 이식성 버그 우회: core/cfg/option.h의 calcDbPower가 std::min(double,float)로 호출돼
#     이 clang에서 'no matching function for call to min'(dreamconn.cpp 등). std::min<float>로 타입 강제.
perl -0777 -pi -e 's/std::min\(std::exp\(4\.605f/std::min<float>(std::exp(4.605f/ unless m{std::min<float>\(std::exp}' \
  "$SRC/core/cfg/option.h"
perl -0777 -pi -e 's/(#include <type_traits>\n)/${1}#include <algorithm>\n/ unless m{#include <algorithm>}' \
  "$SRC/core/cfg/option.h"
# 두 주입은 앵커가 다르니 각각 검증한다(std::min<float> 캐스트 + <algorithm> include).
inject_check 'std::min<float>(std::exp' "$SRC/core/cfg/option.h"
inject_check '#include <algorithm>' "$SRC/core/cfg/option.h"
echo "→ option.h upstream min() 이식성 버그 우회"

# 3e. MoltenVK post-build 복사 가드: USE_VULKAN=OFF인데도 post-build가 "$VULKAN_SDK/lib/libMoltenVK.dylib"
#     (VULKAN_SDK 미설정 → /lib/...)를 복사하려다 실패 → make가 링크된 바이너리를 삭제한다. if(USE_VULKAN)로 가드.
perl -0777 -pi -e 's{(add_custom_command\(TARGET \$\{PROJECT_NAME\} POST_BUILD\s*\n\s*COMMAND \$\{CMAKE_COMMAND\} -E copy "\$ENV\{VULKAN_SDK\}/lib/libMoltenVK\.dylib"\s*\n\s*\$<TARGET_FILE_DIR:flycast>/\.\./Frameworks/libvulkan\.dylib\))}{if(USE_VULKAN AND DEFINED ENV{VULKAN_SDK})\n\t\t\t$1\n\t\t\tendif() # emucap: MoltenVK 복사 가드}s unless m{emucap: MoltenVK 복사 가드}' \
  "$SRC/CMakeLists.txt"
inject_check 'emucap: MoltenVK 복사 가드' "$SRC/CMakeLists.txt"
echo "→ MoltenVK post-build 복사 가드(if USE_VULKAN)"

# 4. 빌드(증분). build/ 없으면 configure(연구문서의 플래그). Unix Makefiles라 CMakeLists 변경 시
#    cmake --build가 자동 재구성하며 새 파일을 잡는다.
JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
# emucap의 정규화된 env로 configure한 build/만 재사용한다. CMakeCache는 있는데 스탬프가 없는 트리는 오염된
#   env로 굳었을 수 있어 clean 재생성한다(오염 캐시는 OBJC 컴파일 규칙 누락으로 generate가 깨진다).
STAMP="$SRC/build/.emucap_configured"
if [ -f "$SRC/build/CMakeCache.txt" ] && [ ! -f "$STAMP" ]; then
  echo "→ emucap 스탬프 없는 기존 build/ 감지 → clean 재configure"
  safe_rm_rf "$SRC/build"
fi
if [ ! -f "$SRC/build/CMakeCache.txt" ]; then
  echo "→ configure (clean env)"
  if [ "$(uname -s)" = "Darwin" ]; then
    # env는 emucap_scrub_build_env가 이미 정규화. -DCMAKE_*_COMPILER로 Apple clang까지 명시 고정.
    cmake -S "$SRC" -B "$SRC/build" -DCMAKE_BUILD_TYPE=Release -DENABLE_GDB_SERVER=ON -DUSE_VULKAN=OFF \
      -DUSE_BREAKPAD=OFF -DCMAKE_OSX_ARCHITECTURES=arm64 \
      -DCMAKE_C_COMPILER=/usr/bin/clang -DCMAKE_CXX_COMPILER=/usr/bin/clang++ \
      -DCMAKE_OBJC_COMPILER=/usr/bin/clang -DCMAKE_OBJCXX_COMPILER=/usr/bin/clang++
  else
    # Linux 등: 시스템 기본 컴파일러.
    cmake -S "$SRC" -B "$SRC/build" -DCMAKE_BUILD_TYPE=Release -DENABLE_GDB_SERVER=ON -DUSE_VULKAN=OFF \
      -DUSE_BREAKPAD=OFF
  fi
  touch "$STAMP"
fi
echo "→ cmake --build (-j$JOBS)"
cmake --build "$SRC/build" -j"$JOBS"
if [ -x "$SRC/build/Flycast.app/Contents/MacOS/Flycast" ]; then
  echo "✓ 빌드 완료: $SRC/build/Flycast.app/Contents/MacOS/Flycast"
elif [ -x "$SRC/build/flycast" ]; then
  echo "✓ 빌드 완료: $SRC/build/flycast"
else
  echo "✓ 빌드 완료: $SRC/build"
fi
