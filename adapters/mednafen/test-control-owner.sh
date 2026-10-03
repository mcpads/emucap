#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/emucap-mednafen-owner.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
"${CXX:-c++}" -std=c++11 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -fno-omit-frame-pointer "$HERE/emucap_control_owner_test.cpp" -o "$TMP/test"
"$TMP/test"
"${CXX:-c++}" -std=c++20 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -fno-omit-frame-pointer "$HERE/emucap_control_owner_reference_test.cpp" -o "$TMP/reference"
"$TMP/reference"
echo '[mednafen-owner] obligations, stale cleanup, retirement and reference transitions passed'
