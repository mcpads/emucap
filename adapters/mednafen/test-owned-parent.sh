#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/emucap-mednafen-owned-parent.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
CACHE="${EMUCAP_MEDNAFEN_WORK:-$HERE/work}/emucap-json-3.11.3.hpp"
bash "$HERE/fetch-json.sh" "$CACHE"
cp "$CACHE" "$TMP/emucap_json.hpp"
"${CXX:-c++}" -std=c++11 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -fno-omit-frame-pointer -I"$TMP" "$HERE/emucap_owned_test.cpp" -o "$TMP/test"
"$TMP/test"
echo '[mednafen-owned-parent] dispatcher admission, obligations, identity and cleanup passed'
