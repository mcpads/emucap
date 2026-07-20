#!/usr/bin/env bash
# Launch MAME PC-98 with a repo-local Lua GDB stub and attach the emucap bridge.
#
# Usage:
#   launch.sh <system.d88|system.hdm|disk.hdi> <EMUCAP_PORT> [EMUCAP_NAME] [machine]
#
# Optional environment:
#   MAME_BIN=/path/to/mame                   default: adapters/mame-pc98/work/mame safe wrapper if built, else mame
#   MAME_ROMPATH=/path/to/mame/roms           default: existing ~/mame/roms or emucap-owned roms dir
#   MAME_HOME=/path/to/mame-writable-home     default: emucap-owned per-OS data dir
#   MAME_GDB_PORT=<port>                 default: EMUCAP_PORT + 1000
#   MAME_PLUGINPATH=<path[;path...]>      default: adapters/mame-pc98/plugins
#   MAME_CBUS0=<slot option>              default: empty for pc9801rs; set to a slot option to override
#   MAME_READCONFIG=1                     opt in to user mame.ini; default ignores it
#   MAME_FLOP2=/path/to/second.hdm
#   MAME_HEADLESS=1|0                    default: 1 (-noreadconfig -video none -sound none)
#   MAME_ALLOW_VISIBLE=1                 required with MAME_HEADLESS=0
#   EMUCAP_PC98_BRIDGE_BIN=/path/to/emucap-mame-pc98-bridge
#   EMUCAP_LOG=/path/to/custom.log       default: <emucap-data>/mame-pc98/<port>/mame-pc98.log
set -euo pipefail

usage() {
  echo "usage: $0 <system.d88|system.hdm|disk.hdi> <EMUCAP_PORT> [EMUCAP_NAME] [machine]" >&2
  echo "  EMUCAP_PORT must be the current emucap MCP status.listening_port." >&2
}

if [ "$#" -lt 2 ]; then
  usage
  exit 2
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
. "$HERE/../_common/runtime-env.sh"
MEDIA="$1"
PORT="$2"
NAME="${3:-}"
MACHINE="${4:-${MAME_MACHINE:-pc9801rs}}"
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
LOCAL_MAME_BIN="$HERE/work/mame"
LOCAL_MAME_RAW_BIN="$HERE/work/mame.raw"
LOCAL_MAME_INVALID=""

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

default_rompath() {
  local base candidate
  for base in "${HOME:-}" "${USERPROFILE:-}"; do
    [ -n "$base" ] || continue
    candidate="$base/mame/roms"
    if [ -d "$candidate" ]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
  printf '%s\n' "$FALLBACK_ROMPATH"
}

if [ -n "${MAME_BIN:-}" ]; then
  MAME_BIN="$MAME_BIN"
elif [ -f "$LOCAL_MAME_BIN" ] && [ -x "$LOCAL_MAME_BIN" ]; then
  MAME_BIN="$LOCAL_MAME_BIN"
else
  if [ -e "$LOCAL_MAME_BIN" ]; then
    LOCAL_MAME_INVALID="$LOCAL_MAME_BIN exists but is not an executable regular file; ignoring it and falling back to PATH mame"
  fi
  MAME_BIN="mame"
fi
EMUCAP_DATA_ROOT="$(emucap_data_root)"
RUN_DIR="$EMUCAP_DATA_ROOT/mame-pc98/$PORT"
FALLBACK_ROMPATH="$EMUCAP_DATA_ROOT/mame-pc98/roms"
DEFAULT_ROMPATH="$(default_rompath)"
ROMPATH="${MAME_ROMPATH:-$DEFAULT_ROMPATH}"
if [ -n "${MAME_GDB_PORT:-}" ]; then
  GDB_PORT="$MAME_GDB_PORT"
  case "$GDB_PORT" in
    ''|*[!0-9]*)
      echo "ERROR: MAME_GDB_PORT must be a decimal TCP port: $GDB_PORT" >&2
      exit 2
      ;;
  esac
  if [ "$GDB_PORT" -lt 1 ] || [ "$GDB_PORT" -gt 65535 ]; then
    echo "ERROR: MAME_GDB_PORT out of range: $GDB_PORT" >&2
    exit 2
  fi
elif [ "$PORT" -le 64535 ]; then
  GDB_PORT="$((PORT + 1000))"
else
  echo "ERROR: EMUCAP_PORT=$PORT is too high for default MAME_GDB_PORT=EMUCAP_PORT+1000." >&2
  exit 2
fi
BACKEND="lua-gdbstub"
HEADLESS="${MAME_HEADLESS:-1}"
WAIT="${EMUCAP_LAUNCH_WAIT:-20}"
POST_CONNECT_GRACE="${EMUCAP_POST_CONNECT_GRACE:-2}"
LOG="${EMUCAP_LOG:-$RUN_DIR/mame-pc98.log}"
MAME_HOME="${MAME_HOME:-$RUN_DIR/home}"
MAME_PIDFILE="$RUN_DIR/mame.pid"
BRIDGE_PIDFILE="$RUN_DIR/bridge.pid"
BRIDGE="${EMUCAP_PC98_BRIDGE_BIN:-}"
if [ -z "$BRIDGE" ]; then
  for candidate in "$ROOT/target/release/emucap-mame-pc98-bridge" \
                   "$ROOT/target/debug/emucap-mame-pc98-bridge"; do
    if [ -x "$candidate" ]; then BRIDGE="$candidate"; break; fi
  done
fi
if [ -z "$BRIDGE" ] && command -v emucap-mame-pc98-bridge >/dev/null 2>&1; then
  BRIDGE="$(command -v emucap-mame-pc98-bridge)"
fi
EMUCAP_BUILD_HASH="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
PLUGINPATH="${MAME_PLUGINPATH:-$HERE/plugins}"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi
MAME_CBUS0_DEFAULTED=0
if [ "${MAME_CBUS0+x}" != "x" ] && [ "$MACHINE" = "pc9801rs" ]; then
  # The pc9801rs romset from a local PC-9801RS BIOS set does not include
  # the default pc9801_26 sound-card ROM.  Disable cbus:0 unless explicitly set.
  MAME_CBUS0=""
  MAME_CBUS0_DEFAULTED=1
fi

[ -f "$MEDIA" ] || { echo "ERROR: media not found: $MEDIA" >&2; exit 1; }
if [ -n "$LOCAL_MAME_INVALID" ]; then
  echo "WARN: $LOCAL_MAME_INVALID" >&2
fi
command -v "$MAME_BIN" >/dev/null 2>&1 || { echo "ERROR: MAME not found: $MAME_BIN" >&2; exit 1; }
[ -n "$BRIDGE" ] && [ -x "$BRIDGE" ] || {
  echo "ERROR: Rust PC-98 bridge not found; build emucap-mame-pc98-bridge or set EMUCAP_PC98_BRIDGE_BIN" >&2
  exit 1
}
if [ "$HEADLESS" != "1" ] && [ "${MAME_ALLOW_VISIBLE:-0}" != "1" ]; then
  echo "ERROR: visible MAME launch is disabled by default. Set MAME_ALLOW_VISIBLE=1 with MAME_HEADLESS=0 if a window is intentional." >&2
  exit 2
fi
export MAME_GDB_PORT="$GDB_PORT"
if [ -f "$LOCAL_MAME_RAW_BIN" ] && [ -x "$LOCAL_MAME_RAW_BIN" ] && [ "$MAME_BIN" = "$LOCAL_MAME_BIN" ]; then
  export EMUCAP_MAME_RAW_BIN="${EMUCAP_MAME_RAW_BIN:-$LOCAL_MAME_RAW_BIN}"
fi

tail_log() {
  echo "---- MAME PC-98 log: $LOG ----" >&2
  if [ -s "$LOG" ]; then
    tail -n 180 "$LOG" >&2
  else
    echo "(log empty or missing)" >&2
  fi
}

kill_ours() {
  local pid="$1"
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null || true
  sleep 1
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
}

connected_pid() {
  local pid="$1"
  command -v lsof >/dev/null 2>&1 || return 2
  lsof -nP -a -p "$pid" -iTCP:"$PORT" -sTCP:ESTABLISHED >/dev/null 2>&1
}

if command -v lsof >/dev/null 2>&1; then
  LISTENER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null \
    | awk 'NR > 1 { print $1 ":" $2 }' \
    | sort -u \
    | tr '\n' ' ' || true)"
  if [ -z "$LISTENER" ]; then
    echo "ERROR: no MCP listener on port $PORT; call emucap status first." >&2
    exit 3
  fi
fi

# A dead MAME generation may leave only its bridge connected to the MCP. Clean that bridge before
# the generic in-use check, but only when both sides of the ownership proof agree: the recorded
# emulator PID is no longer alive, and the recorded bridge command names this bridge with this
# emucap/GDB endpoint. A live or ambiguous PID is never killed by an ordinary launch.
OLD_BRIDGE="$(cat "$BRIDGE_PIDFILE" 2>/dev/null || true)"
OLD_MAME="$(cat "$MAME_PIDFILE" 2>/dev/null || true)"
case "$OLD_MAME" in
  ''|*[!0-9]*) OLD_MAME_ALIVE=0 ;;
  *) if kill -0 "$OLD_MAME" 2>/dev/null; then OLD_MAME_ALIVE=1; else OLD_MAME_ALIVE=0; fi ;;
esac
case "$OLD_BRIDGE" in
  ''|*[!0-9]*) OLD_BRIDGE_ALIVE=0 ;;
  *) if kill -0 "$OLD_BRIDGE" 2>/dev/null; then OLD_BRIDGE_ALIVE=1; else OLD_BRIDGE_ALIVE=0; fi ;;
esac
if [ "$OLD_BRIDGE_ALIVE" = "1" ]; then
  OLD_BRIDGE_COMMAND="$(ps -p "$OLD_BRIDGE" -o command= 2>/dev/null || true)"
  if [ "$OLD_MAME_ALIVE" = "0" ] \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -F 'emucap-mame-pc98-bridge' >/dev/null \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -F " $PORT " >/dev/null \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -E "127\\.0\\.0\\.1:${GDB_PORT}([[:space:]]|$)" >/dev/null; then
    kill_ours "$OLD_BRIDGE"
    OLD_BRIDGE_ALIVE=0
  else
    echo "ERROR: recorded bridge PID $OLD_BRIDGE is live but its generation is live or ambiguous; inspect status before replacing it." >&2
    exit 3
  fi
fi
if [ "$OLD_MAME_ALIVE" = "1" ]; then
  echo "ERROR: recorded MAME PID $OLD_MAME is still live; use the MCP replace path or stop that exact generation first." >&2
  exit 3
fi

# Once the exact dead-generation orphan is gone, any remaining established connection is foreign
# or lacks enough identity evidence. Refuse without killing it.
if command -v lsof >/dev/null 2>&1; then
  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null \
    | awk 'NR > 1 && $1 ~ /(python|mame|MAME|mednafen|Mesen|Flycast)/ { print $2 }' \
    | sort -u \
    | tr '\n' ' ' || true)"
  if [ -n "$INUSE" ]; then
    echo "ERROR: port $PORT already has an emulator/bridge connection (PID: $INUSE)." >&2
    exit 3
  fi
fi

mkdir -p "$RUN_DIR" "$(dirname "$LOG")" "$MAME_HOME"
if [ "$ROMPATH" = "$FALLBACK_ROMPATH" ]; then
  mkdir -p "$ROMPATH"
fi

: >"$LOG"
{
  echo "emucap MAME PC-98 launch"
  echo "  media=$MEDIA"
  echo "  port=$PORT"
  echo "  name=${NAME:-<none>}"
  echo "  session_token=${SESSION_TOKEN:+present}"
  echo "  token_file=$TOKEN_FILE"
  echo "  machine=$MACHINE"
  echo "  mame_bin=$MAME_BIN"
  if [ -n "$LOCAL_MAME_INVALID" ]; then
    echo "  local_mame_ignored=$LOCAL_MAME_INVALID"
  fi
  echo "  mame_raw_bin=${EMUCAP_MAME_RAW_BIN:-<none>}"
  echo "  rompath=$ROMPATH"
  echo "  mame_home=$MAME_HOME"
  echo "  backend=$BACKEND"
  echo "  headless=$HEADLESS"
  echo "  pluginpath=$PLUGINPATH"
  echo "  gdb_port=$GDB_PORT"
  if [ "${MAME_CBUS0+x}" = "x" ]; then
    echo "  cbus0=${MAME_CBUS0:-<empty>}"
    echo "  cbus0_defaulted=$MAME_CBUS0_DEFAULTED"
  fi
  echo "  wait=${WAIT}s"
  echo "  post_connect_grace=${POST_CONNECT_GRACE}s"
} >>"$LOG"

ARGS=(
  "$MACHINE"
  -rompath "$ROMPATH"
  -homepath "$MAME_HOME"
  -cfg_directory "$MAME_HOME/cfg"
  -nvram_directory "$MAME_HOME/nvram"
  -input_directory "$MAME_HOME/inp"
  -state_directory "$MAME_HOME/sta"
  -snapshot_directory "$MAME_HOME/snap"
  -diff_directory "$MAME_HOME/diff"
  -comment_directory "$MAME_HOME/comments"
  -skip_gameinfo
  -debug
)
ARGS+=(
  -debugger none
  -pluginspath "$PLUGINPATH"
  -plugins
  -plugin emucap_gdbstub
)
if [ "${MAME_READCONFIG:-0}" != "1" ]; then
  ARGS+=(-noreadconfig)
fi
if [ "$HEADLESS" = "1" ]; then
  export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-dummy}"
  ARGS+=(-video none -videodriver dummy -window -nomaximize -sound none -keyboardprovider none -mouseprovider none -output none)
else
  ARGS+=(-window -nomaximize -sound none)
fi
if [ "${MAME_CBUS0+x}" = "x" ]; then
  ARGS+=(-cbus:0 "$MAME_CBUS0")
fi

case "${MEDIA##*.}" in
  hdi|HDI)
    ARGS+=(-hard "$MEDIA")
    ;;
  *)
    ARGS+=(-flop1 "$MEDIA")
    ;;
esac
if [ -n "${MAME_FLOP2:-}" ]; then
  ARGS+=(-flop2 "$MAME_FLOP2")
fi

nohup "$MAME_BIN" "${ARGS[@]}" </dev/null >>"$LOG" 2>&1 &
MAME_PID="$!"
echo "$MAME_PID" >"$MAME_PIDFILE"
disown "$MAME_PID" 2>/dev/null || true

if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    if ! kill -0 "$MAME_PID" 2>/dev/null; then
      echo "ERROR: MAME exited before gdbstub was ready (pid=$MAME_PID)." >&2
      tail_log
      exit 4
    fi
    if lsof -nP -a -p "$MAME_PID" -iTCP:"$GDB_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done
  if ! lsof -nP -a -p "$MAME_PID" -iTCP:"$GDB_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "ERROR: MAME did not open gdbstub port $GDB_PORT within ${WAIT}s." >&2
    tail_log
    kill_ours "$MAME_PID"
    exit 4
  fi
fi

EMUCAP_NAME="$NAME" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$MEDIA" \
  EMUCAP_BUILD_HASH="$EMUCAP_BUILD_HASH" EMUCAP_EMULATOR_PID="$MAME_PID" \
  nohup "$BRIDGE" "$PORT" "127.0.0.1:$GDB_PORT" </dev/null >>"$LOG" 2>&1 &
BRIDGE_PID="$!"
echo "$BRIDGE_PID" >"$BRIDGE_PIDFILE"
disown "$BRIDGE_PID" 2>/dev/null || true

if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
      echo "ERROR: bridge exited before emucap connection (pid=$BRIDGE_PID)." >&2
      tail_log
      kill_ours "$MAME_PID"
      exit 4
    fi
    if connected_pid "$BRIDGE_PID"; then
      sleep "$POST_CONNECT_GRACE"
      if ! kill -0 "$BRIDGE_PID" 2>/dev/null || ! connected_pid "$BRIDGE_PID"; then
        echo "ERROR: bridge lost emucap connection after connect (pid=$BRIDGE_PID)." >&2
        tail_log
        kill_ours "$BRIDGE_PID"
        kill_ours "$MAME_PID"
        exit 4
      fi
      echo "MAME PC-98 bridge connected: mame_pid=$MAME_PID bridge_pid=$BRIDGE_PID port=$PORT gdb_port=$GDB_PORT machine=$MACHINE log=$LOG"
      exit 0
    fi
    sleep 1
  done
fi

echo "ERROR: bridge did not connect to EMUCAP_PORT=$PORT within ${WAIT}s." >&2
tail_log
kill_ours "$BRIDGE_PID"
kill_ours "$MAME_PID"
exit 4
