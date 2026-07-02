#!/bin/bash
# Mesen을 이 포트 전용으로 띄운다. 다중 세션 안전 — 이 포트로 이전에 띄운 인스턴스만 PID로 정리하고,
# 다른 세션의 Mesen은 절대 건드리지 않는다(pkill -i mesen / killall Mesen 같은 광역 종료 금지).
# 포트는 emucap-mcp의 status가 알려주는 listening_port를 쓴다(자동 포트라 세션마다 다를 수 있음).
# 해당 포트에 MCP listener가 없으면 에뮬레이터를 띄우지 않고 실패한다(status 선행 강제).
# ⚠ 47800을 하드코딩하지 말 것 — 반드시 status의 listening_port를 넘긴다. 안 그러면 다른 세션의
#   포트에 끼어들어 그 세션 에뮬레이터를 정리해버릴 수 있다(아래 안전장치가 막지만 포트는 맞게 줄 것).
# 사용: launch.sh <ROM> <EMUCAP_PORT> [EMUCAP_NAME]
# 환경변수:
#   MESEN_BIN=/path/to/Mesen                  기본: OS별 일반 설치 경로 또는 PATH의 Mesen
#   EMUCAP_MESEN_LAUNCH_MODE=auto|open|direct 기본: auto(격리 copy를 direct 실행)
#   EMUCAP_EMU_HOME=/path/to/emucap/home      기본: OS별 사용자 데이터 아래 emucap/
#   EMUCAP_LAUNCH_WAIT=<seconds>              기본: 20
#   EMUCAP_POST_CONNECT_GRACE=<seconds>       기본: 2
#   EMUCAP_LOG=/path/to/custom.log            기본: <emucap-data>/mesen2/<port>/mesen.log
set -euo pipefail

usage() {
  echo "usage: $0 <ROM> <EMUCAP_PORT> [EMUCAP_NAME]" >&2
  echo "  EMUCAP_PORT는 emucap MCP status.listening_port를 launch 직전에 다시 조회한 값이어야 한다." >&2
}

if [ "$#" -lt 2 ]; then
  usage
  exit 2
fi

ROM="$1"; PORT="$2"; NAME="${3:-}"
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
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/runtime-env.sh"
LUA="$HERE/emucap-live.lua"
# 빌드 hash: 스크립트 어댑터(Lua)는 로드시가 곧 버전이라 launch 시점 emucap git hash를 넘긴다 — hello/
# status.emulator_build로 노출해 사용자가 git HEAD와 대조한다. emucap-live.lua가 HEAD와 다르면 -dirty.
EMUCAP_BUILD_HASH="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$HERE" diff --quiet HEAD -- emucap-live.lua 2>/dev/null || EMUCAP_BUILD_HASH="${EMUCAP_BUILD_HASH}-dirty"
export EMUCAP_BUILD_HASH

default_emu_home_base() {
  case "$(uname -s 2>/dev/null || echo unknown)" in
    Darwin)
      echo "${HOME:-/tmp}/Library/Application Support/emucap"
      ;;
    MINGW*|MSYS*|CYGWIN*)
      if [ -n "${LOCALAPPDATA:-}" ]; then
        echo "$LOCALAPPDATA/emucap"
      elif [ -n "${APPDATA:-}" ]; then
        echo "$APPDATA/emucap"
      elif [ -n "${USERPROFILE:-}" ]; then
        echo "$USERPROFILE/AppData/Local/emucap"
      elif [ -n "${HOME:-}" ]; then
        echo "$HOME/AppData/Local/emucap"
      else
        echo "/tmp/emucap"
      fi
      ;;
    *)
      if [ -n "${XDG_DATA_HOME:-}" ]; then
        echo "$XDG_DATA_HOME/emucap"
      elif [ -n "${HOME:-}" ]; then
        echo "$HOME/.local/share/emucap"
      else
        echo "/tmp/emucap"
      fi
      ;;
  esac
}

resolve_default_mesen() {
  local candidates=()
  case "$(uname -s 2>/dev/null || echo unknown)" in
    Darwin)
      candidates+=("/Applications/Mesen.app/Contents/MacOS/Mesen")
      ;;
    MINGW*|MSYS*|CYGWIN*)
      local program_files_x86
      program_files_x86="$(printenv 'ProgramFiles(x86)' 2>/dev/null || true)"
      [ -n "${LOCALAPPDATA:-}" ] && candidates+=("$LOCALAPPDATA/Programs/Mesen/Mesen.exe" "$LOCALAPPDATA/Mesen/Mesen.exe")
      [ -n "${ProgramFiles:-}" ] && candidates+=("$ProgramFiles/Mesen/Mesen.exe")
      [ -n "$program_files_x86" ] && candidates+=("$program_files_x86/Mesen/Mesen.exe")
      [ -n "${USERPROFILE:-}" ] && candidates+=("$USERPROFILE/Mesen/Mesen.exe")
      ;;
  esac
  local candidate
  for candidate in "${candidates[@]}"; do
    [ -x "$candidate" ] && { printf '%s\n' "$candidate"; return 0; }
  done
  for candidate in Mesen Mesen.exe mesen; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

EMUCAP_MESEN_BASE="${EMUCAP_EMU_HOME:-$(default_emu_home_base)}"
RUN_DIR="$EMUCAP_MESEN_BASE/mesen2/$PORT"
PIDFILE="$RUN_DIR/mesen.pid"
LOG="${EMUCAP_LOG:-$RUN_DIR/mesen.log}"
WAIT="${EMUCAP_LAUNCH_WAIT:-20}"
POST_CONNECT_GRACE="${EMUCAP_POST_CONNECT_GRACE:-2}"
MESEN_BIN="${MESEN_BIN:-$(resolve_default_mesen || true)}"
case "$MESEN_BIN" in
  *.app) MESEN_BIN="$MESEN_BIN/Contents/MacOS/Mesen" ;;
esac
OLD="$(cat "$PIDFILE" 2>/dev/null || true)"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi
[ -n "$SESSION_TOKEN" ] && export EMUCAP_SESSION_TOKEN="$SESSION_TOKEN"

[ -f "$ROM" ] || { echo "ERROR: ROM 없음: $ROM" >&2; exit 1; }
[ -f "$LUA" ] || { echo "ERROR: Lua 어댑터 없음: $LUA" >&2; exit 1; }
[ -n "$MESEN_BIN" ] && [ -x "$MESEN_BIN" ] || { echo "ERROR: Mesen 바이너리 없음 — MESEN_BIN을 설정하거나 일반 설치 경로/PATH에 Mesen을 준비하라" >&2; exit 1; }

find_app_bundle() {
  local p="$1"
  while [ -n "$p" ] && [ "$p" != "/" ] && [ "$p" != "." ]; do
    case "$p" in
      *.app) echo "$p"; return 0 ;;
    esac
    p="$(dirname "$p")"
  done
  return 1
}

write_portable_settings() {
  local settings="$1"
  mkdir -p "$(dirname "$settings")"
  cat > "$settings" <<'JSON'
{
  "Debug": {
    "ScriptWindow": {
      "AllowIoOsAccess": true,
      "AllowNetworkAccess": true,
      "ScriptTimeout": 60
    }
  },
  "Preferences": {
    "SingleInstance": false
  }
}
JSON
}

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

copy_file_replace() {
  local src="$1"
  local dst="$2"
  local tmp
  tmp="$(unique_runtime_path "$dst" "tmp")"
  mkdir -p "$(dirname "$dst")"
  if [ -d "$dst" ]; then
    echo "ERROR: portable 파일 대상이 디렉터리임: $dst" >&2
    return 1
  fi
  cp -p "$src" "$tmp" || {
    rm -f "$tmp"
    return 1
  }
  mv -f "$tmp" "$dst" || {
    rm -f "$tmp"
    return 1
  }
}

copy_app_bundle_replace() {
  local src_app="$1"
  local dst_app="$2"
  local tmp_app backup_app
  local had_dst=0
  case "$dst_app" in
    "$RUN_DIR"/*) ;;
    *) echo "ERROR: unsafe portable app path: $dst_app" >&2; return 1 ;;
  esac
  tmp_app="$(unique_runtime_path "$dst_app" "tmp")"
  backup_app="$(unique_runtime_path "$dst_app" "old")"
  mkdir -p "$(dirname "$dst_app")"
  if command -v ditto >/dev/null 2>&1; then
    ditto "$src_app" "$tmp_app" || {
      rm -rf -- "$tmp_app"
      return 1
    }
  else
    cp -R "$src_app" "$tmp_app" || {
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

prepare_portable_mesen() {
  local source_bin="$1"
  local emu_home="$RUN_DIR"
  mkdir -p "$emu_home"

  local source_app
  source_app="$(find_app_bundle "$source_bin" 2>/dev/null || true)"
  if [ -n "$source_app" ]; then
    local app_name portable_app rel
    app_name="$(basename "$source_app")"
    portable_app="$emu_home/$app_name"
    rel="${source_bin#"$source_app"/}"
    copy_app_bundle_replace "$source_app" "$portable_app" || {
      echo "ERROR: portable Mesen.app 복사 실패: $source_app → $portable_app" >&2
      exit 1
    }
    MESEN_BIN="$portable_app/$rel"
    MESEN_APP_BUNDLE="$portable_app"
  else
    local portable_dir
    portable_dir="$emu_home/portable"
    mkdir -p "$portable_dir"
    MESEN_BIN="$portable_dir/$(basename "$source_bin")"
    copy_file_replace "$source_bin" "$MESEN_BIN" || {
      echo "ERROR: portable Mesen 바이너리 복사 실패: $source_bin → $MESEN_BIN" >&2
      exit 1
    }
    chmod +x "$MESEN_BIN" 2>/dev/null || true
    MESEN_APP_BUNDLE="$(find_app_bundle "$MESEN_BIN" 2>/dev/null || true)"
  fi

  MESEN_SETTINGS="$(dirname "$MESEN_BIN")/settings.json"
  write_portable_settings "$MESEN_SETTINGS"
  [ -x "$MESEN_BIN" ] || { echo "ERROR: portable Mesen 바이너리 실행 불가: $MESEN_BIN" >&2; exit 1; }
  export EMUCAP_MESEN_HOME="$emu_home"
}

tail_log() {
  echo "---- Mesen log: $LOG ----" >&2
  if [ -s "$LOG" ]; then
    tail -n 140 "$LOG" >&2
  else
    echo "(log empty or missing)" >&2
  fi
}

connected_pid() {
  local pid="$1"
  command -v lsof >/dev/null 2>&1 || return 2
  lsof -nP -a -p "$pid" -iTCP:"$PORT" -sTCP:ESTABLISHED >/dev/null 2>&1
}

connected_port_pid() {
  command -v lsof >/dev/null 2>&1 || return 2
  lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null \
    | awk 'NR > 1 && $1 ~ /Mesen/ { print $2; exit }'
}

kill_ours() {
  local pid="$1"
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null || true
  sleep 1
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
}

# 안전장치 0 (status 선행 강제): 이 포트에 MCP listener가 없으면 status를 안 불렀거나 stale 포트를
#   넘긴 것이다. 이 상태에서 GUI를 띄우면 연결 대기 후 launcher가 죽여 "비디오 초기화 직후 종료"처럼 보이므로
#   에뮬레이터를 아예 띄우지 않는다.
# 안전장치 1 (cross-session 보호 — 연결된 건 절대 안 죽임): 이 포트에 에뮬레이터가 이미 연결
#   (ESTABLISHED)돼 있으면 = 누군가(이 세션이든 타 세션이든) 쓰는 중이다. 아무것도 죽이지 않고 거부한다.
#   에이전트가 stale 포트(예전 status값)로 띄워 다른 세션의 인스턴스를 죽이는 사고를 여기서 원천 차단.
#   같은 세션이면 그 인스턴스를 그대로 쓰고, 정말 새로 띄우려면 그것을 먼저 닫거나 status를 "다시" 조회해
#   현재 listening_port로 띄운다(포트는 세션 중에도 바뀔 수 있으니 캐시하지 말 것).
if command -v lsof >/dev/null 2>&1; then
  LISTENER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null \
            | awk 'NR > 1 { print $1 ":" $2 }' \
            | sort -u \
            | tr '\n' ' ' || true)"
  if [ -z "$LISTENER" ]; then
    echo "ERROR: 포트 $PORT 에 MCP listener가 없다 — Mesen을 띄우지 않는다." >&2
    echo "  먼저 emucap MCP status를 호출해 이 세션의 listening_port를 받은 뒤, 그 값을 launch.sh에 넘겨라." >&2
    echo "  47800 하드코딩이나 이전 status 포트 캐시는 금지." >&2
    exit 3
  fi

  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null \
            | awk 'NR > 1 && $1 ~ /(Mesen|mednafen|Flycast|pcsx-redux)/ { print $2 }' \
            | sort -u \
            | tr '\n' ' ' || true)"
  if [ -n "$INUSE" ]; then
    echo "ERROR: 포트 $PORT 에 이미 에뮬레이터(PID: $INUSE)가 연결돼 있다 — 아무것도 안 죽인다." >&2
    echo "  같은 세션이면 그 인스턴스를 그대로 쓰라. 새로 띄우려면 그것을 먼저 닫거나, status를 '다시' 조회해" >&2
    echo "  현재 listening_port로 띄우라(예전 포트 캐시 금지)." >&2
    exit 3
  fi
fi

# 안전장치 2 (고아만 정리): 위에서 '연결된 에뮬레이터 없음'을 확인했으니, 우리 PIDFILE의 OLD가 살아 있고
#   Mesen.app이면 = emucap-mcp는 죽었는데 떠 있는 고아 창(우리 이전 실행의 잔해)일 뿐이다. 그것만 정리한다.
#   연결돼 일하는 인스턴스는 위 안전장치 1이 이미 거부했으므로 여기서 타 세션을 죽일 일은 없다.
if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null && ps -p "$OLD" -o command= 2>/dev/null | grep -q 'Mesen.app'; then
  kill_ours "$OLD"
fi

SOURCE_MESEN_BIN="$MESEN_BIN"
MESEN_APP_BUNDLE=""
MESEN_SETTINGS=""
prepare_portable_mesen "$SOURCE_MESEN_BIN"

# macOS 디스플레이 슬립 대응(macOS 전용 — caffeinate 있을 때만, 타 플랫폼은 no-op):
#   Mesen(Avalonia GUI)은 실제 디스플레이가 깨어 있어야 렌더러가 뜬다. 사용자가 자리를 비워
#   디스플레이가 슬립이면(화면 잠금 해제 != display awake) "Avalonia.Native ... RenderTimer (-6661)"로
#   즉사한다. 띄우기 전 깨우고(caffeinate -u), 사는 동안 슬립을 막는다(caffeinate -d -w: Mesen 죽으면
#   자동 해제돼 좀비 없음). caffeinate가 없는 플랫폼은 이 가드를 건너뛴다(그 OS는 다른 메커니즘).
if command -v caffeinate >/dev/null 2>&1; then
  caffeinate -u -t 5 >/dev/null 2>&1 & disown   # 슬립이었으면 디스플레이 깨우기
  sleep 1                                         # 렌더러 init 전에 깨어날 시간
fi

mkdir -p "$(dirname "$LOG")" "$RUN_DIR"
: > "$LOG"
{
  echo "emucap Mesen launch"
  echo "  rom=$ROM"
  echo "  port=$PORT"
  echo "  name=${NAME:-<none>}"
  echo "  session_token=${SESSION_TOKEN:+present}"
  echo "  token_file=$TOKEN_FILE"
  echo "  lua=$LUA"
  echo "  wait=${WAIT}s"
  echo "  post_connect_grace=${POST_CONNECT_GRACE}s"
  echo "  source_mesen_bin=$SOURCE_MESEN_BIN"
  echo "  portable_mesen_bin=$MESEN_BIN"
  echo "  portable_settings=$MESEN_SETTINGS"
  echo "  emucap_mesen_home=${EMUCAP_MESEN_HOME:-<none>}"
  echo "  launch_mode=${EMUCAP_MESEN_LAUNCH_MODE:-auto}"
} >>"$LOG"

LAUNCH_MODE="${EMUCAP_MESEN_LAUNCH_MODE:-auto}"
if [ "$LAUNCH_MODE" = "auto" ]; then
  LAUNCH_MODE="direct"
fi

NEWPID=""
CAFFEINATED=0
if [ "$LAUNCH_MODE" = "open" ]; then
  if [ -z "$MESEN_APP_BUNDLE" ] || [ ! -d "$MESEN_APP_BUNDLE" ]; then
    echo "ERROR: open launch mode requires a portable .app bundle; use direct mode for this binary." >&2
    exit 2
  fi
  # Codex처럼 transient PTY/agent에서 GUI .app을 직접 exec하면 macOS reopen/open 경로와 충돌해
  # 신규 창이 안 뜨거나 연결 직후 사라지는 사례가 있다. LaunchServices로 새 인스턴스를 요청하되,
  # EMUCAP_* 환경변수는 open --env로 명시 전달한다. 실제 PID는 포트 연결 후 lsof로 역추적한다.
  OPEN_ENV=(--env "EMUCAP_PORT=$PORT" --env "EMUCAP_BUILD_HASH=$EMUCAP_BUILD_HASH")
  [ -n "$NAME" ] && OPEN_ENV+=(--env "EMUCAP_NAME=$NAME")
  [ -n "$SESSION_TOKEN" ] && OPEN_ENV+=(--env "EMUCAP_SESSION_TOKEN=$SESSION_TOKEN")
  OPEN_ENV+=(--env "EMUCAP_CONTENT=$ROM")
  [ -n "${EMUCAP_PREARM:-}" ] && OPEN_ENV+=(--env "EMUCAP_PREARM=$EMUCAP_PREARM")
  [ -n "${EMUCAP_FREEZE_KEY:-}" ] && OPEN_ENV+=(--env "EMUCAP_FREEZE_KEY=$EMUCAP_FREEZE_KEY")
  open -n -g "$MESEN_APP_BUNDLE" --stdout "$LOG" --stderr "$LOG" "${OPEN_ENV[@]}" --args "$ROM" "$LUA"
else
  # Codex 같은 transient PTY/agent에서 direct exec를 쓰면 부모 shell 종료나 PTY 정리가
  # GUI 프로세스에 영향을 줄 수 있다. Mednafen launcher와 같은 방식으로 가능한 경우
  # 새 세션(start_new_session)에 띄우고 stdio를 /dev/null + log로 분리한다.
  if command -v python3 >/dev/null 2>&1; then
    NEWPID="$(
      python3 - "$LOG" "$PIDFILE" "$MESEN_BIN" "$PORT" "$NAME" "$ROM" "$LUA" <<'PY'
import os
import subprocess
import sys

log_path, pidfile, exe, port, name, rom, lua = sys.argv[1:]
env = os.environ.copy()
env["EMUCAP_PORT"] = port
env["EMUCAP_CONTENT"] = rom
if name:
    env["EMUCAP_NAME"] = name
token = os.environ.get("EMUCAP_SESSION_TOKEN")
if token:
    env["EMUCAP_SESSION_TOKEN"] = token

devnull = open(os.devnull, "rb")
log = open(log_path, "ab", buffering=0)
try:
    proc = subprocess.Popen(
        [exe, rom, lua],
        stdin=devnull,
        stdout=log,
        stderr=subprocess.STDOUT,
        close_fds=True,
        start_new_session=True,
        env=env,
    )
finally:
    log.close()
    devnull.close()

with open(pidfile, "w", encoding="ascii") as f:
    f.write(f"{proc.pid}\n")
print(proc.pid)
PY
    )"
  else
    {
      trap '' HUP
      env EMUCAP_PORT="$PORT" EMUCAP_CONTENT="$ROM" ${NAME:+EMUCAP_NAME="$NAME"} ${SESSION_TOKEN:+EMUCAP_SESSION_TOKEN="$SESSION_TOKEN"} ${EMUCAP_PREARM:+EMUCAP_PREARM="$EMUCAP_PREARM"} \
        ${EMUCAP_FREEZE_KEY:+EMUCAP_FREEZE_KEY="$EMUCAP_FREEZE_KEY"} \
        nohup "$MESEN_BIN" "$ROM" "$LUA" >>"$LOG" 2>&1 &
      echo "$!" > "$PIDFILE"
    }
    NEWPID="$(cat "$PIDFILE")"
    disown "$NEWPID" 2>/dev/null || true
  fi
fi

if ! command -v lsof >/dev/null 2>&1; then
  echo "Mesen 기동: pid=$NEWPID port=$PORT name=${NAME:-<none>} log=$LOG (lsof 없음 — 연결 확인은 MCP status로 수행)" >&2
  exit 0
fi

for ((i = 0; i < WAIT; i++)); do
  if [ "$LAUNCH_MODE" = "open" ]; then
    NEWPID="$(connected_port_pid || true)"
    if [ -n "$NEWPID" ]; then
      echo "$NEWPID" > "$PIDFILE"
    fi
  elif ! kill -0 "$NEWPID" 2>/dev/null; then
    echo "ERROR: Mesen이 MCP 연결 전에 종료됨(pid=$NEWPID)." >&2
    tail_log
    exit 4
  fi

  if [ -n "$NEWPID" ] && connected_pid "$NEWPID"; then
    if [ "$CAFFEINATED" = "0" ] && command -v caffeinate >/dev/null 2>&1; then
      caffeinate -d -w "$NEWPID" >/dev/null 2>&1 & disown
      CAFFEINATED=1
    fi
    if [ "$POST_CONNECT_GRACE" != "0" ]; then
      sleep "$POST_CONNECT_GRACE"
      if ! kill -0 "$NEWPID" 2>/dev/null; then
        echo "ERROR: Mesen이 MCP 연결 직후 종료됨(pid=$NEWPID)." >&2
        echo "  macOS reopen/open dialog나 saved-state 복원이 신규 Mesen 창 생성을 막았을 수 있다. log와 화면 상태를 확인하라." >&2
        {
          echo
          echo "emucap post-connect failure: pid=$NEWPID 가 EMUCAP_PORT=$PORT 연결 직후 종료됐다."
          echo "emucap post-connect failure: launch.sh가 성공으로 반환하지 않았으므로, 이 상태를 video crash로 단정하지 말고 로그 tail과 macOS reopen/open 상태, status/listening_port를 확인하라."
        } >>"$LOG"
        tail_log
        exit 4
      fi
      if ! connected_pid "$NEWPID"; then
        echo "ERROR: Mesen이 MCP 연결 직후 연결을 잃음(pid=$NEWPID)." >&2
        echo "  macOS reopen/open dialog나 saved-state 복원이 신규 Mesen 창 생성을 막았을 수 있다. log와 화면 상태를 확인하라." >&2
        {
          echo
          echo "emucap post-connect failure: pid=$NEWPID 가 EMUCAP_PORT=$PORT 연결 직후 ESTABLISHED 상태를 잃었다."
          echo "emucap post-connect failure: status/listening_port, MCP 세션 재시작, stale port, 또는 macOS reopen/open 상태를 먼저 확인하라."
        } >>"$LOG"
        tail_log
        kill_ours "$NEWPID"
        exit 4
      fi
    fi
    echo "Mesen 연결됨: pid=$NEWPID port=$PORT name=${NAME:-<none>} log=$LOG rom=$ROM"
    exit 0
  fi
  sleep 1
done

echo "ERROR: Mesen이 ${WAIT}s 안에 EMUCAP_PORT=$PORT 로 연결하지 못함(pid=$NEWPID)." >&2
{
  echo
  echo "emucap launch timeout: ${WAIT}s 동안 pid=${NEWPID:-<unknown>} 가 EMUCAP_PORT=$PORT 로 ESTABLISHED 연결을 만들지 못했다."
  echo "emucap launch timeout: 이 프로세스는 launcher가 SIGTERM으로 정리한다. 렌더러/비디오 초기화 직후 종료처럼 보이면 video crash보다 status/listening_port 누락 또는 stale port를 먼저 의심하라."
} >>"$LOG"
tail_log
kill_ours "$NEWPID"
exit 4
