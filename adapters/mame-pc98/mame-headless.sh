#!/usr/bin/env bash
# Safe MAME frontend for agent-run probes.
#
# This wrapper is intentionally conservative: unless visible mode is explicitly
# allowed, it appends headless/video-isolating options after the caller's
# arguments so saved mame.ini fullscreen/window settings cannot steal focus.
# Host audio remains disabled unless MAME_ALLOW_SOUND=1 is explicit; visibility
# and audio authorization are independent.
set -euo pipefail

usage() {
  echo "usage: EMUCAP_MAME_RAW_BIN=/path/to/mame $0 [mame args...]" >&2
}

RAW="${EMUCAP_MAME_RAW_BIN:-${MAME_RAW_BIN:-}}"
if [ -z "$RAW" ]; then
  echo "ERROR: EMUCAP_MAME_RAW_BIN is required for mame-headless.sh." >&2
  usage
  exit 2
fi
if [ ! -x "$RAW" ]; then
  echo "ERROR: raw MAME binary is not executable: $RAW" >&2
  exit 2
fi

if [ "${MAME_ALLOW_VISIBLE:-0}" = "1" ]; then
  exec "$RAW" "$@" -mouse
fi

export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-dummy}"

SAFE_ARGS=(
  -noreadconfig
  -video none
  -videodriver dummy
  -window
  -nomaximize
  -mouse
  -keyboardprovider none
  -mouseprovider none
  -output none
)
if [ "${MAME_ALLOW_SOUND:-0}" != "1" ]; then
  SAFE_ARGS+=(-sound none)
fi

exec "$RAW" "$@" "${SAFE_ARGS[@]}"
