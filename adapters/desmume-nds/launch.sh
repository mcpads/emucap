#!/usr/bin/env bash
# Launch headless DeSmuME (Nintendo DS) with its ARM9/ARM7 GDB stubs and attach the
# emucap NDS bridge.
#
# Usage:
#   launch.sh <rom.nds> <EMUCAP_PORT> [EMUCAP_NAME]
#
# Optional environment:
#   EMUCAP_DESMUME_BIN=/path/to/desmume-cli   default: adapters/desmume-nds/work/.../desmume-cli (built by build.sh)
#   EMUCAP_NDS_BRIDGE_BIN=/path/to/bridge     default: target/release/emucap-desmume-nds-bridge
#   NDS_ARM9_GDB_PORT=<port>                  default: EMUCAP_PORT + 1000
#   NDS_ARM7_GDB_PORT=<port>                  default: EMUCAP_PORT + 1001
#   EMUCAP_LOG=/path/to/custom.log            default: <emucap-data>/desmume-nds/<port>/desmume-nds.log
set -euo pipefail

usage() { echo "usage: $0 <rom.nds> <EMUCAP_PORT> [EMUCAP_NAME]" >&2; }
if [ "$#" -lt 2 ]; then usage; exit 2; fi

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
. "$HERE/../_common/runtime-env.sh"
ROM="$1"
PORT="$2"
NAME="${3:-}"
case "$PORT" in ''|*[!0-9]*) echo "ERROR: EMUCAP_PORT must be a decimal TCP port: $PORT" >&2; exit 2;; esac
if [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65534 ]; then echo "ERROR: EMUCAP_PORT out of range: $PORT" >&2; exit 2; fi

emucap_data_root() {
  if [ -n "${EMUCAP_EMU_HOME:-}" ]; then printf '%s\n' "$EMUCAP_EMU_HOME"; return; fi
  case "$(uname -s 2>/dev/null || echo unknown)" in
    Darwin) printf '%s\n' "${HOME:-/tmp}/Library/Application Support/emucap" ;;
    MINGW*|MSYS*|CYGWIN*) printf '%s\n' "${LOCALAPPDATA:-${HOME:-/tmp}/AppData/Local}/emucap" ;;
    *) printf '%s\n' "${XDG_DATA_HOME:-${HOME:-/tmp}/.local/share}/emucap" ;;
  esac
}

# GDB ports (DeSmuME opens one stub per CPU). 파생 PORT+1000/+1001은 인접 emucap 포트끼리 겹친다
# (세션 N의 ARM7 = N+1의 ARM9), 그래서 OS가 배정한 자유 포트를 쓴다 — 두 소켓을 동시에 바인딩해 서로
# 다름을 보장하고 닫아 desmume가 다시 바인딩하게 한다. env override는 유지(Rust launcher와 일관).
if command -v python3 >/dev/null 2>&1; then
  _PORTS="$(python3 -c 'import socket
a=socket.socket(); a.bind(("127.0.0.1",0))
b=socket.socket(); b.bind(("127.0.0.1",0))
print(a.getsockname()[1], b.getsockname()[1])
a.close(); b.close()' 2>/dev/null)"
  _A9="${_PORTS%% *}"; _A7="${_PORTS##* }"
fi
: "${_A9:=$((PORT + 1000))}"; : "${_A7:=$((PORT + 11000))}"   # python3 없음/실패 시 wide-gap 폴백(인접 세션 비충돌)
ARM9_PORT="${NDS_ARM9_GDB_PORT:-$_A9}"
ARM7_PORT="${NDS_ARM7_GDB_PORT:-$_A7}"
for p in "$ARM9_PORT" "$ARM7_PORT"; do
  case "$p" in ''|*[!0-9]*) echo "ERROR: GDB port must be decimal: $p" >&2; exit 2;; esac
  if [ "$p" -lt 1 ] || [ "$p" -gt 65535 ]; then echo "ERROR: GDB port out of range: $p" >&2; exit 2; fi
done

DATA_ROOT="$(emucap_data_root)"
RUN_DIR="$DATA_ROOT/desmume-nds/$PORT"
LOG="${EMUCAP_LOG:-$RUN_DIR/desmume-nds.log}"
EMU_PIDFILE="$RUN_DIR/desmume.pid"
BRIDGE_PIDFILE="$RUN_DIR/bridge.pid"
WAIT="${EMUCAP_LAUNCH_WAIT:-20}"
POST_CONNECT_GRACE="${EMUCAP_POST_CONNECT_GRACE:-2}"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi
EMUCAP_BUILD_HASH="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"

# Resolve the headless desmume-cli binary.
if [ -n "${EMUCAP_DESMUME_BIN:-}" ]; then
  EMU_BIN="$EMUCAP_DESMUME_BIN"
else
  EMU_BIN="$HERE/work/src/desmume/src/frontend/posix/build-headless/cli/desmume-cli"
fi
# Resolve the Rust NDS bridge binary.
BRIDGE_NAME="emucap-desmume-nds-bridge"
if [ -n "${EMUCAP_NDS_BRIDGE_BIN:-}" ]; then
  BRIDGE_BIN="$EMUCAP_NDS_BRIDGE_BIN"
elif [ -x "$REPO_ROOT/target/release/$BRIDGE_NAME" ]; then
  BRIDGE_BIN="$REPO_ROOT/target/release/$BRIDGE_NAME"
elif [ -x "$REPO_ROOT/target/debug/$BRIDGE_NAME" ]; then
  BRIDGE_BIN="$REPO_ROOT/target/debug/$BRIDGE_NAME"
else
  BRIDGE_BIN="$(command -v "$BRIDGE_NAME" || true)"
fi

[ -f "$ROM" ] || { echo "ERROR: ROM not found: $ROM" >&2; exit 1; }
[ -x "$EMU_BIN" ] || { echo "ERROR: desmume-cli not found (build it): $EMU_BIN" >&2; exit 1; }
[ -n "$BRIDGE_BIN" ] && [ -x "$BRIDGE_BIN" ] || { echo "ERROR: NDS bridge binary not found; build emucap-desmume-nds-bridge or set EMUCAP_NDS_BRIDGE_BIN" >&2; exit 1; }

tail_log() { echo "---- DeSmuME NDS log: $LOG ----" >&2; [ -s "$LOG" ] && tail -n 120 "$LOG" >&2 || echo "(log empty)" >&2; }
kill_ours() {
  local pid="$1"; [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null || true; sleep 1
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true   # stub ignores SIGTERM → SIGKILL
}
connected_pid() { command -v lsof >/dev/null 2>&1 || return 2; lsof -nP -a -p "$1" -iTCP:"$PORT" -sTCP:ESTABLISHED >/dev/null 2>&1; }

# MCP listener must be up; refuse if the port already has an emulator/bridge connection.
if command -v lsof >/dev/null 2>&1; then
  lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 || { echo "ERROR: no MCP listener on port $PORT; call emucap status first." >&2; exit 3; }
  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null | awk 'NR>1 && $1 ~ /(desmume|emucap-desmume|python|mednafen|Mesen|Flycast|mame)/ {print $2}' | sort -u | tr '\n' ' ' || true)"
  [ -z "$INUSE" ] || { echo "ERROR: port $PORT already has an emulator/bridge connection (PID: $INUSE)." >&2; exit 3; }
fi

# Clean up only orphans this launcher started (pidfile PID must still be the right process).
OLD_BRIDGE="$(cat "$BRIDGE_PIDFILE" 2>/dev/null || true)"
OLD_EMU="$(cat "$EMU_PIDFILE" 2>/dev/null || true)"
[ -n "$OLD_BRIDGE" ] && kill -0 "$OLD_BRIDGE" 2>/dev/null && ps -p "$OLD_BRIDGE" -o command= 2>/dev/null | grep -qi "$BRIDGE_NAME" && kill_ours "$OLD_BRIDGE" || true
[ -n "$OLD_EMU" ] && kill -0 "$OLD_EMU" 2>/dev/null && ps -p "$OLD_EMU" -o command= 2>/dev/null | grep -qi 'desmume-cli' && kill_ours "$OLD_EMU" || true

mkdir -p "$RUN_DIR"
: >"$LOG"
{
  echo "emucap DeSmuME NDS launch"
  echo "  rom=$ROM"; echo "  port=$PORT"; echo "  name=${NAME:-<none>}"
  echo "  session_token=${SESSION_TOKEN:+present}"; echo "  arm9_gdb=$ARM9_PORT"; echo "  arm7_gdb=$ARM7_PORT"
  echo "  desmume_bin=$EMU_BIN"; echo "  bridge_bin=$BRIDGE_BIN"
} >>"$LOG"

spawn_detached() {  # pidfile exe args... -> prints pid
  local pidfile="$1"; shift
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$LOG" "$pidfile" "$@" <<'PY'
import os, subprocess, sys
log_path, pidfile, exe, *args = sys.argv[1:]
log = open(log_path, "ab", buffering=0)
proc = subprocess.Popen([exe, *args], stdin=open(os.devnull, "rb"), stdout=log,
                        stderr=subprocess.STDOUT, close_fds=True, start_new_session=True,
                        env=os.environ.copy())
open(pidfile, "w").write(f"{proc.pid}\n"); print(proc.pid)
PY
  else
    nohup "$@" </dev/null >>"$LOG" 2>&1 & echo "$!" | tee "$pidfile"; disown 2>/dev/null || true
  fi
}

# 1. Spawn headless DeSmuME with both GDB stubs; wait for both ports to LISTEN.
EMU_PID="$(spawn_detached "$EMU_PIDFILE" "$EMU_BIN" --arm9gdb "$ARM9_PORT" --arm7gdb "$ARM7_PORT" --disable-sound "$ROM")"
if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    kill -0 "$EMU_PID" 2>/dev/null || { echo "ERROR: desmume-cli exited before gdb was ready (pid=$EMU_PID)." >&2; tail_log; exit 4; }
    if lsof -nP -a -p "$EMU_PID" -iTCP:"$ARM9_PORT" -sTCP:LISTEN >/dev/null 2>&1 \
       && lsof -nP -a -p "$EMU_PID" -iTCP:"$ARM7_PORT" -sTCP:LISTEN >/dev/null 2>&1; then break; fi
    sleep 1
  done
  lsof -nP -a -p "$EMU_PID" -iTCP:"$ARM9_PORT" -sTCP:LISTEN >/dev/null 2>&1 \
    || { echo "ERROR: desmume-cli did not open GDB ports $ARM9_PORT/$ARM7_PORT within ${WAIT}s." >&2; tail_log; kill_ours "$EMU_PID"; exit 4; }
fi

# 2. Spawn the bridge; wait for it to connect to the emucap port.
BRIDGE_PID="$(EMUCAP_NAME="$NAME" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$ROM" EMUCAP_BUILD_HASH="$EMUCAP_BUILD_HASH" \
  spawn_detached "$BRIDGE_PIDFILE" "$BRIDGE_BIN" "$PORT" "127.0.0.1:$ARM9_PORT" "127.0.0.1:$ARM7_PORT")"
if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "ERROR: bridge exited before emucap connection (pid=$BRIDGE_PID)." >&2; tail_log; kill_ours "$EMU_PID"; exit 4; }
    if connected_pid "$BRIDGE_PID"; then
      sleep "$POST_CONNECT_GRACE"
      connected_pid "$BRIDGE_PID" || { echo "ERROR: bridge lost emucap connection after connect." >&2; tail_log; kill_ours "$BRIDGE_PID"; kill_ours "$EMU_PID"; exit 4; }
      echo "DeSmuME NDS bridge connected: desmume_pid=$EMU_PID bridge_pid=$BRIDGE_PID port=$PORT arm9_gdb=$ARM9_PORT arm7_gdb=$ARM7_PORT log=$LOG"
      exit 0
    fi
    sleep 1
  done
fi
echo "ERROR: bridge did not connect to EMUCAP_PORT=$PORT within ${WAIT}s." >&2
tail_log; kill_ours "$BRIDGE_PID"; kill_ours "$EMU_PID"; exit 4
