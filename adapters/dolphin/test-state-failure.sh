#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-${EMUCAP_DOLPHIN_WORK:-$HERE/work}/dolphin-src}"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/emucap-dolphin-state.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT
FLAGS=(-std=c++23 -g -O1 -Wall -Wextra -DFMT_HEADER_ONLY
       -ffunction-sections -fdata-sections)
case "${EMUCAP_SANITIZERS:-}" in
  '') ;;
  address,undefined) FLAGS+=("-fsanitize=$EMUCAP_SANITIZERS") ;;
  *) echo "EMUCAP_SANITIZERS must be address,undefined" >&2; exit 1 ;;
esac
if [ "$(uname)" = Darwin ]; then
  LIBS=(-Wl,-dead_strip)
else
  LIBS=(-Wl,--gc-sections)
fi
"${CXX:-c++}" "${FLAGS[@]}" -I"$SRC/Source/Core" -I"$SRC/Externals/fmt/fmt/include" \
  -I"$SRC/Externals/imgui/imgui" "$HERE/StateFailureTest.cpp" \
  "$SRC/Source/Core/VideoCommon/AbstractStagingTexture.cpp" \
  "$SRC/Source/Core/VideoCommon/AbstractTexture.cpp" \
  "$SRC/Source/Core/VideoCommon/TextureConfig.cpp" \
  "${LIBS[@]}" -o "$OUT/StateFailureTest"
"$OUT/StateFailureTest"
