#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-${EMUCAP_DOLPHIN_WORK:-$HERE/work}/dolphin-src}"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/emucap-dolphin-settings.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT
FLAGS=(-std=c++23 -fno-exceptions -pthread -g -O1 -Wall -Wextra -Werror)
case "${EMUCAP_SANITIZERS:-}" in
  '') ;;
  thread|address,undefined) FLAGS+=("-fsanitize=$EMUCAP_SANITIZERS") ;;
  *) echo "EMUCAP_SANITIZERS must be thread or address,undefined" >&2; exit 1 ;;
esac
for TEST in SettingsTest PacingTest; do
  "${CXX:-c++}" "${FLAGS[@]}" -I"$SRC/Source/Core" -I"$HERE" "$HERE/$TEST.cpp" \
    "$SRC/Source/Core/Common/Config/Config.cpp" \
    "$SRC/Source/Core/Common/Config/Layer.cpp" \
    "$SRC/Source/Core/Common/Config/ConfigInfo.cpp" -o "$OUT/$TEST"
  "$OUT/$TEST"
done

"${CXX:-c++}" "${FLAGS[@]}" -I"$HERE" "$HERE/AudioObservationTest.cpp" -o "$OUT/AudioObservationTest"
"$OUT/AudioObservationTest"
