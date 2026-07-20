#!/usr/bin/env bash
# Launch headless PPSSPP (PSP) with its debugger WebSocket and attach the emucap PSP bridge.
#
# Usage:
#   launch.sh <game.iso|game.cso|game.pbp> <EMUCAP_PORT> [EMUCAP_NAME]
#
# Optional environment:
#   EMUCAP_PPSSPP_BIN=/path/to/PPSSPPHeadless   default: adapters/ppsspp/work/ppsspp/build-headless/PPSSPPHeadless (built by build.sh)
#   EMUCAP_PSP_BRIDGE_BIN=/path/to/bridge       default: target/release/emucap-ppsspp-bridge
#   PSP_DEBUGGER_PORT=<port>                    default: an OS-assigned free port
#   EMUCAP_LOG=/path/to/custom.log               default: <emucap-data>/ppsspp/<port>/ppsspp.log
#
# Upstream command-line constraints:
#   - The content is passed *positionally*, never via -m/--mount (that flag only mounts a second
#     image on umd1: for ELF+CSO test harnesses; passed alone it leaves the boot list empty).
#   - --timeout is never passed: it aborts the run after N wall-clock seconds regardless of
#     debugger/WebSocket activity, which would kill an interactive debugging session.
set -euo pipefail

usage() { echo "usage: $0 <game.iso|game.cso|game.pbp> <EMUCAP_PORT> [EMUCAP_NAME]" >&2; }
if [ "$#" -lt 2 ]; then usage; exit 2; fi

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
. "$HERE/../_common/runtime-env.sh"
CONTENT="$1"
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

# Debugger WebSocket port. A single OS-assigned free port avoids collisions across concurrent PSP
# sessions (same rationale as NDS's ARM9/ARM7 GDB ports); env override kept for parity with the
# Rust launcher.
if [ -n "${PSP_DEBUGGER_PORT:-}" ]; then
  WS_PORT="$PSP_DEBUGGER_PORT"
elif command -v python3 >/dev/null 2>&1; then
  WS_PORT="$(python3 -c 'import socket
s=socket.socket(); s.bind(("127.0.0.1",0))
print(s.getsockname()[1])
s.close()' 2>/dev/null)"
fi
: "${WS_PORT:=$((PORT + 1000))}"   # python3 없음/실패 시 wide-gap 폴백(인접 세션 비충돌)
case "$WS_PORT" in ''|*[!0-9]*) echo "ERROR: PSP_DEBUGGER_PORT must be decimal: $WS_PORT" >&2; exit 2;; esac
if [ "$WS_PORT" -lt 1 ] || [ "$WS_PORT" -gt 65535 ]; then echo "ERROR: PSP_DEBUGGER_PORT out of range: $WS_PORT" >&2; exit 2; fi

DATA_ROOT="$(emucap_data_root)"
RUN_DIR="$DATA_ROOT/ppsspp/$PORT"
LOG="${EMUCAP_LOG:-$RUN_DIR/ppsspp.log}"
EMU_PIDFILE="$RUN_DIR/ppsspp.pid"
BRIDGE_PIDFILE="$RUN_DIR/bridge.pid"
WAIT="${EMUCAP_LAUNCH_WAIT:-20}"
POST_CONNECT_GRACE="${EMUCAP_POST_CONNECT_GRACE:-2}"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi
EMUCAP_BUILD_HASH="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"

# Resolve the headless PPSSPPHeadless binary.
if [ -n "${EMUCAP_PPSSPP_BIN:-}" ]; then
  EMU_BIN="$EMUCAP_PPSSPP_BIN"
else
  EMU_BIN="$HERE/work/ppsspp/build-headless/PPSSPPHeadless"
fi
# Resolve the Rust PSP bridge binary.
BRIDGE_NAME="emucap-ppsspp-bridge"
if [ -n "${EMUCAP_PSP_BRIDGE_BIN:-}" ]; then
  BRIDGE_BIN="$EMUCAP_PSP_BRIDGE_BIN"
elif [ -x "$REPO_ROOT/target/release/$BRIDGE_NAME" ]; then
  BRIDGE_BIN="$REPO_ROOT/target/release/$BRIDGE_NAME"
elif [ -x "$REPO_ROOT/target/debug/$BRIDGE_NAME" ]; then
  BRIDGE_BIN="$REPO_ROOT/target/debug/$BRIDGE_NAME"
else
  BRIDGE_BIN="$(command -v "$BRIDGE_NAME" || true)"
fi

[ -f "$CONTENT" ] || { echo "ERROR: content not found: $CONTENT" >&2; exit 1; }
[ -x "$EMU_BIN" ] || { echo "ERROR: PPSSPPHeadless not found (build it): $EMU_BIN" >&2; exit 1; }
[ -n "$BRIDGE_BIN" ] && [ -x "$BRIDGE_BIN" ] || { echo "ERROR: PSP bridge binary not found; build emucap-ppsspp-bridge or set EMUCAP_PSP_BRIDGE_BIN" >&2; exit 1; }

tail_log() { echo "---- PPSSPP PSP log: $LOG ----" >&2; [ -s "$LOG" ] && tail -n 120 "$LOG" >&2 || echo "(log empty)" >&2; }
kill_ours() {
  local pid="$1"; [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null || true; sleep 1
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
}
connected_pid() { command -v lsof >/dev/null 2>&1 || return 2; lsof -nP -a -p "$1" -iTCP:"$PORT" -sTCP:ESTABLISHED >/dev/null 2>&1; }

# MCP listener must be up.
if command -v lsof >/dev/null 2>&1; then
  lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 || { echo "ERROR: no MCP listener on port $PORT; call emucap status first." >&2; exit 3; }
fi

# Clean only a bridge whose recorded emulator is dead and whose command matches this complete
# endpoint tuple. A live or ambiguous recorded process requires an explicit replace decision.
OLD_BRIDGE="$(cat "$BRIDGE_PIDFILE" 2>/dev/null || true)"
OLD_EMU="$(cat "$EMU_PIDFILE" 2>/dev/null || true)"
case "$OLD_EMU" in
  ''|*[!0-9]*) OLD_EMU_ALIVE=0 ;;
  *) if kill -0 "$OLD_EMU" 2>/dev/null; then OLD_EMU_ALIVE=1; else OLD_EMU_ALIVE=0; fi ;;
esac
case "$OLD_BRIDGE" in
  ''|*[!0-9]*) OLD_BRIDGE_ALIVE=0 ;;
  *) if kill -0 "$OLD_BRIDGE" 2>/dev/null; then OLD_BRIDGE_ALIVE=1; else OLD_BRIDGE_ALIVE=0; fi ;;
esac
if [ "$OLD_BRIDGE_ALIVE" = "1" ]; then
  OLD_BRIDGE_COMMAND="$(ps -p "$OLD_BRIDGE" -o command= 2>/dev/null || true)"
  if [ "$OLD_EMU_ALIVE" = "0" ] \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -F "$BRIDGE_NAME" >/dev/null \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -F " $PORT " >/dev/null \
     && printf '%s\n' "$OLD_BRIDGE_COMMAND" | grep -E "[[:space:]]${WS_PORT}([[:space:]]|$)" >/dev/null; then
    kill_ours "$OLD_BRIDGE"
  else
    echo "ERROR: recorded bridge PID $OLD_BRIDGE is live but its generation is live or ambiguous; inspect status before replacing it." >&2
    exit 3
  fi
fi
if [ "$OLD_EMU_ALIVE" = "1" ]; then
  echo "ERROR: recorded PPSSPP PID $OLD_EMU is still live; use the MCP replace path or stop that exact generation first." >&2
  exit 3
fi
if command -v lsof >/dev/null 2>&1; then
  INUSE="$(lsof -nP -iTCP:"$PORT" -sTCP:ESTABLISHED 2>/dev/null | awk 'NR>1 && $1 ~ /(PPSSPP|emucap-ppsspp|desmume|python|mednafen|Mesen|Flycast|mame)/ {print $2}' | sort -u | tr '\n' ' ' || true)"
  [ -z "$INUSE" ] || { echo "ERROR: port $PORT already has an emulator/bridge connection (PID: $INUSE)." >&2; exit 3; }
fi

mkdir -p "$RUN_DIR"
: >"$LOG"
{
  echo "emucap PPSSPP PSP launch"
  echo "  content=$CONTENT"; echo "  port=$PORT"; echo "  name=${NAME:-<none>}"
  echo "  session_token=${SESSION_TOKEN:+present}"; echo "  ws_port=$WS_PORT"
  echo "  ppsspp_bin=$EMU_BIN"; echo "  bridge_bin=$BRIDGE_BIN"
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

# 1. Spawn headless PPSSPP with the debugger WebSocket; wait for the port to LISTEN. The content is
#    positional (never -m/--mount — see the header note), and --timeout is never passed.
EMU_PID="$(spawn_detached "$EMU_PIDFILE" "$EMU_BIN" "--debugger=$WS_PORT" "--graphics=software" "$CONTENT")"
if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    kill -0 "$EMU_PID" 2>/dev/null || { echo "ERROR: PPSSPPHeadless exited before the debugger port was ready (pid=$EMU_PID)." >&2; tail_log; exit 4; }
    lsof -nP -a -p "$EMU_PID" -iTCP:"$WS_PORT" -sTCP:LISTEN >/dev/null 2>&1 && break
    sleep 1
  done
  lsof -nP -a -p "$EMU_PID" -iTCP:"$WS_PORT" -sTCP:LISTEN >/dev/null 2>&1 \
    || { echo "ERROR: PPSSPPHeadless did not open debugger port $WS_PORT within ${WAIT}s." >&2; tail_log; kill_ours "$EMU_PID"; exit 4; }
fi

# 2. Spawn the bridge; wait for it to connect to the emucap port. Unlike the NDS bridge, this
#    bridge makes a single WebSocket connect attempt with no retry, so step 1's readiness wait is
#    load-bearing here (not just a liveness check).
BRIDGE_PID="$(EMUCAP_NAME="$NAME" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$CONTENT" EMUCAP_BUILD_HASH="$EMUCAP_BUILD_HASH" EMUCAP_EMULATOR_PID="$EMU_PID" \
  spawn_detached "$BRIDGE_PIDFILE" "$BRIDGE_BIN" "$PORT" "$WS_PORT")"
if command -v lsof >/dev/null 2>&1; then
  for ((i = 0; i < WAIT; i++)); do
    kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "ERROR: bridge exited before emucap connection (pid=$BRIDGE_PID)." >&2; tail_log; kill_ours "$EMU_PID"; exit 4; }
    if connected_pid "$BRIDGE_PID"; then
      sleep "$POST_CONNECT_GRACE"
      connected_pid "$BRIDGE_PID" || { echo "ERROR: bridge lost emucap connection after connect." >&2; tail_log; kill_ours "$BRIDGE_PID"; kill_ours "$EMU_PID"; exit 4; }
      echo "PPSSPP PSP bridge connected: ppsspp_pid=$EMU_PID bridge_pid=$BRIDGE_PID port=$PORT ws_port=$WS_PORT log=$LOG"
      exit 0
    fi
    sleep 1
  done
fi
echo "ERROR: bridge did not connect to EMUCAP_PORT=$PORT within ${WAIT}s." >&2
tail_log; kill_ours "$BRIDGE_PID"; kill_ours "$EMU_PID"; exit 4
