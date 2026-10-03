#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$(mktemp "${TMPDIR:-/tmp}/emucap-dolphin-temporal.XXXXXX")"
trap 'rm -f "$OUT"' EXIT
"${CXX:-c++}" -std=c++20 -fno-exceptions -pthread -Wall -Wextra -Werror -I"$HERE" "$HERE/EmuCapTemporalTest.cpp" -o "$OUT"
"$OUT"
"${CXX:-c++}" -std=c++20 -fno-exceptions -pthread -Wall -Wextra -Werror -I"$HERE" "$HERE/EmuCapOwnerTest.cpp" -o "$OUT"
"$OUT"
SRC="${EMUCAP_DOLPHIN_WORK:-$HERE/work}/dolphin-src"
"${CXX:-c++}" -std=c++20 -fno-exceptions -pthread -Wall -Wextra -Werror -I"$HERE" \
  -isystem "$SRC/Externals/tinygltf/tinygltf" "$HERE/EmuCapWireTest.cpp" -o "$OUT"
"$OUT"
echo "[dolphin-temporal] admission, progress, cleanup and queued-mutation deadlines passed"
