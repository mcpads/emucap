#!/usr/bin/env bash
# Mednafen 포크를 emucap용으로 안전하게 띄운다.
#
# Agent-safe launch contract:
#   1. emucap MCP `status`로 받은 listening_port를 반드시 두 번째 인자로 넘긴다.
#   2. 해당 포트에 MCP listener가 없으면 에뮬레이터를 띄우지 않고 실패한다(status 선행 강제).
#   3. 이 스크립트만 프로세스 detaching/log/PIDFILE/연결 확인을 처리한다.
#   4. 성공은 "프로세스 시작"이 아니라 "해당 PID가 EMUCAP_PORT에 ESTABLISHED 연결"이다.
#   5. 실패하면 로그 tail을 출력하고 이 스크립트가 띄운 새 프로세스만 정리한다.
#
# 사용:
#   launch.sh <disc.cue|rom.pce|rom.md|disc.chd|...> <EMUCAP_PORT> [EMUCAP_NAME] [force_module]
#
# 예:
#   adapters/mednafen/launch.sh game.cue 47800
#   adapters/mednafen/launch.sh pce_game.cue 47800 pce1 pce
#   adapters/mednafen/launch.sh md_game.md 47800 sonic md
#
# 환경변수:
#   MEDNAFEN_BIN=/path/to/mednafen          기본: 플랫폼별 work/mednafen/src/mednafen(.exe), 없으면 PATH
#   MEDNAFEN_FORCE_MODULE=pce|psx|ss|md     4번째 인자 없을 때 사용
#   MEDNAFEN_SOUND=0|1                      기본: 0
#   EMUCAP_HEADLESS=1|0                     기본: 1(SDL_VIDEODRIVER=dummy)
#   EMUCAP_LAUNCH_WAIT=<seconds>            기본: 20
#   EMUCAP_POST_CONNECT_GRACE=<seconds>     기본: 3(연결 직후 사망/끊김 검출)
#   EMUCAP_EMU_HOME=/path/to/emucap-data    기본: OS별 emucap 데이터 루트
#   EMUCAP_LOG=/path/to/custom.log          기본: <emucap-data>/mednafen/<port>/mednafen.log
set -euo pipefail

usage() {
  echo "usage: $0 <disc.cue|rom.pce|rom.md|disc.chd|...> <EMUCAP_PORT> [EMUCAP_NAME] [force_module]" >&2
  echo "  EMUCAP_PORT는 emucap MCP status.listening_port를 launch 직전에 다시 조회한 값이어야 한다." >&2
}

if [ "$#" -lt 2 ]; then
  usage
  exit 2
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/runtime-env.sh"
CONTENT="$1"
PORT="$2"
NAME="${3:-}"
MODULE="${4:-${MEDNAFEN_FORCE_MODULE:-}}"
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
    printf '%s\n' "$EMUCAP_EMU_HOME"
    return
  fi
  case "$(uname -s 2>/dev/null || echo unknown)" in
    Darwin)
      if [ -n "${HOME:-}" ]; then
        printf '%s\n' "$HOME/Library/Application Support/emucap"
      else
        printf '%s\n' "/tmp/emucap"
      fi
      ;;
    MINGW*|MSYS*|CYGWIN*)
      if [ -n "${LOCALAPPDATA:-}" ]; then
        printf '%s\n' "$LOCALAPPDATA/emucap"
      elif [ -n "${APPDATA:-}" ]; then
        printf '%s\n' "$APPDATA/emucap"
      elif [ -n "${USERPROFILE:-}" ]; then
        printf '%s\n' "$USERPROFILE/AppData/Local/emucap"
      elif [ -n "${HOME:-}" ]; then
        printf '%s\n' "$HOME/AppData/Local/emucap"
      else
        printf '%s\n' "/tmp/emucap"
      fi
      ;;
    *)
      if [ -n "${XDG_DATA_HOME:-}" ]; then
        printf '%s\n' "$XDG_DATA_HOME/emucap"
      elif [ -n "${HOME:-}" ]; then
        printf '%s\n' "$HOME/.local/share/emucap"
      else
        printf '%s\n' "/tmp/emucap"
      fi
      ;;
  esac
}

EMUCAP_DATA_ROOT="$(emucap_data_root)"
RUN_DIR="$EMUCAP_DATA_ROOT/mednafen/$PORT"
WAIT="${EMUCAP_LAUNCH_WAIT:-20}"
POST_CONNECT_GRACE="${EMUCAP_POST_CONNECT_GRACE:-3}"
LOG="${EMUCAP_LOG:-$RUN_DIR/mednafen.log}"
PIDFILE="$RUN_DIR/mednafen.pid"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi

default_mednafen_name() {
  case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW*|MSYS*|CYGWIN*) printf '%s\n' "mednafen.exe" ;;
    *) printf '%s\n' "mednafen" ;;
  esac
}

resolve_default_mednafen() {
  local name built
  name="$(default_mednafen_name)"
  built="$HERE/work/mednafen/src/$name"
  if [ -x "$built" ]; then
    printf '%s\n' "$built"
    return
  fi
  if command -v mednafen >/dev/null 2>&1; then
    command -v mednafen
    return
  fi
  printf '%s\n' "$built"
}

copy_runtime_binary() {
  local src="$1"
  local dst="$2"
  local tmp="${dst}.tmp.$$"
  mkdir -p "$(dirname "$dst")"
  if [ -d "$dst" ]; then
    echo "ERROR: 런타임 바이너리 대상이 디렉터리임: $dst" >&2
    return 1
  fi
  cp "$src" "$tmp" || {
    rm -f "$tmp"
    return 1
  }
  chmod +x "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$dst" || {
    rm -f "$tmp"
    return 1
  }
}

BIN="${MEDNAFEN_BIN:-$(resolve_default_mednafen)}"

[ -f "$CONTENT" ] || { echo "ERROR: content 없음: $CONTENT" >&2; exit 1; }
[ -x "$BIN" ] || { echo "ERROR: Mednafen 바이너리 없음: $BIN (adapters/mednafen/build.sh 실행 필요)" >&2; exit 1; }

tail_log() {
  echo "---- Mednafen log: $LOG ----" >&2
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

kill_ours() {
  local pid="$1"
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null || true
  sleep 1
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
}

# Cross-session guard: status로 MCP listener를 먼저 세웠는지 확인하고,
# 이미 이 포트에 에뮬레이터가 연결돼 있으면 아무것도 죽이지 않는다.
if command -v lsof >/dev/null 2>&1; then
  LISTENER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null \
    | awk 'NR > 1 { print $1 ":" $2 }' \
    | sort -u \
    | tr '\n' ' ' || true)"
  if [ -z "$LISTENER" ]; then
    echo "ERROR: 포트 $PORT 에 MCP listener가 없다 — 에뮬레이터를 띄우지 않는다." >&2
    echo "  먼저 emucap MCP status를 호출해 이 세션의 listening_port를 받은 뒤, 그 값을 launch.sh에 넘겨라." >&2
    echo "  47800 하드코딩이나 이전 status 포트 캐시는 금지." >&2
    exit 3
  fi

  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null \
    | awk 'NR > 1 && $1 ~ /(mednafen|Mesen|Flycast|pcsx-redux)/ { print $2 }' \
    | sort -u \
    | tr '\n' ' ' || true)"
  if [ -n "$INUSE" ]; then
    echo "ERROR: 포트 $PORT 에 이미 에뮬레이터(PID: $INUSE)가 연결돼 있다 — 아무것도 안 죽인다." >&2
    echo "  같은 세션이면 그 인스턴스를 그대로 쓰라. 새로 띄우려면 먼저 닫고 status.listening_port를 다시 조회하라." >&2
    exit 3
  fi
fi

# 런타임↔빌드트리 분리: 기본 빌드 바이너리는 포트별 emucap 데이터 디렉터리로 복사해 실행한다.
if [ -z "${MEDNAFEN_BIN:-}" ]; then
  RUN_BIN="$RUN_DIR/$(basename "$BIN")"
  mkdir -p "$RUN_DIR"
  copy_runtime_binary "$BIN" "$RUN_BIN" || { echo "ERROR: 바이너리 복사 실패: $BIN → $RUN_BIN" >&2; exit 1; }
  BIN="$RUN_BIN"
fi

# 이전에 이 launcher가 띄웠지만 현재 연결되지 않은 고아만 정리한다.
OLD="$(cat "$PIDFILE" 2>/dev/null || true)"
if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null && ps -p "$OLD" -o command= 2>/dev/null | grep -qi 'mednafen'; then
  kill_ours "$OLD"
fi

mkdir -p "$(dirname "$LOG")" "$RUN_DIR"
: > "$LOG"
{
  echo "emucap Mednafen launch"
  echo "  content=$CONTENT"
  echo "  port=$PORT"
  echo "  name=${NAME:-<none>}"
  echo "  session_token=${SESSION_TOKEN:+present}"
  echo "  token_file=$TOKEN_FILE"
  echo "  module=${MODULE:-<auto>}"
  echo "  bin=$BIN"
  echo "  wait=${WAIT}s"
  echo "  post_connect_grace=${POST_CONNECT_GRACE}s"
} >>"$LOG"

export EMUCAP_PORT="$PORT"
export EMUCAP_CONTENT="$CONTENT"
export MEDNAFEN_ALLOWMULTI="${MEDNAFEN_ALLOWMULTI:-1}"
if [ -n "$NAME" ]; then
  export EMUCAP_NAME="$NAME"
fi
if [ -n "$SESSION_TOKEN" ]; then
  export EMUCAP_SESSION_TOKEN="$SESSION_TOKEN"
fi
if [ "${EMUCAP_HEADLESS:-1}" = "1" ]; then
  export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-dummy}"
fi

ARGS=(-sound "${MEDNAFEN_SOUND:-0}")
if [ "$MODULE" = "md" ]; then
  # Force a 6-button pad so the emucap raw input mask has a stable 2-byte buffer.
  # Games that only use 3-button inputs still see the low bits normally.
  ARGS+=(-md.input.auto 0 -md.input.port1 gamepad6)
fi
if [ -n "$MODULE" ]; then
  ARGS+=(-force_module "$MODULE")
fi
ARGS+=("$CONTENT")

if command -v python3 >/dev/null 2>&1; then
  NEWPID="$(
    python3 - "$LOG" "$PIDFILE" "$BIN" "${ARGS[@]}" <<'PY'
import os
import subprocess
import sys

log_path, pidfile, exe, *args = sys.argv[1:]
devnull = open(os.devnull, "rb")
log = open(log_path, "ab", buffering=0)
try:
    proc = subprocess.Popen(
        [exe, *args],
        stdin=devnull,
        stdout=log,
        stderr=subprocess.STDOUT,
        close_fds=True,
        start_new_session=True,
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
    nohup "$BIN" "${ARGS[@]}" </dev/null >>"$LOG" 2>&1 &
    echo "$!" > "$PIDFILE"
  }
  NEWPID="$(cat "$PIDFILE")"
  disown "$NEWPID" 2>/dev/null || true
fi

if ! command -v lsof >/dev/null 2>&1; then
  echo "Mednafen 기동: pid=$NEWPID port=$PORT log=$LOG (lsof 없음 — 연결 확인은 MCP status로 수행)" >&2
  exit 0
fi

for ((i = 0; i < WAIT; i++)); do
  if ! kill -0 "$NEWPID" 2>/dev/null; then
    echo "ERROR: Mednafen이 MCP 연결 전에 종료됨(pid=$NEWPID)." >&2
    tail_log
    exit 4
  fi
  if connected_pid "$NEWPID"; then
    if [ "$POST_CONNECT_GRACE" != "0" ]; then
      sleep "$POST_CONNECT_GRACE"
      if ! kill -0 "$NEWPID" 2>/dev/null; then
        echo "ERROR: Mednafen이 MCP 연결 직후 종료됨(pid=$NEWPID)." >&2
        {
          echo
          echo "emucap post-connect failure: pid=$NEWPID 가 EMUCAP_PORT=$PORT 연결 직후 종료됐다."
          echo "emucap post-connect failure: launch.sh가 성공으로 반환하지 않았으므로, 이 상태를 video crash로 단정하지 말고 로그 tail과 CUE/BIOS/상위 MCP 세션 상태를 확인하라."
        } >>"$LOG"
        tail_log
        exit 4
      fi
      if ! connected_pid "$NEWPID"; then
        echo "ERROR: Mednafen이 MCP 연결 직후 연결을 잃음(pid=$NEWPID)." >&2
        {
          echo
          echo "emucap post-connect failure: pid=$NEWPID 가 EMUCAP_PORT=$PORT 연결 직후 ESTABLISHED 상태를 잃었다."
          echo "emucap post-connect failure: status/listening_port, MCP 세션 재시작, stale port, 또는 에뮬레이터 자체 종료를 먼저 확인하라."
        } >>"$LOG"
        tail_log
        kill_ours "$NEWPID"
        exit 4
      fi
    fi
    echo "Mednafen 연결됨: pid=$NEWPID port=$PORT log=$LOG content=$CONTENT module=${MODULE:-<auto>}"
    exit 0
  fi
  sleep 1
done

echo "ERROR: Mednafen이 ${WAIT}s 안에 EMUCAP_PORT=$PORT 로 연결하지 못함(pid=$NEWPID)." >&2
{
  echo
  echo "emucap launch timeout: ${WAIT}s 동안 pid=$NEWPID 가 EMUCAP_PORT=$PORT 로 ESTABLISHED 연결을 만들지 못했다."
  echo "emucap launch timeout: 이 프로세스는 launcher가 SIGTERM으로 정리한다. 로그가 'Initializing video...' 뒤에서 끝나면 video crash가 아니라 status/listening_port 누락 또는 stale port를 먼저 의심하라."
} >>"$LOG"
tail_log
kill_ours "$NEWPID"
exit 4
