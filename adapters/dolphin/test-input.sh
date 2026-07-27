#!/usr/bin/env bash
# Compile and run the production Wii-input request state machine without linking Dolphin.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PICOJSON="${EMUCAP_DOLPHIN_PICOJSON:-$HERE/work/dolphin-src/Externals/picojson}"
[ -f "$PICOJSON/picojson.h" ] || {
  echo "ERROR: picojson.h not found under $PICOJSON; run adapters/dolphin/build.sh first" >&2
  exit 1
}

OUT="$(mktemp "${TMPDIR:-/tmp}/emucap-dolphin-input.XXXXXX")"
trap 'rm -f "$OUT"' EXIT
"${CXX:-c++}" -std=c++20 -Wall -Wextra -Werror -I"$HERE" -I"$PICOJSON" \
  "$HERE/EmuCapInput.cpp" "$HERE/EmuCapInputTest.cpp" -o "$OUT"
"$OUT"
echo "[dolphin-input] production request state machine passed"
