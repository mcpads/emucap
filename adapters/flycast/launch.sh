#!/bin/bash
# Flycast(Dreamcast)를 emucap용으로 안전하게 띄운다. Mesen launch.sh와 동형 — 다중세션 안전(이 포트로
# 띄운 우리 인스턴스만 PID로 정리, 광역 kill 금지), 디스플레이 슬립 대응(caffeinate), 그리고 Flycast
# 특유의 macOS 함정을 처리한다:
#   - dynarec(JIT): 재빌드한 .app은 JIT 엔타이틀먼트 서명이 없어 dynarec가 Init에서 verify 크래시한다
#     (nvmem/blockmanager). emucap은 디버깅 어댑터라 인터프리터로 충분(GDB 스텁도 인터프리터 강제) →
#     emu.cfg에 Dynarec.Enabled=no 강제. (풀스피드가 필요하면 .app을 com.apple.security.cs.allow-jit로 재서명.)
#   - "지난번 비정상 종료… 창을 다시 열까요?" 대화창: 반복 크래시가 띄운다. ApplePersistenceIgnoreState로
#     억제한다.
# 사용: launch.sh <disc.gdi/chd/cdi> <EMUCAP_PORT>   (포트는 emucap-mcp status의 listening_port)
#   EMUCAP_MUTE=0 으로 소리를 켤 수 있다(기본 1=음소거). 음소거·Dynarec·GDB 설정은 emucap 전용
#   격리 config 사본에만 적용한다.
set -euo pipefail

usage() {
  echo "usage: $0 <disc.gdi|disc.chd|disc.cdi> <EMUCAP_PORT>" >&2
  echo "  EMUCAP_PORT must be the current emucap MCP status.listening_port." >&2
}

if [ "$#" -lt 2 ]; then
  usage
  exit 2
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/runtime-env.sh"
DISC="$1"; PORT="$2"
MUTE="${EMUCAP_MUTE:-1}"   # 기본 음소거(디버깅 중 소음 방지). EMUCAP_MUTE=0이면 소리 유지.
case "$PORT" in
  ''|*[!0-9]*)
    echo "ERROR: EMUCAP_PORT must be a decimal TCP port: $PORT" >&2
    exit 2
    ;;
esac
if [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65535 ]; then
  echo "ERROR: EMUCAP_PORT out of range: $PORT" >&2
  exit 2
fi

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

default_flycast_binary() {
  local build_home="${EMUCAP_FLYCAST_BUILD_HOME:-$EMUCAP_DATA_ROOT/flycast-build}"
  case "$(uname -s)" in
    Darwin) echo "$build_home/work/build/Flycast.app/Contents/MacOS/Flycast" ;;
    MINGW*|MSYS*|CYGWIN*) echo "$build_home/work/build/Flycast.exe" ;;
    *) echo "$build_home/work/build/flycast" ;;
  esac
}

EMUCAP_DATA_ROOT="$(emucap_data_root)"
RUN_DIR="$EMUCAP_DATA_ROOT/flycast/$PORT"
FLY="${FLYCAST_APP:-$(default_flycast_binary)}"
case "$FLY" in
  *.app) FLY="$FLY/Contents/MacOS/Flycast" ;;
esac
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi
[ -n "$DISC" ] && [ -f "$DISC" ] || { echo "ERROR: 디스크 없음: $DISC"; exit 1; }
[ -x "$FLY" ] || { echo "ERROR: Flycast 바이너리 없음: $FLY (adapters/flycast/build.sh로 빌드하거나 FLYCAST_APP 지정)"; exit 1; }
PIDFILE="$RUN_DIR/flycast.pid"

# 안전장치(cross-session): 이 포트에 에뮬레이터가 이미 연결(ESTABLISHED)돼 있으면 거부(아무것도 안 죽임).
if command -v lsof >/dev/null 2>&1; then
  LISTENER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 { print $1 ":" $2 }' | sort -u | tr '\n' ' ' || true)"
  [ -z "$LISTENER" ] && { echo "ERROR: 포트 ${PORT}에 MCP listener가 없음 — 먼저 emucap status로 listening_port를 받아라."; exit 3; }
  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null | grep -iE 'Flycast|Mesen|mednafen|pcsx-redux|mame' | awk '{print $2}' | sort -u || true)"
  [ -n "$INUSE" ] && { echo "ERROR: 포트 ${PORT}에 이미 에뮬레이터(PID $(echo "$INUSE" | tr '\n' ' '))가 연결됨 — 거부."; exit 3; }
fi
# 고아만 정리(우리 PIDFILE)
OLD="$(cat "$PIDFILE" 2>/dev/null || true)"
if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null && ps -p "$OLD" -o command= 2>/dev/null | grep -qi 'flycast'; then
  kill -9 "$OLD" 2>/dev/null || true
fi

# 사용자의 실제 Flycast 설정·바이너리를 건드리지 않는다: 런타임 복사본과 config 사본을 emucap 전용 디렉터리에 둔다.
mkdir -p "$RUN_DIR"
VOL=$([ "$MUTE" = "1" ] && echo 0 || echo 100)
GDB=$([ "${EMUCAP_GDB:-0}" = "1" ] && echo yes || echo no)
FLYCAST_HOME_ISO="${EMUCAP_FLYCAST_HOME:-$RUN_DIR}"

unique_runtime_path() {
  local base="$1" label="$2" candidate n=0
  while :; do
    candidate="${base}.${label}.$$.$n"
    if [ ! -e "$candidate" ] && [ ! -L "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    n=$((n + 1))
  done
}

copy_runtime_file_replace() {
  local src="$1" dst="$2" base="$3" tmp
  case "$dst" in
    "$base"/*) ;;
    *) echo "ERROR: unsafe portable file path: $dst" >&2; return 1 ;;
  esac
  if [ -d "$dst" ]; then
    echo "ERROR: portable file target is a directory: $dst" >&2
    return 1
  fi
  tmp="$(unique_runtime_path "$dst" "tmp")"
  mkdir -p "$(dirname "$dst")"
  cp -p "$src" "$tmp" || {
    rm -f -- "$tmp"
    return 1
  }
  chmod +x "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$dst" || {
    rm -f -- "$tmp"
    return 1
  }
}

copy_app_bundle_replace() {
  local src_app="$1" dst_app="$2" base="$3"
  local tmp_app backup_app
  local had_dst=0
  case "$dst_app" in
    "$base"/*) ;;
    *) echo "ERROR: unsafe portable app path: $dst_app" >&2; return 1 ;;
  esac
  tmp_app="$(unique_runtime_path "$dst_app" "tmp")"
  backup_app="$(unique_runtime_path "$dst_app" "old")"
  mkdir -p "$tmp_app"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a "$src_app"/ "$tmp_app"/ || {
      rm -rf -- "$tmp_app"
      return 1
    }
  else
    (cd "$src_app" && tar -cf - .) | (cd "$tmp_app" && tar -xf -) || {
      rm -rf -- "$tmp_app"
      return 1
    }
  fi
  if [ -e "$dst_app" ] || [ -L "$dst_app" ]; then
    if [ ! -d "$dst_app" ]; then
      echo "ERROR: portable app target is not a directory: $dst_app" >&2
      rm -rf -- "$tmp_app"
      return 1
    fi
    mv "$dst_app" "$backup_app" || {
      rm -rf -- "$tmp_app"
      return 1
    }
    had_dst=1
  fi
  if mv "$tmp_app" "$dst_app"; then
    if [ "$had_dst" -eq 1 ]; then
      rm -rf -- "$backup_app" || true
    fi
    return 0
  else
    local rc=$?
    if [ "$had_dst" -eq 1 ] && [ ! -e "$dst_app" ] && [ ! -L "$dst_app" ]; then
      mv "$backup_app" "$dst_app" 2>/dev/null || true
    fi
    rm -rf -- "$tmp_app"
    return "$rc"
  fi
}

prepare_runtime_flycast() {
  local src="$1" iso="$2" portable app_root rel dst_app dst
  portable="$iso/portable"
  mkdir -p "$portable"
  case "$src" in
    "$portable"/*)
      printf '%s\n' "$src"
      return
      ;;
    *.app/Contents/MacOS/*)
      app_root="${src%%.app/Contents/MacOS/*}.app"
      rel="${src#$app_root/}"
      dst_app="$portable/$(basename "$app_root")"
      copy_app_bundle_replace "$app_root" "$dst_app" "$portable"
      printf '%s\n' "$dst_app/$rel"
      ;;
    *)
      dst="$portable/$(basename "$src")"
      copy_runtime_file_replace "$src" "$dst" "$portable"
      printf '%s\n' "$dst"
      ;;
  esac
}
FLY_RUNTIME="$(prepare_runtime_flycast "$FLY" "$FLYCAST_HOME_ISO")"

if [ "$(uname -s)" = "Darwin" ]; then
  ISO_CFGDIR="$FLYCAST_HOME_ISO/.flycast"
  FLYCAST_ISO_SRCS=()
  if [ -n "${HOME:-}" ]; then
    FLYCAST_ISO_SRCS=("$HOME/.flycast/emu.cfg" "$HOME/Library/Application Support/flycast/emu.cfg" "$HOME/Library/Application Support/Flycast/emu.cfg")
  fi
  FLYCAST_ISO_ENV=(HOME="$FLYCAST_HOME_ISO")
else
  ISO_CFGDIR="$FLYCAST_HOME_ISO/config/flycast"
  FLYCAST_ISO_SRCS=()
  if [ -n "${XDG_CONFIG_HOME:-}" ]; then
    FLYCAST_ISO_SRCS=("$XDG_CONFIG_HOME/flycast/emu.cfg")
  elif [ -n "${HOME:-}" ]; then
    FLYCAST_ISO_SRCS=("$HOME/.config/flycast/emu.cfg")
  fi
  FLYCAST_ISO_ENV=(XDG_CONFIG_HOME="$FLYCAST_HOME_ISO/config" XDG_DATA_HOME="$FLYCAST_HOME_ISO/data")
fi
ISO_CFG="$ISO_CFGDIR/emu.cfg"
mkdir -p "$ISO_CFGDIR"
# 사용자 실제 config를 사본으로(있으면 — BIOS·컨트롤 보존). 없으면 최소 [config]로 시작.
ISO_SRC=""
for c in "${FLYCAST_ISO_SRCS[@]}"; do [ -f "$c" ] && { ISO_SRC="$c"; break; }; done
if [ -n "$ISO_SRC" ]; then cp "$ISO_SRC" "$ISO_CFG"; else printf '[config]\n' > "$ISO_CFG"; fi
grep -q '^\[config\]' "$ISO_CFG" || printf '[config]\n' >> "$ISO_CFG"
apply_iso_cfg() {  # $1=key $2=value — 격리 사본에만 적용. sed -i.bak는 BSD·GNU 둘 다 동작.
  if grep -q "^$1 = " "$ISO_CFG"; then sed -i.bak "s|^$1 = .*|$1 = $2|" "$ISO_CFG" && rm -f "$ISO_CFG.bak"
  else perl -0777 -pi -e "s/(\[config\]\n)/\${1}$1 = $2\n/" "$ISO_CFG"; fi
}
apply_iso_cfg "Dynarec.Enabled" "no"
apply_iso_cfg "aica.Volume" "$VOL"
apply_iso_cfg "Debug.GDBEnabled" "$GDB"
# macOS: 격리 HOME의 앱 도메인에만 재기동-차단 설정을 쓴다.
if [ "$(uname -s)" = "Darwin" ]; then
  HOME="$FLYCAST_HOME_ISO" defaults write com.flyinghead.Flycast ApplePersistenceIgnoreState -bool YES 2>/dev/null || true
  HOME="$FLYCAST_HOME_ISO" defaults write com.flyinghead.Flycast NSQuitAlwaysKeepsWindows -bool false 2>/dev/null || true
fi

# 디스플레이 슬립 대응(macOS 전용 — caffeinate 있을 때만)
if command -v caffeinate >/dev/null 2>&1; then
  caffeinate -u -t 5 >/dev/null 2>&1 & disown
  sleep 1
fi

# 위에서 정한 격리 env(macOS=HOME, Linux=XDG)로 Flycast가 시드된 격리 config/data를 쓰게 한다(사용자 실제 config 불가침).
# 격리 env는 배열이라 `env`로 적용한다 — 배열-확장된 VAR=val은 셸이 할당 접두사가 아니라 명령 이름으로 해석한다.
# 출력은 discoverable 로그로(무음 실패 방지 — 부팅 에러를 에이전트가 볼 수 있게).
LAUNCH_LOG="${EMUCAP_LOG:-$RUN_DIR/flycast.log}"
mkdir -p "$(dirname "$LAUNCH_LOG")"
env EMUCAP_PORT="$PORT" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$DISC" "${FLYCAST_ISO_ENV[@]}" nohup "$FLY_RUNTIME" "$DISC" >"$LAUNCH_LOG" 2>&1 &
NEWPID=$!; disown
command -v caffeinate >/dev/null 2>&1 && { caffeinate -d -w "$NEWPID" >/dev/null 2>&1 & disown; }
echo "$NEWPID" > "$PIDFILE"
echo "Flycast 기동: pid=$NEWPID port=$PORT disc=$DISC session_token=${SESSION_TOKEN:+present} (runtime=$FLY_RUNTIME; 인터프리터; 음소거=$MUTE; log=$LAUNCH_LOG; 정리는 이 포트의 우리 인스턴스만)"
