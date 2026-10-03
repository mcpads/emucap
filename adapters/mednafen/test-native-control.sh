#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/emucap-mednafen-native-control.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
"${CXX:-c++}" -std=c++11 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -fno-omit-frame-pointer "$HERE/emucap_native_control_test.cpp" -o "$TMP/test"
"$TMP/test"
echo '[mednafen-native-control] generation and nested stop observations passed'
