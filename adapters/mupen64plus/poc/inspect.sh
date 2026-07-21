#!/usr/bin/env bash
# Verify that the pinned N64 PoC has the debugger surface needed by the adapter.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
. "$HERE/upstream.lock"
ROOT="${EMUCAP_M64P_ROOT:-$HERE/work/mupen64plus-bundle-src-$M64P_VERSION/test}"
CORE="$ROOT/libmupen64plus.dylib"
if [ ! -f "$CORE" ]; then
  CORE="$ROOT/libmupen64plus.so.2"
fi

[ -x "$ROOT/mupen64plus" ] || {
  echo "ERROR: Mupen64Plus frontend not found under $ROOT" >&2
  exit 1
}
[ -f "$CORE" ] || {
  echo "ERROR: Mupen64Plus core not found under $ROOT" >&2
  exit 1
}

if [ "$(uname)" = "Darwin" ]; then
  SYMBOLS="$(nm -gU "$CORE")"
else
  SYMBOLS="$(nm -D "$CORE")"
fi
for symbol in \
  CoreStartup CoreDoCommand CoreAttachPlugin \
  DebugSetCallbacks DebugSetRunState DebugGetState DebugStep \
  DebugGetCPUDataPtr DebugMemRead8 DebugMemWrite8 DebugBreakpointCommand; do
  printf '%s\n' "$SYMBOLS" | grep -q "[ _]${symbol}$" || {
    echo "ERROR: missing debugger symbol: $symbol" >&2
    exit 1
  }
done

ROM_SHA="$(shasum -a 256 "$ROOT/m64p_test_rom.v64" | awk '{print $1}')"
[ "$ROM_SHA" = "$M64P_TEST_ROM_SHA256" ] || {
  echo "ERROR: official test ROM digest mismatch: $ROM_SHA" >&2
  exit 1
}

printf 'core=%s\n' "$CORE"
printf 'frontend=%s\n' "$ROOT/mupen64plus"
printf 'test_rom_sha256=%s\n' "$ROM_SHA"
printf 'debugger_symbols=verified\n'
