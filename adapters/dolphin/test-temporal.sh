#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$(mktemp "${TMPDIR:-/tmp}/emucap-dolphin-temporal.XXXXXX")"
trap 'rm -f "$OUT"' EXIT
"${CXX:-c++}" -std=c++20 -pthread -Wall -Wextra -Werror -I"$HERE" "$HERE/EmuCapTemporalTest.cpp" -o "$OUT"
"$OUT"
echo "[dolphin-temporal] admission, progress and disconnect cleanup passed"
