#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-${EMUCAP_DOLPHIN_WORK:-$HERE/work}/dolphin-src}"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/emucap-dolphin-mixer.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT
FLAGS=(-std=c++23 -fno-exceptions -pthread -g -O1 -Wall -Wextra
       -ffunction-sections -fdata-sections -DFMT_HEADER_ONLY)
case "${EMUCAP_SANITIZERS:-}" in
  '') ;;
  thread|address,undefined) FLAGS+=("-fsanitize=$EMUCAP_SANITIZERS") ;;
  *) echo "EMUCAP_SANITIZERS must be thread or address,undefined" >&2; exit 1 ;;
esac
if [ "$(uname)" = Darwin ]; then
  LIBS=(-Wl,-dead_strip -liconv)
else
  LIBS=(-Wl,--gc-sections)
fi
"${CXX:-c++}" "${FLAGS[@]}" -I"$SRC/Source/Core" -I"$SRC/Externals/fmt/fmt/include" \
  "$HERE/MixerTest.cpp" "$SRC/Source/Core/AudioCommon/Mixer.cpp" \
  "$SRC/Source/Core/Common/Config/Config.cpp" "$SRC/Source/Core/Common/Config/Layer.cpp" \
  "$SRC/Source/Core/Common/Config/ConfigInfo.cpp" "$SRC/Source/Core/Common/StringUtil.cpp" \
  "${LIBS[@]}" -o "$OUT/MixerTest"
"$OUT/MixerTest"
"$OUT/MixerTest" rates
