#!/usr/bin/env bash
# Mednafen 어댑터 재현 빌드(Saturn + PlayStation + PC Engine + Mega Drive).
#
# Mednafen은 GPL이라 통째 벤더링/재배포하지 않는다. 우리 추가분(emucap.cpp/.h)만 이 저장소에
# 두고, 업스트림 Mednafen을 로컬에서 받아 패치·빌드한다.
#
# 한 바이너리가 ss(Saturn)·psx(PlayStation)·pce(PC Engine)·md(Mega Drive)를 모두 처리한다(모두 컴파일·링크).
# emucap이 런타임에 CurGame->shortname으로 시스템을 분기한다(주소공간·버튼·엔디안). 이 스크립트는 fresh
# 추출 후 모든 emucap 훅을 perl로 재주입하므로 손편집에 의존하지 않는다 — 재현 가능한 빌드.
#
# automake가 없어도 되도록, ./configure가 만든 Makefile을 직접 편집해 emucap.cpp를 추가한다.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
LOCK_FILE="$HERE/upstream.lock"

lock_value() {
  sed -n "s/^$1=//p" "$LOCK_FILE"
}

VER="$(lock_value MEDNAFEN_VERSION)"
URL="$(lock_value MEDNAFEN_URL)"
MEDNAFEN_SHA256="$(lock_value MEDNAFEN_SHA256)"
[ -n "$VER" ] && [ -n "$URL" ] &&
  printf '%s' "$MEDNAFEN_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "ERROR: invalid Mednafen upstream lock: $LOCK_FILE" >&2
  exit 1
}
DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_MEDNAFEN_WORK:-$DEFAULT_WORK}"
CUSTOM_WORK=0
if [ -n "${EMUCAP_MEDNAFEN_WORK:-}" ]; then
  CUSTOM_WORK=1
fi
WORK_CREATED=0
if [ ! -d "$WORK_INPUT" ]; then
  WORK_CREATED=1
fi
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"  # 세션별 병렬 빌드 격리용 오버라이드(기본 공유 work/ + 락 직렬화)
OWNER_FILE="$WORK/.emucap-mednafen-work"
SRC="$WORK/mednafen"
TARBALL="$WORK/mednafen-$VER.tar.xz"

abs_child_path() {
  local path="$1"
  local parent base
  parent="$(dirname "$path")"
  base="$(basename "$path")"
  if [ ! -d "$parent" ]; then
    echo "ERROR: parent directory does not exist for $path" >&2
    exit 2
  fi
  printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$base"
}

safe_rm_rf_under_work() {
  local target="$1"
  local abs_target
  abs_target="$(abs_child_path "$target")"
  case "$abs_target" in
    "$WORK"/*) ;;
    *)
      echo "ERROR: refusing to remove path outside EMUCAP_MEDNAFEN_WORK: $target" >&2
      exit 2
      ;;
  esac
  if [ "$abs_target" = "$WORK" ] || [ "$abs_target" = "/" ] || [ -z "$abs_target" ]; then
    echo "ERROR: refusing to remove unsafe path: $target" >&2
    exit 2
  fi
  rm -rf -- "$abs_target"
}

work_has_entries() {
  [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]
}

if [ "$CUSTOM_WORK" = "1" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ "$WORK_CREATED" != "1" ] && work_has_entries; then
    echo "ERROR: EMUCAP_MEDNAFEN_WORK is not empty or emucap-owned: $WORK" >&2
    echo "       Use an empty build directory or one previously created by this script." >&2
    exit 2
  fi
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "Mednafen"
: >"$OWNER_FILE"

sha256_path() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "ERROR: shasum or sha256sum is required" >&2
    return 1
  fi
}

# 1. 소스(캐시)
if [ ! -f "$TARBALL" ]; then
  echo "→ Mednafen $VER 다운로드"
  DOWNLOAD="$TARBALL.download.$$"
  rm -f "$DOWNLOAD"
  if ! curl -fsSL -o "$DOWNLOAD" "$URL"; then
    rm -f "$DOWNLOAD"
    exit 1
  fi
  GOT_MEDNAFEN_SHA256="$(sha256_path "$DOWNLOAD")"
  if [ "$GOT_MEDNAFEN_SHA256" != "$MEDNAFEN_SHA256" ]; then
    rm -f "$DOWNLOAD"
    echo "ERROR: downloaded Mednafen archive checksum mismatch" >&2
    echo "  expected=$MEDNAFEN_SHA256" >&2
    echo "  actual=$GOT_MEDNAFEN_SHA256" >&2
    exit 1
  fi
  mv "$DOWNLOAD" "$TARBALL"
fi
GOT_MEDNAFEN_SHA256="$(sha256_path "$TARBALL")"
[ "$GOT_MEDNAFEN_SHA256" = "$MEDNAFEN_SHA256" ] || {
  echo "ERROR: Mednafen archive checksum mismatch: $TARBALL" >&2
  echo "  expected=$MEDNAFEN_SHA256" >&2
  echo "  actual=$GOT_MEDNAFEN_SHA256" >&2
  exit 1
}

# 2. 깨끗이 추출
echo "→ 추출"
safe_rm_rf_under_work "$SRC"; mkdir -p "$SRC"
tar xf "$TARBALL" -C "$SRC" --strip-components=1

# 3. 우리 소켓 클라이언트
cp "$HERE/emucap.cpp" "$HERE/emucap.h" "$HERE/emucap_input.h" "$SRC/src/drivers/"
# 빌드 hash: 이 .app이 어느 emucap 커밋에서 빌드됐는지 hello/status.emulator_build로 알리게 한다 —
# 사용자가 `git rev-parse --short HEAD`와 대조해 재빌드 필요 여부를 확인한다(build-time 임베드라 재빌드
# 안 하면 옛 hash 그대로다). 어댑터 production source가 HEAD와 다르면(미커밋) -dirty.
BUILD_HASH="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$HERE" diff --quiet HEAD -- emucap.cpp emucap.h emucap_input.h 2>/dev/null || BUILD_HASH="${BUILD_HASH}-dirty"
BUILD_HASH="${BUILD_HASH}@mednafen-$VER"
printf '#define EMUCAP_BUILD_HASH "%s"\n' "$BUILD_HASH" > "$SRC/src/drivers/emucap_build.h"

# 헬퍼: perl 주입 후 마커(고정 문자열)가 들어갔는지 검증. fresh 빌드가 조용히 깨지는 것을 막는다.
inject_check() { grep -qF "$1" "$2" || { echo "ERROR: $3"; exit 1; }; }
count_of() { grep -cF "$1" "$2" 2>/dev/null || true; }

# 4. main.cpp 훅: emucap.h include + 프레임 루프 서비스 호출(MDFNI_Emulate 직후, SoftFB 직전).
#    화면 캡처(emucap_capture)도 여기 — ss·psx·pce·md 공통 드라이버 경로라 screenshot 동작.
perl -0777 -pi -e 's/(#include "main\.h"\n)/${1}#include "emucap.h"\n/ unless m{emucap\.h}' \
  "$SRC/src/drivers/main.cpp"
perl -0777 -pi -e \
  's/^([ \t]*)(SoftFB\[SoftFB_BackBuffer\]\.rect = espec\.DisplayRect;)/${1}{ static uint64_t emucap_frame = 0; ::emucap_service(emucap_frame++); ::emucap_capture((const void*)espec.surface, (const void*)\&espec.DisplayRect, (const void*)espec.LineWidths); }\n${1}${2}/m unless m{emucap_service}' \
  "$SRC/src/drivers/main.cpp"
inject_check emucap_capture "$SRC/src/drivers/main.cpp" "main.cpp 훅 삽입 실패"

# 4b. 입력 주입(코어-비특이): mednafen.cpp의 movie/netplay와 동일 위상(Emulate 직전 + MidSync)에서
#     PortData[0]을 주입한다. 드라이버 Input_Update 주입은 게임 INTBACK이 읽는 스냅샷과 위상이
#     어긋나 누락될 수 있어 폐기. PortData[0]은 ss·psx·pce·md 공통이라 한 곳으로 네 시스템을 커버한다.
perl -0777 -pi -e 's/(#include "qtrecord\.h"\n)/${1}\nextern "C" void emucap_apply_input(unsigned char*, unsigned);\n/ unless m{emucap_apply_input}' \
  "$SRC/src/mednafen.cpp"
# (a) Emulate 직전: movie/netplay ProcessInput 직후, if(qtrecorder) 앞.
perl -0777 -pi -e 's{(MDFNMOV_ProcessInput\(PortData, PortDataLen, MDFNGameInfo->PortInfo\.size\(\)\);\n)(\n if\(qtrecorder\))}{${1}\n { if(PortData[0]) emucap_apply_input(PortData[0], PortDataLen[0]); }${2}}' \
  "$SRC/src/mednafen.cpp"
# (b) MidSync 위상: 게임 INTBACK이 프레임 중간 스냅샷을 읽는 경우 대비(주석 앵커로 식별).
perl -0777 -pi -e 's{(// Call even during netplay[^\n]*\n\s*MDFNMOV_ProcessInput\(PortData, PortDataLen, MDFNGameInfo->PortInfo\.size\(\)\);\n)}{${1}  { if(PortData[0]) emucap_apply_input(PortData[0], PortDataLen[0]); }\n}' \
  "$SRC/src/mednafen.cpp"
[ "$(count_of 'emucap_apply_input(PortData[0], PortDataLen[0])' "$SRC/src/mednafen.cpp")" -ge 2 ] || { echo "ERROR: mednafen.cpp 입력주입 2지점 삽입 실패"; exit 1; }

# 4c. 값-조건 BP 기록 — Saturn(ss/debug.inc): read/write BP 매칭 시 접근 주소/길이/유형 기록.
perl -0777 -pi -e 's/( bool inss;\n\} DBG;\n)/${1}\nextern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write);\n/ unless m{emucap_bp_record}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's{(DBG\.BreakPointsRead\b.*?DBG\.FoundBPoint = true;\n)( *return;)}{${1}    emucap_bp_record(len, addr, 0);\n${2}}s' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's{(DBG\.BreakPointsWrite\b.*?DBG\.FoundBPoint = true;\n)( *return;)}{${1}    emucap_bp_record(len, addr, 1);\n${2}}s' \
  "$SRC/src/ss/debug.inc"
[ "$(count_of 'emucap_bp_record(len, addr' "$SRC/src/ss/debug.inc")" -ge 2 ] || { echo "ERROR: ss/debug.inc BP기록 2함수 삽입 실패"; exit 1; }

# 4d. 입력 진단 — Saturn(ss/input/gamepad.cpp): 게임이 실제 읽은 입력 비트 기록(status 노출).
perl -0777 -pi -e 's/(#include "gamepad\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/ss/input/gamepad.cpp"
perl -0777 -pi -e 's{(buttons = \(~\(data\[0\] \| \(data\[1\] << 8\)\)\) &~ 0x3000;\n)}{${1} emucap_game_data_store((unsigned short)(data[0] | (data[1] << 8)));\n}' \
  "$SRC/src/ss/input/gamepad.cpp"
inject_check 'emucap_game_data_store((unsigned short)(data' "$SRC/src/ss/input/gamepad.cpp" "ss/gamepad.cpp 입력진단 삽입 실패"

# 4e. Saturn SMPC 진단 — 게임이 실제 읽은 OREG/direct-port 값을 status에 노출한다.
perl -0777 -pi -e 's/(#include "input\/multitap\.h"\n)/${1}\nextern "C" void emucap_smpc_read_store(unsigned, unsigned, const unsigned char*, unsigned);\n/ unless m{emucap_smpc_read_store}' \
  "$SRC/src/ss/smpc.cpp"
perl -0777 -pi -e 's{(\tret = OREG\[\(size_t\)A - 0x10\];\n)}{${1}\t::emucap_smpc_read_store(A, ret, OREG, sizeof(OREG));\n} unless m{emucap_smpc_read_store\(A, ret, OREG}' \
  "$SRC/src/ss/smpc.cpp"
perl -0777 -pi -e 's{(\tret = \(ret & 0x80\) \| IOBusState\[0\];\n)}{${1}\t::emucap_smpc_read_store(A, ret, OREG, sizeof(OREG));\n} unless m{IOBusState\[0\];\n\t::emucap_smpc_read_store}' \
  "$SRC/src/ss/smpc.cpp"
perl -0777 -pi -e 's{(\tret = \(ret & 0x80\) \| IOBusState\[1\];\n)}{${1}\t::emucap_smpc_read_store(A, ret, OREG, sizeof(OREG));\n} unless m{IOBusState\[1\];\n\t::emucap_smpc_read_store}' \
  "$SRC/src/ss/smpc.cpp"
[ "$(count_of 'emucap_smpc_read_store(A, ret, OREG' "$SRC/src/ss/smpc.cpp")" -ge 3 ] || { echo "ERROR: ss/smpc.cpp SMPC read 진단 삽입 실패"; exit 1; }

# 4f. 값-조건 write BP에 *쓰는 값* 주입 — PSX(psx/{cpu.h,cpu.cpp,debug.cpp}).
#     PSX의 BP 매칭은 명령 디코드(CheckBreakpoints, 실행 *전*)에서 일어나 단일 콜백
#     (CheckCPUBPCallB)으로 모인다. 콜백 시그니처에 value를 스레딩해, store 옵코드가
#     GPR[rt]을 폭마스크해 넘기면 write BP의 value 필터가 *쓰는 값*과 비교돼 실제로 동작한다.
#     (MD는 clone-68000 실제-write 훅으로 같은 효과 — PSX는 디코드 지점이라 GPR 복제가 정답.)
#     일반 케이스 SB(0x28)&0xFF / SH(0x29)&0xFFFF / SW(0x2B) 전체 완전 지원. load·부분워드
#     store(SWL 0x2A/SWR 0x2E)·coprocessor store(SWC2 0x3A)는 값 출처가 GPR[rt]이 아니거나
#     바이트 루프라 value=0(희귀 — 일반 케이스는 완전).
#     read BP는 무변경: 읽는 값=현재 메모리라 emucap_read_value_for_bp fallback이 정확하므로,
#     read는 has_value=false인 emucap_bp_record로 두고 write만 emucap_bp_record_value로 보낸다
#     (MD 동형). read를 value=0로 emucap_bp_record_value에 보내면 has_value=true가 돼 read
#     value-조건 BP가 0에만 매칭되는 회귀가 생긴다(emucap.cpp:1693 분기).
# ⑴ cpu.h: 콜백 타입에 value 파라미터.
perl -0777 -pi -e 's{void CheckBreakpoints\(void \(\*callback\)\(bool write, uint32 address, unsigned int len\), uint32 instr\);}{void CheckBreakpoints(void (*callback)(bool write, uint32 address, unsigned int len, uint32 value), uint32 instr);} unless m{unsigned int len, uint32 value}' \
  "$SRC/src/psx/cpu.h"
inject_check 'void (*callback)(bool write, uint32 address, unsigned int len, uint32 value)' "$SRC/src/psx/cpu.h" "psx/cpu.h 콜백 타입 value 추가 실패"
# ⑵ cpu.cpp: CheckBreakpoints 시그니처 + store 옵코드 GPR[rt] 폭마스크. rt는 ITYPE이 정의.
perl -0777 -pi -e 's{void PS_CPU::CheckBreakpoints\(void \(\*callback\)\(bool write, uint32 address, unsigned int len\), uint32 instr\)}{void PS_CPU::CheckBreakpoints(void (*callback)(bool write, uint32 address, unsigned int len, uint32 value), uint32 instr)} unless m{unsigned int len, uint32 value}' \
  "$SRC/src/psx/cpu.cpp"
# store: 옵코드별 BEGIN_OPF 앵커로 그 블록의 첫 callback만 폭마스크 값으로 변환(SWL/SWR과 충돌 방지).
perl -0777 -pi -e 's{(BEGIN_OPF\(0x28, 0\);.*?)callback\(true, address, 1\);}{${1}callback(true, address, 1, GPR[rt] & 0xFF);}s' \
  "$SRC/src/psx/cpu.cpp"
perl -0777 -pi -e 's{(BEGIN_OPF\(0x29, 0\);.*?)callback\(true, address, 2\);}{${1}callback(true, address, 2, GPR[rt] & 0xFFFF);}s' \
  "$SRC/src/psx/cpu.cpp"
perl -0777 -pi -e 's{(BEGIN_OPF\(0x2B, 0\);.*?)callback\(true, address, 4\);}{${1}callback(true, address, 4, GPR[rt]);}s' \
  "$SRC/src/psx/cpu.cpp"
# 나머지(load·SWL·SWR·SWC2·LWC2): value=0 추가. 이미 4번째 인자를 가진 store는 매칭 안 돼 idempotent.
perl -0777 -pi -e 's{callback\((false|true), address, (\d)\);}{callback($1, address, $2, 0);}g' \
  "$SRC/src/psx/cpu.cpp"
inject_check 'void PS_CPU::CheckBreakpoints(void (*callback)(bool write, uint32 address, unsigned int len, uint32 value)' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp CheckBreakpoints 시그니처 value 추가 실패"
inject_check 'callback(true, address, 1, GPR[rt] & 0xFF);' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp SB value 주입 실패"
inject_check 'callback(true, address, 2, GPR[rt] & 0xFFFF);' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp SH value 주입 실패"
inject_check 'callback(true, address, 4, GPR[rt]);' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp SW value 주입 실패"
inject_check 'callback(false, address, 1, 0);' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp load value=0 주입 실패"
inject_check 'callback(true, address, 1, 0);' "$SRC/src/psx/cpu.cpp" "psx/cpu.cpp SWL/SWR value=0 주입 실패"
# ⑶ debug.cpp: CheckCPUBPCallB 시그니처에 value + write만 emucap_bp_record_value로(read는 fallback 유지).
perl -0777 -pi -e 's{(void CheckCPUBPCallB\(bool write, uint32 address, unsigned int len)\)}{extern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write);\nextern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value);\n${1}, uint32 value)} unless m{emucap_bp_record_value}' \
  "$SRC/src/psx/debug.cpp"
perl -0777 -pi -e 's{(if\(address >= bpit->A\[0\] && address <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = true;)}{${1}   if(write) emucap_bp_record_value(len, address, 1, value);\n   else emucap_bp_record(len, address, 0);\n${2}}' \
  "$SRC/src/psx/debug.cpp"
inject_check 'void CheckCPUBPCallB(bool write, uint32 address, unsigned int len, uint32 value)' "$SRC/src/psx/debug.cpp" "psx/debug.cpp CheckCPUBPCallB 시그니처 value 추가 실패"
inject_check 'if(write) emucap_bp_record_value(len, address, 1, value);' "$SRC/src/psx/debug.cpp" "psx/debug.cpp write value 기록 삽입 실패"
inject_check 'else emucap_bp_record(len, address, 0);' "$SRC/src/psx/debug.cpp" "psx/debug.cpp read fallback 기록 삽입 실패"

# 4g. 입력 진단 — PSX(psx/input/gamepad.cpp): UpdateInput이 읽은 버튼 비트(d8[0..1]) 기록.
perl -0777 -pi -e 's/(#include "gamepad\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/psx/input/gamepad.cpp"
perl -0777 -pi -e 's{(buttons\[0\] = d8\[0\];\n\s*buttons\[1\] = d8\[1\];\n)}{${1} emucap_game_data_store((unsigned short)(d8[0] | (d8[1] << 8)));\n}' \
  "$SRC/src/psx/input/gamepad.cpp"
inject_check 'emucap_game_data_store((unsigned short)(d8' "$SRC/src/psx/input/gamepad.cpp" "psx/gamepad.cpp 입력진단 삽입 실패"

# 4h. 값-조건 BP 기록 — PCE(pce/debug.cpp): HuC6280 logical read/write BP 매칭 지점에 주입.
#     pce_fast는 Debugger가 없으므로 분석용 BP 대상은 정확도 우선의 pce 코어다.
#     write BP: DECLFW(WriteHandler)=void WriteHandler(uint32 A, uint8 V) — V가 스코프에 있어 직접 주입.
perl -0777 -pi -e 's/(static bool FoundBPoint = false;\n)/${1}extern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write);\nextern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value);\n/ unless m{emucap_bp_record}' \
  "$SRC/src/pce/debug.cpp"
perl -0777 -pi -e 's{(static DECLFR\(ReadHandler\).*?if\(testA >= bpit->A\[0\] && testA <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = 1;)}{${1}   emucap_bp_record(1, testA, 0);\n${2}}s' \
  "$SRC/src/pce/debug.cpp"
perl -0777 -pi -e 's{(static DECLFW\(WriteHandler\).*?if\(testA >= bpit->A\[0\] && testA <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = 1;)}{${1}   emucap_bp_record_value(1, testA, 1, V);\n${2}}s unless m{emucap_bp_record_value\(1, testA}' \
  "$SRC/src/pce/debug.cpp"
[ "$(count_of 'emucap_bp_record(1, testA' "$SRC/src/pce/debug.cpp")" -ge 1 ] || { echo "ERROR: pce/debug.cpp BP기록(read) 삽입 실패"; exit 1; }
inject_check 'emucap_bp_record_value(1, testA, 1, V)' "$SRC/src/pce/debug.cpp" "pce/debug.cpp write BP 값주입 실패"

# 4i. 입력 진단 — PCE exact/fast: 게임패드가 실제 읽은 16비트 버튼 버퍼 기록(status 노출).
perl -0777 -pi -e 's/(#include "gamepad\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/pce/input/gamepad.cpp"
perl -0777 -pi -e 's{(buttons = MDFN_de16lsb\(data\);\n)}{${1} emucap_game_data_store((unsigned short)buttons);\n}' \
  "$SRC/src/pce/input/gamepad.cpp"
inject_check 'emucap_game_data_store((unsigned short)buttons)' "$SRC/src/pce/input/gamepad.cpp" "pce/gamepad.cpp 입력진단 삽입 실패"
perl -0777 -pi -e 's/(#include "input\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/pce_fast/input.cpp"
perl -0777 -pi -e 's{(pce_jp_data\[x\] = new_data;\n)}{${1}   if(x == 0) emucap_game_data_store((unsigned short)new_data);\n}' \
  "$SRC/src/pce_fast/input.cpp"
inject_check 'emucap_game_data_store((unsigned short)new_data)' "$SRC/src/pce_fast/input.cpp" "pce_fast/input.cpp 입력진단 삽입 실패"

# 4j. 값-조건 BP 기록 — MD(md/debug.cpp): 68000 read/write BP 매칭 지점에 접근 주소/길이 기록.
#     write BP는 clone 68000으로 실제 쓰기 전 감지되므로, value filter 정확도를 위해 write 값을 직접 기록한다.
perl -0777 -pi -e 's/(static bool FoundBPoint(?: = false)?;\n)/${1}extern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write);\nextern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value);\n/ unless m{emucap_bp_record}' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{(static MDFN_FASTCALL uint8 DBG_BusRead8\(uint32 address\).*?if\(address >= bpit->A\[0\] && address <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = true;)}{${1}   emucap_bp_record(1, address, 0);\n${2}}s' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{(static MDFN_FASTCALL uint16 DBG_BusRead16\(uint32 address\).*?if\(\(address \| 1\) >= bpit->A\[0\] && address <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = true;)}{${1}   emucap_bp_record(2, address, 0);\n${2}}s' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{(static MDFN_FASTCALL void DBG_BusWrite8\(uint32 address, uint8 value\).*?if\(address >= bpit->A\[0\] && address <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = true;)}{${1}   emucap_bp_record(1, address, 1);\n${2}}s' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{(static MDFN_FASTCALL void DBG_BusWrite16\(uint32 address, uint16 value\).*?if\(\(address \| 1\) >= bpit->A\[0\] && address <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = true;)}{${1}   emucap_bp_record(2, address, 1);\n${2}}s' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{emucap_bp_record\(1, address, 1\);}{emucap_bp_record_value(1, address, 1, value);}g' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{emucap_bp_record\(2, address, 1\);}{emucap_bp_record_value(2, address, 1, value);}g' \
  "$SRC/src/md/debug.cpp"
[ "$(count_of 'emucap_bp_record' "$SRC/src/md/debug.cpp")" -ge 6 ] || { echo "ERROR: md/debug.cpp BP기록 삽입 실패"; exit 1; }
inject_check 'extern "C" void emucap_bp_record' "$SRC/src/md/debug.cpp" "md/debug.cpp BP 선언 삽입 실패"
inject_check 'emucap_bp_record_value(1, address, 1, value)' "$SRC/src/md/debug.cpp" "md/debug.cpp write8 값기록 삽입 실패"
inject_check 'emucap_bp_record_value(2, address, 1, value)' "$SRC/src/md/debug.cpp" "md/debug.cpp write16 값기록 삽입 실패"

# 4k. MD 디버거 address space 확장 — upstream md/debug.cpp는 cpu/ram만 등록한다.
#     분석 작업에 필요한 Z80 RAM, VDP VRAM/CRAM/VSRAM/register를 side-effect-aware accessor로 노출한다.
perl -0777 -pi -e 's{(\n private:\n)}{\n void DBG_GetVRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n {\n  while(Length--)\n  {\n   *Buffer++ = READ_BYTE_LSB(vram, Address \& 0xFFFF);\n   Address++;\n  }\n }\n\n void DBG_PutVRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n {\n  while(Length--)\n  {\n   const uint32 a = Address \& 0xFFFF;\n   const uint8 v = *Buffer++;\n   if((a \& sat_base_mask) == satb)\n    sat[a \& sat_addr_mask] = v;\n   if(v != READ_BYTE_LSB(vram, a))\n   {\n    WRITE_BYTE_LSB(vram, a, v);\n    MARK_BG_DIRTY(a);\n   }\n   Address++;\n  }\n }\n\n void DBG_GetCRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n {\n  while(Length--)\n  {\n   const uint16 d = UNPACK_CRAM(cram[(Address >> 1) \& 0x3F]);\n   *Buffer++ = (Address \& 1) ? (d \& 0xFF) : ((d >> 8) \& 0xFF);\n   Address++;\n  }\n }\n\n void DBG_PutCRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n {\n  while(Length--)\n  {\n   const uint32 a = Address \& 0x7F;\n   uint16 d = UNPACK_CRAM(cram[(a >> 1) \& 0x3F]);\n   if(a \& 1) d = (d \& 0xFF00) | *Buffer++;\n   else d = (d \& 0x00FF) | (*Buffer++ << 8);\n   const uint16 old_addr = addr;\n   addr = a \& 0x7E;\n   WriteCRAM(d);\n   addr = old_addr;\n   Address++;\n  }\n }\n\n void DBG_GetVSRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n {\n  while(Length--)\n  {\n   const uint16 d = vsram[(Address >> 1) \& 0x3F];\n   *Buffer++ = (Address \& 1) ? (d \& 0xFF) : ((d >> 8) \& 0xFF);\n   Address++;\n  }\n }\n\n void DBG_PutVSRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n {\n  while(Length--)\n  {\n   const uint32 a = Address \& 0x7F;\n   uint16 *p = &vsram[(a >> 1) \& 0x3F];\n   if(a \& 1) *p = (*p \& 0xFF00) | *Buffer++;\n   else *p = (*p \& 0x00FF) | (*Buffer++ << 8);\n   Address++;\n  }\n }\n\n void DBG_GetVDPReg(uint32 Address, uint32 Length, uint8 *Buffer)\n {\n  while(Length--)\n  {\n   *Buffer++ = reg[Address \& 0x1F];\n   Address++;\n  }\n }\n\n void DBG_PutVDPReg(uint32 Address, uint32 Length, const uint8 *Buffer)\n {\n  while(Length--)\n  {\n   vdp_reg_w(Address \& 0x1F, *Buffer++);\n   Address++;\n  }\n }\n${1}} unless m{DBG_GetVRAM}' \
  "$SRC/src/md/vdp.h"
inject_check 'DBG_GetVRAM' "$SRC/src/md/vdp.h" "md/vdp.h debug accessor 삽입 실패"
perl -0777 -pi -e 's# void DBG_GetVRAM.*?\n private:# void DBG_GetVRAM(uint32 Address, uint32 Length, uint8 *Buffer);\n void DBG_PutVRAM(uint32 Address, uint32 Length, const uint8 *Buffer);\n void DBG_GetCRAM(uint32 Address, uint32 Length, uint8 *Buffer);\n void DBG_PutCRAM(uint32 Address, uint32 Length, const uint8 *Buffer);\n void DBG_GetVSRAM(uint32 Address, uint32 Length, uint8 *Buffer);\n void DBG_PutVSRAM(uint32 Address, uint32 Length, const uint8 *Buffer);\n void DBG_GetVDPReg(uint32 Address, uint32 Length, uint8 *Buffer);\n void DBG_PutVDPReg(uint32 Address, uint32 Length, const uint8 *Buffer);\n\n private:#s' \
  "$SRC/src/md/vdp.h"
perl -0777 -pi -e 's#(\n// Only used for DMA fill and VRAM->VRAM DMA copy\.)#\nvoid MDVDP::DBG_GetVRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n{\n while(Length--)\n {\n  *Buffer++ = READ_BYTE_LSB(vram, Address \& 0xFFFF);\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_PutVRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n{\n while(Length--)\n {\n  const uint32 a = Address \& 0xFFFF;\n  const uint8 v = *Buffer++;\n  if((a \& sat_base_mask) == satb)\n   sat[a \& sat_addr_mask] = v;\n  if(v != READ_BYTE_LSB(vram, a))\n  {\n   WRITE_BYTE_LSB(vram, a, v);\n   MARK_BG_DIRTY(a);\n  }\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_GetCRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n{\n while(Length--)\n {\n  const uint16 d = UNPACK_CRAM(cram[(Address >> 1) \& 0x3F]);\n  *Buffer++ = (Address \& 1) ? (d \& 0xFF) : ((d >> 8) \& 0xFF);\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_PutCRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n{\n while(Length--)\n {\n  const uint32 a = Address \& 0x7F;\n  uint16 d = UNPACK_CRAM(cram[(a >> 1) \& 0x3F]);\n  if(a \& 1) d = (d \& 0xFF00) | *Buffer++;\n  else d = (d \& 0x00FF) | (*Buffer++ << 8);\n  const uint16 old_addr = addr;\n  addr = a \& 0x7E;\n  WriteCRAM(d);\n  addr = old_addr;\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_GetVSRAM(uint32 Address, uint32 Length, uint8 *Buffer)\n{\n while(Length--)\n {\n  const uint16 d = vsram[(Address >> 1) \& 0x3F];\n  *Buffer++ = (Address \& 1) ? (d \& 0xFF) : ((d >> 8) \& 0xFF);\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_PutVSRAM(uint32 Address, uint32 Length, const uint8 *Buffer)\n{\n while(Length--)\n {\n  const uint32 a = Address \& 0x7F;\n  uint16 *p = &vsram[(a >> 1) \& 0x3F];\n  if(a \& 1) *p = (*p \& 0xFF00) | *Buffer++;\n  else *p = (*p \& 0x00FF) | (*Buffer++ << 8);\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_GetVDPReg(uint32 Address, uint32 Length, uint8 *Buffer)\n{\n while(Length--)\n {\n  *Buffer++ = reg[Address \& 0x1F];\n  Address++;\n }\n}\n\nvoid MDVDP::DBG_PutVDPReg(uint32 Address, uint32 Length, const uint8 *Buffer)\n{\n while(Length--)\n {\n  vdp_reg_w(Address \& 0x1F, *Buffer++);\n  Address++;\n }\n}\n$1#s unless m{MDVDP::DBG_GetVRAM}' \
  "$SRC/src/md/vdp.cpp"
inject_check 'void MDVDP::DBG_GetVRAM' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP debug accessor 정의 삽입 실패"
inject_check 'void MDVDP::DBG_PutCRAM' "$SRC/src/md/vdp.cpp" "md/vdp.cpp CRAM debug accessor 정의 삽입 실패"

perl -0777 -pi -e 's{( else if\(!strcmp\(name, "ram"\)\)\n \{\n  while\(Length--\)\n  \{\n   \*Buffer = Main68K_BusPeek8\(\(Address \& 0xFFFF\) \| 0xFF0000\);\n   Address\+\+;\n   Buffer\+\+;\n  \}\n \}\n)}{${1} else if(!strcmp(name, "zram"))\n {\n  while(Length--)\n  {\n   *Buffer++ = zram[Address \& 0x1FFF];\n   Address++;\n  }\n }\n else if(!strcmp(name, "vram"))\n  MainVDP.DBG_GetVRAM(Address, Length, Buffer);\n else if(!strcmp(name, "cram"))\n  MainVDP.DBG_GetCRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vsram"))\n  MainVDP.DBG_GetVSRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vdpreg"))\n  MainVDP.DBG_GetVDPReg(Address, Length, Buffer);\n}s unless m{!strcmp\(name, "zram"\)}' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{( else if\(!strcmp\(name, "ram"\)\)\n \{\n  while\(Length--\)\n  \{\n   Main68K_BusPoke8\(\(Address \& 0xFFFF\) \| 0xFF0000, \*Buffer\);\n   Address\+\+;\n   Buffer\+\+;\n  \}\n \}\n)}{${1} else if(!strcmp(name, "zram"))\n {\n  while(Length--)\n  {\n   zram[Address \& 0x1FFF] = *Buffer++;\n   Address++;\n  }\n }\n else if(!strcmp(name, "vram"))\n  MainVDP.DBG_PutVRAM(Address, Length, Buffer);\n else if(!strcmp(name, "cram"))\n  MainVDP.DBG_PutCRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vsram"))\n  MainVDP.DBG_PutVSRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vdpreg"))\n  MainVDP.DBG_PutVDPReg(Address, Length, Buffer);\n}s unless m{!strcmp\(name, "zram"\).*DBG_PutVRAM}s' \
  "$SRC/src/md/debug.cpp"
# Fallback: upstream whitespace has drifted across MD debug.cpp revisions; match from the function header
# to the one-space-indented ram block close so a failed exact substitution cannot silently register dead spaces.
perl -0777 -pi -e 's#(static void GetAddressSpaceBytes[^{]*\{.*?else if\(!strcmp\(name, "ram"\)\)\n \{\n.*?^ \}\n)(^ \}\n)#$1 else if(!strcmp(name, "zram"))\n {\n  while(Length--)\n  {\n   *Buffer++ = zram[Address \& 0x1FFF];\n   Address++;\n  }\n }\n else if(!strcmp(name, "vram"))\n  MainVDP.DBG_GetVRAM(Address, Length, Buffer);\n else if(!strcmp(name, "cram"))\n  MainVDP.DBG_GetCRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vsram"))\n  MainVDP.DBG_GetVSRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vdpreg"))\n  MainVDP.DBG_GetVDPReg(Address, Length, Buffer);\n$2#ms unless m{MainVDP\.DBG_GetVRAM}' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's#(static void PutAddressSpaceBytes[^{]*\{.*?else if\(!strcmp\(name, "ram"\)\)\n \{\n.*?^ \}\n)(^ \}\n)#$1 else if(!strcmp(name, "zram"))\n {\n  while(Length--)\n  {\n   zram[Address \& 0x1FFF] = *Buffer++;\n   Address++;\n  }\n }\n else if(!strcmp(name, "vram"))\n  MainVDP.DBG_PutVRAM(Address, Length, Buffer);\n else if(!strcmp(name, "cram"))\n  MainVDP.DBG_PutCRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vsram"))\n  MainVDP.DBG_PutVSRAM(Address, Length, Buffer);\n else if(!strcmp(name, "vdpreg"))\n  MainVDP.DBG_PutVDPReg(Address, Length, Buffer);\n$2#ms unless m{MainVDP\.DBG_PutVRAM}' \
  "$SRC/src/md/debug.cpp"
perl -0777 -pi -e 's{( ASpace_Add\(GetAddressSpaceBytes, PutAddressSpaceBytes, "ram", "Work RAM", 16\);\n)}{${1} ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "zram", "Z80 RAM", 13);\n ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "vram", "VDP VRAM", 16);\n ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "cram", "VDP CRAM", 7);\n ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "vsram", "VDP VSRAM", 7);\n ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "vdpreg", "VDP registers", 5);\n} unless m{ASpace_Add\(GetAddressSpaceBytes, PutAddressSpaceBytes, "vram"}' \
  "$SRC/src/md/debug.cpp"
inject_check 'ASpace_Add(GetAddressSpaceBytes, PutAddressSpaceBytes, "vram"' "$SRC/src/md/debug.cpp" "md/debug.cpp VDP address space 등록 실패"
inject_check 'MainVDP.DBG_GetVRAM(Address, Length, Buffer)' "$SRC/src/md/debug.cpp" "md/debug.cpp VDP read branch 삽입 실패"
inject_check 'MainVDP.DBG_PutVRAM(Address, Length, Buffer)' "$SRC/src/md/debug.cpp" "md/debug.cpp VDP write branch 삽입 실패"
inject_check '*Buffer++ = zram[Address & 0x1FFF]' "$SRC/src/md/debug.cpp" "md/debug.cpp zram read branch 삽입 실패"
inject_check 'zram[Address & 0x1FFF] = *Buffer++' "$SRC/src/md/debug.cpp" "md/debug.cpp zram write branch 삽입 실패"

# 4l. MD VDP write trace — Mednafen debugger는 VDP VRAM/CRAM/VSRAM write BP를 제공하지 않는다.
#     코어 VDP data/DMA write 경로에 직접 hook을 심어 port/DMA side-effect를 poll_events로 보낸다.
perl -0777 -pi -e 's/(#include "hvc\.h"\n)/${1}\nextern "C" void emucap_md_vdp_write(const char*, unsigned, unsigned, unsigned, unsigned, const char*, unsigned);\n/ unless m{emucap_md_vdp_write}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's/(namespace MDFN_IEN_MD\n\{\n)/${1}\nstatic const char* emucap_vdp_write_source = "data_port";\nstatic uint32 emucap_vdp_write_source_addr = 0xFFFFFFFFu;\nstatic INLINE void emucap_md_vdp_record_write(const char* memory_type, uint32 address, unsigned length, uint32 value)\n{\n emucap_md_vdp_write(memory_type, address, length, value, Main68K.GetRegister(M68K::GSREG_PC), emucap_vdp_write_source, emucap_vdp_write_source_addr);\n}\n/ unless m{emucap_md_vdp_record_write}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(vdp_reg_w\(r, d\);\n)}{${1}            emucap_md_vdp_write("vdpreg", r, 1, d, Main68K.GetRegister(M68K::GSREG_PC), "control_port", 0xFFFFFFFFu);\n} unless m{emucap_md_vdp_write\("vdpreg"}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(INLINE void MDVDP::MemoryWrite8\(uint8 data\)\n\{\n)}{${1} const uint16 emucap_write_addr = addr;\n} unless m{MemoryWrite8\(uint8 data\)\n\{\n const uint16 emucap_write_addr}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(INLINE void MDVDP::MemoryWrite16\(uint16 data\)\n\{\n)}{${1} const uint16 emucap_write_addr = addr;\n} unless m{MemoryWrite16\(uint16 data\)\n\{\n const uint16 emucap_write_addr}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(                MARK_BG_DIRTY\(addr\);\n            \}\n)(            break;)}{${1}            emucap_md_vdp_record_write("vram", emucap_write_addr \& 0xFFFF, 1, data);\n${2}} unless m{emucap_md_vdp_record_write\("vram", emucap_write_addr \& 0xFFFF, 1, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(\t    WriteCRAM\(data\);\n)(\t    break;)}{${1}\t    emucap_md_vdp_record_write("cram", emucap_write_addr \& 0x7F, 1, data);\n${2}} unless m{emucap_md_vdp_record_write\("cram", emucap_write_addr \& 0x7F, 1, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(            vsram\[\(addr \& 0x7E\) >> 1\] = data;\n)(            break;)}{${1}            emucap_md_vdp_record_write("vsram", emucap_write_addr \& 0x7F, 1, data);\n${2}} unless m{emucap_md_vdp_record_write\("vsram", emucap_write_addr \& 0x7F, 1, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(                MARK_BG_DIRTY\(addr\);\n            \}\n)(            break;\n\n        case 0x03: /\* CRAM \*/)}{${1}            emucap_md_vdp_record_write("vram", emucap_write_addr \& 0xFFFE, 2, data);\n${2}} unless m{emucap_md_vdp_record_write\("vram", emucap_write_addr \& 0xFFFE, 2, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(        case 0x03: /\* CRAM \*/\n            WriteCRAM\(data\);\n)(\s*break;)}{${1}            emucap_md_vdp_record_write("cram", emucap_write_addr \& 0x7E, 2, data);\n${2}} unless m{emucap_md_vdp_record_write\("cram", emucap_write_addr \& 0x7E, 2, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(            vsram\[\(addr \& 0x7E\) >> 1\] = data;\n)(            break;\n \})}{${1}            emucap_md_vdp_record_write("vsram", emucap_write_addr \& 0x7E, 2, data);\n${2}} unless m{emucap_md_vdp_record_write\("vsram", emucap_write_addr \& 0x7E, 2, data}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(    //printf\("%04x, %d\\n", data, scanline\);\n)(    MemoryWrite16\(data\);)}{${1}    emucap_vdp_write_source = "data_port";\n    emucap_vdp_write_source_addr = 0xFFFFFFFFu;\n${2}} unless m{emucap_vdp_write_source = "data_port";\n    emucap_vdp_write_source_addr = 0xFFFFFFFFu;\n    MemoryWrite16\(data\);}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{uint16 temp = vdp_dma_r\(\(DMASource \& 0x7FFFFF\) << 1\);}{uint32 emucap_dma_source = (DMASource \& 0x7FFFFF) << 1;\n             uint16 temp = vdp_dma_r(emucap_dma_source);} unless m{emucap_dma_source = \(DMASource}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(	     DMASource = \(DMASource \& 0xFF0000\) \| \(\(DMASource \+ 1\) \& 0xFFFF\);\n)(	     MemoryWrite16\(temp\);)}{${1}\t     emucap_vdp_write_source = "dma_vbus";\n\t     emucap_vdp_write_source_addr = emucap_dma_source;\n${2}} unless m{emucap_vdp_write_source = "dma_vbus"}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(\n\s*MemoryWrite8\(dma_fill_latch\);)}{\n             emucap_vdp_write_source = "dma_fill";\n             emucap_vdp_write_source_addr = 0xFFFFFFFFu;${1}} unless m{emucap_vdp_write_source = "dma_fill"}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(\s*uint8 temp = READ_BYTE_LSB\(vram, DMASource \& 0xFFFF\);\n)(\s*WRITE_BYTE_LSB\(vram, addr, temp\);)}{${1}             uint32 emucap_copy_source = DMASource \& 0xFFFF;\n${2}} unless m{emucap_copy_source = DMASource}' \
  "$SRC/src/md/vdp.cpp"
perl -0777 -pi -e 's{(\n\s*addr = \(addr \+ reg\[15\]\) \& 0xFFFF;)}{\n             emucap_md_vdp_write("vram", addr \& 0xFFFF, 1, temp, Main68K.GetRegister(M68K::GSREG_PC), "dma_copy", emucap_copy_source);${1}} unless m{emucap_md_vdp_write\("vram", addr \& 0xFFFF, 1, temp}' \
  "$SRC/src/md/vdp.cpp"
inject_check 'emucap_md_vdp_record_write("vram"' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP write hook 삽입 실패"
inject_check 'emucap_md_vdp_write("vdpreg"' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP register hook 삽입 실패"
inject_check 'emucap_vdp_write_source = "dma_vbus"' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP DMA source hook 삽입 실패"
inject_check 'emucap_vdp_write_source = "dma_fill"' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP DMA fill hook 삽입 실패"
inject_check 'emucap_md_vdp_record_write("cram", emucap_write_addr & 0x7E, 2, data)' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP CRAM16 hook 삽입 실패"
inject_check 'emucap_md_vdp_write("vram", addr & 0xFFFF, 1, temp' "$SRC/src/md/vdp.cpp" "md/vdp.cpp VDP copy hook 삽입 실패"

# 4m. 입력 진단 — MD gamepad2/3/6: 실제 UpdatePhysicalState가 받은 raw bit를 기록한다.
perl -0777 -pi -e 's/(#include "gamepad\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/md/input/gamepad.cpp"
perl -0777 -pi -e 's{(void Gamepad2::UpdatePhysicalState\(const void \*data\)\n\{\n buttons = \*\(uint8 \*\)data;\n)}{${1} emucap_game_data_store((unsigned short)buttons);\n}' \
  "$SRC/src/md/input/gamepad.cpp"
perl -0777 -pi -e 's{(void Gamepad3::UpdatePhysicalState\(const void \*data\)\n\{\n buttons = \*\(uint8 \*\)data;\n)}{${1} emucap_game_data_store((unsigned short)buttons);\n}' \
  "$SRC/src/md/input/gamepad.cpp"
perl -0777 -pi -e 's{(void Gamepad6::UpdatePhysicalState\(const void \*data\)\n\{\n .*?buttons = MDFN_de16lsb\(\(uint8 \*\)data\);\n)}{${1} emucap_game_data_store((unsigned short)buttons);\n}s' \
  "$SRC/src/md/input/gamepad.cpp"
[ "$(count_of 'emucap_game_data_store((unsigned short)buttons)' "$SRC/src/md/input/gamepad.cpp")" -ge 3 ] || { echo "ERROR: md/gamepad.cpp 입력진단 삽입 실패"; exit 1; }

# 4n. Saturn VDP2 관측성 — RawRegs/CRAM은 ss/vdp2.cpp의 파일-스코프 static이라 어댑터/디버거가
#     못 본다. 전역 accessor(PeekRawReg/PeekCRAM)를 추가하고, CRAM(팔레트)을 읽기전용 AddressSpace
#     "cram"으로 디버거에 노출한다(후속 get_video_state/resolve_tile의 팔레트 읽기 경로).
#     - PeekCRAM: CRAM[2048](4KB)의 *raw 내부 저장* 워드를 (a>>1)&0x7FF로 노출한다. 이는
#       RGB555_1024/2048 모드에선 CPU 버스순서와 일치하지만, RGB888_1024/illegal(CRAM_Mode>=2)
#       에선 vdp2.cpp:797/813의 bank-swizzle ((cri>>1)&0x3FF)|((cri&1)<<10) 을 적용해야 버스순서가
#       된다. 즉 "cram"은 vdp2vram처럼 raw palette-RAM 덤프이고, index swizzle·RGB888 2-word 색포맷
#       해석은 decode 층(get_video_state/resolve_tile이 CRAM_Mode로 CacheCRE 재현)의 몫이다.
#       ("버스 순서"라 칭하지 않는다 — raw 저장 노출이다.)
#     - get case: vdp2vram과 동형의 big-endian 바이트 추출.
#     - Put은 no-op(read-only): CRAM 포크는 렌더러 측 별도 CRAM[]+ColorCache(CRAM_Mode 의존,
#       VDP2REND 경유)도 갱신해야 하므로 관측성 읽기 경로 범위를 벗어난다.
perl -0777 -pi -e 's#(uint8 PeekVRAM\(uint32 addr\)\n)#uint16 PeekRawReg(uint32 a)\n{\n return RawRegs[(a >> 1) & 0xFF];\n}\n\nuint16 PeekCRAM(uint32 a)\n{\n return CRAM[(a >> 1) & 0x7FF];\n}\n\n${1}# unless m{PeekRawReg}' \
  "$SRC/src/ss/vdp2.cpp"
inject_check 'uint16 PeekRawReg(uint32 a)' "$SRC/src/ss/vdp2.cpp" "ss/vdp2.cpp PeekRawReg 정의 삽입 실패"
inject_check 'uint16 PeekCRAM(uint32 a)' "$SRC/src/ss/vdp2.cpp" "ss/vdp2.cpp PeekCRAM 정의 삽입 실패"
perl -0777 -pi -e 's#(void PokeVRAM\(uint32 addr, const uint8 val\) MDFN_COLD;\n)#${1}uint16 PeekRawReg(uint32 a) MDFN_COLD;\nuint16 PeekCRAM(uint32 a) MDFN_COLD;\n# unless m{PeekRawReg}' \
  "$SRC/src/ss/vdp2.h"
inject_check 'uint16 PeekCRAM(uint32 a) MDFN_COLD;' "$SRC/src/ss/vdp2.h" "ss/vdp2.h Peek 선언 삽입 실패"
perl -0777 -pi -e 's# ASPACE_VDP2VRAM\n\};# ASPACE_VDP2VRAM,\n ASPACE_CRAM\n};# unless m{ASPACE_CRAM}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's#(case ASPACE_VDP2VRAM:\s+Address &= 0x7FFFF;\s+\*Buffer = VDP2::PeekVRAM\(Address\);\s+break;\n)#${1}\n   case ASPACE_CRAM:\n\tAddress &= 0xFFF;\n\t*Buffer = VDP2::PeekCRAM(Address) >> (((Address & 1) ^ 1) << 3);\n\tbreak;\n# unless m{PeekCRAM\(Address\)}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's#(case ASPACE_VDP2VRAM:\s+Address &= 0x7FFFF;\s+VDP2::PokeVRAM\(Address, \*Buffer\);\s+break;\n)#${1}\n   case ASPACE_CRAM:\n\t// Read-only: a CRAM poke must also update the renderer-side CRAM[] + ColorCache\n\t// (CRAM_Mode-dependent, via VDP2REND); out of scope for the read path, so no-op.\n\tbreak;\n# unless m{// Read-only: a CRAM poke}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's#( ASpace_Add\(GetAddressSpaceBytes<ASPACE_VDP2VRAM>, PutAddressSpaceBytes<ASPACE_VDP2VRAM>, "vdp2vram", "VDP2 VRAM", 19\);\n)#${1} ASpace_Add(GetAddressSpaceBytes<ASPACE_CRAM>, PutAddressSpaceBytes<ASPACE_CRAM>, "cram", "VDP2 CRAM (palette, raw store)", 12);\n# unless m{"cram", "VDP2 CRAM}' \
  "$SRC/src/ss/debug.inc"
inject_check 'ASPACE_VDP2VRAM,' "$SRC/src/ss/debug.inc" "ss/debug.inc ASPACE_CRAM enum 삽입 실패"
inject_check '*Buffer = VDP2::PeekCRAM(Address)' "$SRC/src/ss/debug.inc" "ss/debug.inc CRAM get case 삽입 실패"
inject_check '// Read-only: a CRAM poke must also update' "$SRC/src/ss/debug.inc" "ss/debug.inc CRAM put no-op 삽입 실패"
inject_check 'ASpace_Add(GetAddressSpaceBytes<ASPACE_CRAM>, PutAddressSpaceBytes<ASPACE_CRAM>, "cram"' "$SRC/src/ss/debug.inc" "ss/debug.inc cram AddressSpace 등록 실패"

# 4o. Saturn 값-조건 write BP — 쓰는 값 복제 주입(decoder-replicate).
#     SS는 write 훅이 Step(실제-write) 이전이라 MD식 "실제-write 값 주입"이 불가하다(값이 한 명령 늦음).
#     대신 CheckRWBreakpoints(sh7095.inc:5581, 실행 *전*, 전체 CPU 상태를 const로 보유 — freeze 판정과
#     동일 지점)에서 21개 write 옵코드의 쓰는 값을 sh7095_ops.inc 본체와 1:1로 복제해 MWrite로 흘린다.
#     값식 출처(실코드 확인): MOV.x Rm,@... = R[instr_nyb1](ops val=R[m], m=nyb1); MOV R0,@(disp,Rn)/
#     @(disp,GBR) = R[0]; STC.L = CtrlRegs[(instr>>4)&3], STS.L = SysRegs[(instr>>4)&3](ops cri/sri 동일);
#     RMW(ea = R[0]+GBR 또는 R[n]) — AND.B = peek&imm, OR.B = peek|imm, XOR.B = peek^imm, TAS.B = peek|0x80
#     (ops가 read 후 tmp 연산; peek=CheatMemRead(ss.cpp:381) side-effect-free, 같은 TU·실행 전이라
#     current_mem[ea]와 동일). 폭 마스킹(byte&0xFF/word&0xFFFF/long 그대로)은 emucap value_mask 기본
#     0xFFFFFFFF에서 상위바이트로 인한 조용한 불일치를 막으려 필수.
#     미세엣지: RMW의 peek는 27비트 FastMap만 본다 — work RAM은 정확하나 on-chip
#     레지스터(0xFFFFxxxx) 대상 RMW는 peek가 0/더미를 줄 수 있다. 값-조건 write BP의 실사용 대상은
#     work RAM 플래그/락이라 실질 영향 없음(좁은 코드근거 한계). emucap.cpp gate 완화는 후속 과제.
#
# (1) MWrite 함수포인터/파라미터 타입에 value 추가 — 선언(sh7095.h:606) + 정의(sh7095.inc:5581).
perl -0777 -pi -e 's{(void \(\*MWrite\)\(unsigned len, uint32 addr)\)}{${1}, uint32 value)} unless m{void \(\*MWrite\)\(unsigned len, uint32 addr, uint32 value\)}' \
  "$SRC/src/ss/sh7095.h"
perl -0777 -pi -e 's{(void \(\*MWrite\)\(unsigned len, uint32 addr)\)}{${1}, uint32 value)} unless m{void \(\*MWrite\)\(unsigned len, uint32 addr, uint32 value\)}' \
  "$SRC/src/ss/sh7095.inc"
inject_check 'void (*MWrite)(unsigned len, uint32 addr, uint32 value)' "$SRC/src/ss/sh7095.h" "ss/sh7095.h MWrite value 파라미터 삽입 실패"
inject_check 'void (*MWrite)(unsigned len, uint32 addr, uint32 value)' "$SRC/src/ss/sh7095.inc" "ss/sh7095.inc MWrite value 파라미터 삽입 실패"

# (2) 21개 write 옵코드 MWrite(len, ea) 호출에 옵코드별 value식 주입. 앵커는 BEGIN_BP_OP(NAME)로
#     유니크하고, tempered (?:(?!END_BP_OP).)*? 로 옵코드 블록 경계를 넘지 않는다(re-run 시 가드로 스킵).
perl -0777 -pi -e '
my %V = (
  MOV_B_REG_REGINDIR      => [1, "R[instr_nyb1] & 0xFF"],
  MOV_W_REG_REGINDIR      => [2, "R[instr_nyb1] & 0xFFFF"],
  MOV_L_REG_REGINDIR      => [4, "R[instr_nyb1]"],
  MOV_B_REG_REGINDIRPD    => [1, "R[instr_nyb1] & 0xFF"],
  MOV_W_REG_REGINDIRPD    => [2, "R[instr_nyb1] & 0xFFFF"],
  MOV_L_REG_REGINDIRPD    => [4, "R[instr_nyb1]"],
  MOV_B_REG0_REGINDIRDISP => [1, "R[0] & 0xFF"],
  MOV_W_REG0_REGINDIRDISP => [2, "R[0] & 0xFFFF"],
  MOV_L_REG_REGINDIRDISP  => [4, "R[instr_nyb1]"],
  MOV_B_REG_IDXREGINDIR   => [1, "R[instr_nyb1] & 0xFF"],
  MOV_W_REG_IDXREGINDIR   => [2, "R[instr_nyb1] & 0xFFFF"],
  MOV_L_REG_IDXREGINDIR   => [4, "R[instr_nyb1]"],
  MOV_B_REG0_GBRINDIRDISP => [1, "R[0] & 0xFF"],
  MOV_W_REG0_GBRINDIRDISP => [2, "R[0] & 0xFFFF"],
  MOV_L_REG0_GBRINDIRDISP => [4, "R[0]"],
  AND_B_IMM_IDXGBRINDIR   => [1, "(CheatMemRead(ea) & (uint8)instr) & 0xFF"],
  OR_B_IMM_IDXGBRINDIR    => [1, "(CheatMemRead(ea) | (uint8)instr) & 0xFF"],
  XOR_B_IMM_IDXGBRINDIR   => [1, "(CheatMemRead(ea) ^ (uint8)instr) & 0xFF"],
  TAS_B_REGINDIR          => [1, "(CheatMemRead(ea) | 0x80) & 0xFF"],
  STC_L                   => [4, "CtrlRegs[(instr >> 4) & 0x3]"],
  STS_L                   => [4, "SysRegs[(instr >> 4) & 0x3]"],
);
for my $op (sort keys %V) {
  my ($len, $val) = @{$V{$op}};
  next if /BEGIN_BP_OP\(\Q$op\E\)(?:(?!END_BP_OP).)*?MWrite\($len, ea, /s;
  s/(BEGIN_BP_OP\(\Q$op\E\)(?:(?!END_BP_OP).)*?MWrite\($len, ea)\)/$1, $val)/s
    or die "ERROR: ss/sh7095.inc value inject failed for $op\n";
}
' "$SRC/src/ss/sh7095.inc"
[ "$(grep -cE 'MWrite\([0-9], ea, ' "$SRC/src/ss/sh7095.inc")" -eq 21 ] || { echo "ERROR: ss/sh7095.inc 21 write-value 주입 실패 (got $(grep -cE 'MWrite\([0-9], ea, ' "$SRC/src/ss/sh7095.inc"))"; exit 1; }

# (3) ss/debug.inc: emucap_bp_record_value extern(:60) + DBG_CheckWriteBP 시그니처 +value(:83) +
#     write 기록을 _value로 치환(:92). 4c가 base record/extern을 먼저 주입하므로 이 섹션은 그 뒤다.
perl -0777 -pi -e 's{(extern "C" void emucap_bp_record\(unsigned len, unsigned addr, int is_write\);\n)}{${1}extern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value);\n} unless m{emucap_bp_record_value}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's{(static MDFN_COLD void DBG_CheckWriteBP\(unsigned len, uint32 addr)\)}{${1}, uint32 value)} unless m{DBG_CheckWriteBP\(unsigned len, uint32 addr, uint32 value\)}' \
  "$SRC/src/ss/debug.inc"
perl -0777 -pi -e 's{emucap_bp_record\(len, addr, 1\);}{emucap_bp_record_value(len, addr, 1, value);} unless m{emucap_bp_record_value\(len, addr, 1, value\)}' \
  "$SRC/src/ss/debug.inc"
inject_check 'extern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value)' "$SRC/src/ss/debug.inc" "ss/debug.inc record_value extern 삽입 실패"
inject_check 'DBG_CheckWriteBP(unsigned len, uint32 addr, uint32 value)' "$SRC/src/ss/debug.inc" "ss/debug.inc DBG_CheckWriteBP 시그니처 삽입 실패"
inject_check 'emucap_bp_record_value(len, addr, 1, value)' "$SRC/src/ss/debug.inc" "ss/debug.inc write 값기록 _value 치환 실패"

# 4h. MD 68000 디스어셈블러(desa68) 백워드 분기 타깃 오류 수정: relPC()가 16비트 PC-상대 변위 d.w를
#     부호확장하지 않고 unsigned로 가산해 백워드 Bcc.W/BSR/JSR(d16,PC) 타깃이 +0x10000 어긋났다
#     (예: BCS 0x5922E가 실제 0x4922E). 8비트(.S)는 (s32)(s8)로 이미 부호확장하나 16비트만 누락 →
#     (s32)(s16)로 부호확장한다. 68000 전용(desa68)이라 MD만 영향. static disasm 콜체인역추적의 정확도 직결.
perl -0777 -pi -e 's{return \(d\.pc \+ d\.w - 2\) & d\.memmsk;}{return (d.pc + (s32)(s16)d.w - 2) & d.memmsk;} unless m{\(s16\)d\.w}' \
  "$SRC/src/desa68/desa68.c"
inject_check '(s32)(s16)d.w - 2) & d.memmsk' "$SRC/src/desa68/desa68.c" "desa68.c relPC 16비트 disp 부호확장 패치 실패"

# 4p. 값-조건 BP 기록 — WonderSwan/WSC(wswan/debug.cpp): V30MZ debug 훅의 read/write BP 매칭 지점.
#     read/write 훅은 20비트 물리 주소 A를 받고(v30mz PutMemB/GetMemB의 (seg<<4)+off), WriteHandler는
#     쓰는 값 V가 스코프에 있으므로 값을 직접 주입한다(PCE WriteHandler와 동형). read는 fallback
#     (emucap_read_value_for_bp가 physical에서 리틀엔디언으로 읽음)이 정확하므로 값 없이 기록한다.
perl -0777 -pi -e 's/(static bool FoundBPoint = 0;\n)/${1}extern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write);\nextern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value);\n/ unless m{emucap_bp_record}' \
  "$SRC/src/wswan/debug.cpp"
perl -0777 -pi -e 's{(static uint8 ReadHandler\(uint32 A\).*?if\(testA >= bpit->A\[0\] && testA <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = 1;)}{${1}   emucap_bp_record(1, testA, 0);\n${2}}s unless m{emucap_bp_record\(1, testA}' \
  "$SRC/src/wswan/debug.cpp"
perl -0777 -pi -e 's{(static void WriteHandler\(uint32 A, uint8 V\).*?if\(testA >= bpit->A\[0\] && testA <= bpit->A\[1\]\)\n\s*\{\n)(\s*FoundBPoint = 1;)}{${1}   emucap_bp_record_value(1, testA, 1, V);\n${2}}s unless m{emucap_bp_record_value\(1, testA}' \
  "$SRC/src/wswan/debug.cpp"
inject_check 'emucap_bp_record(1, testA, 0)' "$SRC/src/wswan/debug.cpp" "wswan/debug.cpp read BP 기록 삽입 실패"
inject_check 'emucap_bp_record_value(1, testA, 1, V)' "$SRC/src/wswan/debug.cpp" "wswan/debug.cpp write BP 값주입 삽입 실패"

# 4q. 입력 진단 — WonderSwan(wswan/main.cpp): 코어가 매 프레임 PortDeviceData(=주입된 PortData[0])를
#     WSButtonStatus로 읽는 지점. 게임에 도달한 버튼 비트를 status.last_game_input에 노출한다(패드 래치 등가).
#     extern "C" 선언은 파일 스코프(namespace 진입 전)에 둔다 — 함수 본문 안엔 linkage-spec을 못 쓴다.
perl -0777 -pi -e 's/(#include "debug\.h"\n)/${1}\nextern "C" void emucap_game_data_store(unsigned short);\n/ unless m{emucap_game_data_store}' \
  "$SRC/src/wswan/main.cpp"
perl -0777 -pi -e 's{(WSButtonStatus = MDFN_de16lsb\(PortDeviceData\);\n)}{${1} ::emucap_game_data_store((unsigned short)WSButtonStatus);\n} unless m{::emucap_game_data_store}' \
  "$SRC/src/wswan/main.cpp"
inject_check '::emucap_game_data_store((unsigned short)WSButtonStatus)' "$SRC/src/wswan/main.cpp" "wswan/main.cpp 입력진단 삽입 실패"

# 5. configure — ss+psx+pce+md+wswan 활성(한 바이너리 멀티시스템). Saturn은 host_cpu 자동탐지 실패라
#    --enable-ss 명시 필수. psx/pce/md/wswan은 기본 on이나 명시해 의도를 고정한다.
echo "→ configure (--enable-ss --enable-psx --enable-pce --enable-pce-fast --enable-md --enable-wswan --enable-debugger)"
cd "$SRC"
if command -v brew >/dev/null 2>&1; then
  export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
fi
# Windows(MSYS2/MinGW): 소켓이 winsock이라 링크에 ws2_32가 필요하다(emucap.cpp의 socket/WSAStartup 등).
# autotools는 LIBS를 링크 커맨드에 붙이므로 configure 전에 주입한다.
case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW*|MSYS*|CYGWIN*) export LIBS="-lws2_32 ${LIBS:-}" ;;
esac
./configure --enable-ss --enable-psx --enable-pce --enable-pce-fast --enable-md --enable-wswan --enable-debugger >/dev/null

# 6. emucap.cpp를 빌드에 추가(automake 불필요 — 생성된 Makefile의 OBJECTS에 추가, 일반 .cpp.o 규칙이 컴파일)
perl -0777 -pi -e 's/(am_libmdfnsdl_a_OBJECTS = main\.\$\(OBJEXT\) )/${1}emucap.\$(OBJEXT) /' \
  src/drivers/Makefile

# 7. 빌드
echo "→ make"
make -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"

echo ""
echo "✓ 빌드 완료: $SRC/src/mednafen (ss + psx + pce + md + wswan)"
echo "  실행: adapters/mednafen/launch.sh <disc_or_rom> <status.listening_port> [name] [force_module]"
echo "  예:   MEDNAFEN_FORCE_MODULE=pce adapters/mednafen/launch.sh <pce.cue|rom.pce> 47800"
echo "  예:   MEDNAFEN_FORCE_MODULE=md adapters/mednafen/launch.sh <rom.md|rom.gen|rom.smd> 47800"
echo "  예:   MEDNAFEN_FORCE_MODULE=wswan adapters/mednafen/launch.sh <rom.ws|rom.wsc> 47800"
