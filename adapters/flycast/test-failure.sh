#!/bin/sh
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT="${TMPDIR:-/tmp}/emucap-flycast-failure-test-$$"
trap 'rm -f "$OUT"' EXIT INT TERM
"${CXX:-c++}" -std=c++17 -Wall -Wextra -Werror \
  "$HERE/emucap_failure.cpp" "$HERE/emucap_failure_test.cpp" -o "$OUT"
"$OUT"
