#!/usr/bin/env bash
# PCSX-Redux + emucap 어댑터(emucap-live.lua) 기동.
#
# ⚠ 반드시 사용자의 GUI 세션 터미널에서 foreground로 실행한다(이 스크립트는 블로킹). PCSX-Redux는
#   GLFW 기반이라 실제 디스플레이/모니터 연결이 필요하다 — nohup/백그라운드/headless(-no-ui)로 띄우면
#   "ImGui assert: g.PlatformIO.Monitors.Size > 0 ... is false"로 GUI 초기화가 실패하고, 그러면
#   에뮬레이션 루프도 luv(libuv) 이벤트 루프도 안 돌아 emucap-mcp에 접속조차 안 된다. (Mesen은
#   Avalonia라 nohup 가능했지만 PCSX-Redux는 다르다.)
#
# 포트는 emucap-mcp의 status가 알려주는 listening_port를 쓴다(47800 하드코딩 금지 — 다중 세션 격리).
#
# 사용: launch.sh <disc.cue|exe> [EMUCAP_PORT]
set -euo pipefail

DISC="$1"; PORT="${2:-47800}"
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/runtime-env.sh"
LUA="$HERE/emucap-live.lua"
BIN="${PCSX_REDUX_BIN:-$HOME/pcsx-redux/bins/Release/pcsx-redux}"
TOKEN_FILE="$(emucap_session_token_file "$PORT")"
SESSION_TOKEN="${EMUCAP_SESSION_TOKEN:-}"
if [ -z "$SESSION_TOKEN" ] && [ -r "$TOKEN_FILE" ]; then
  SESSION_TOKEN="$(head -n 1 "$TOKEN_FILE" | tr -d '\r\n')"
fi

[ -x "$BIN" ] || { echo "ERROR: PCSX-Redux 바이너리 없음: $BIN (먼저 build.sh)" >&2; exit 1; }
[ -f "$DISC" ] || { echo "ERROR: 디스크 없음: $DISC" >&2; exit 1; }

if command -v lsof >/dev/null 2>&1; then
  LISTENER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null \
    | awk 'NR > 1 { print $1 ":" $2 }' \
    | sort -u \
    | tr '\n' ' ' || true)"
  if [ -z "$LISTENER" ]; then
    echo "ERROR: 포트 $PORT 에 MCP listener가 없다 — 먼저 emucap status로 listening_port를 받아라." >&2
    exit 3
  fi
fi

echo "PCSX-Redux 기동(foreground, GUI 창): port=$PORT disc=$DISC"
echo "  session_token=${SESSION_TOKEN:+present} token_file=$TOKEN_FILE"
echo "  창을 닫거나 Ctrl-C로 종료. emucap-mcp(EMUCAP_PORT=$PORT)에 자동 접속한다."
# 디스플레이 슬립 대응(macOS 전용 — caffeinate 있을 때만): 슬립이면 GLFW가 "Monitors.Size>0 is false"로
#   죽는다. 깨우고(caffeinate -u), 실행 동안 슬립 방지(caffeinate -dimsu가 바이너리를 자식으로 실행 —
#   종료되면 caffeinate도 종료). caffeinate 없는 플랫폼은 바이너리를 직접 exec.
if command -v caffeinate >/dev/null 2>&1; then
  caffeinate -u -t 5 >/dev/null 2>&1 & disown
  sleep 1
  EMUCAP_PORT="$PORT" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$DISC" exec caffeinate -dimsu "$BIN" -interpreter -iso "$DISC" -dofile "$LUA" -run
else
  EMUCAP_PORT="$PORT" EMUCAP_SESSION_TOKEN="$SESSION_TOKEN" EMUCAP_CONTENT="$DISC" exec "$BIN" -interpreter -iso "$DISC" -dofile "$LUA" -run
fi
