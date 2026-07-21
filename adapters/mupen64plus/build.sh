#!/usr/bin/env bash
# Build the pinned debugger-enabled Mupen64Plus bundle used by the N64 adapter.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"

DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_M64P_WORK:-$DEFAULT_WORK}"
CUSTOM_WORK=0
if [ -n "${EMUCAP_M64P_WORK:-}" ]; then
  CUSTOM_WORK=1
fi
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: Mupen64Plus work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
}
WORK_CREATED=0
if [ ! -d "$WORK_INPUT" ]; then
  WORK_CREATED=1
fi
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
OWNER_FILE="$WORK/.emucap-mupen64plus-work"
work_has_entries() {
  [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]
}
if [ "$CUSTOM_WORK" = "1" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ "$WORK_CREATED" != "1" ] && work_has_entries; then
    echo "ERROR: EMUCAP_M64P_WORK is not empty or emucap-owned: $WORK" >&2
    echo "       Use an empty build directory or one previously created by this script." >&2
    exit 2
  fi
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "Mupen64Plus"
: >"$OWNER_FILE"

sha256_path() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "ERROR: shasum or sha256sum is required" >&2
    return 1
  fi
}

ARCHIVE="$WORK/mupen64plus-bundle-src-$M64P_VERSION.tar.gz"
if [ ! -f "$ARCHIVE" ]; then
  curl -fL "$M64P_BUNDLE_URL" -o "$ARCHIVE.part"
  mv "$ARCHIVE.part" "$ARCHIVE"
fi
ACTUAL_SHA="$(sha256_path "$ARCHIVE")"
if [ "$ACTUAL_SHA" != "$M64P_BUNDLE_SHA256" ]; then
  echo "ERROR: Mupen64Plus archive digest mismatch: $ACTUAL_SHA" >&2
  exit 1
fi

SRC="$WORK/mupen64plus-bundle-src-$M64P_VERSION"
if [ ! -d "$SRC" ]; then
  tar -xzf "$ARCHIVE" -C "$WORK"
fi
[ -f "$SRC/m64p_build.sh" ] || {
  echo "ERROR: incomplete Mupen64Plus source tree: $SRC" >&2
  exit 1
}

if [ "$(uname)" = "Darwin" ]; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "ERROR: debugger-enabled Mupen64Plus on macOS requires Homebrew binutils" >&2
    exit 1
  fi
  BINUTILS_PREFIX="$(brew --prefix binutils 2>/dev/null || true)"
  if [ ! -f "$BINUTILS_PREFIX/lib/libopcodes.a" ] || [ ! -f "$BINUTILS_PREFIX/include/dis-asm.h" ]; then
    echo "ERROR: install Homebrew binutils before building debugger-enabled Mupen64Plus" >&2
    exit 1
  fi
  export CPPFLAGS="-I$BINUTILS_PREFIX/include ${CPPFLAGS:-}"
  export LDFLAGS="-L$BINUTILS_PREFIX/lib ${LDFLAGS:-}"
  M64P_STRINGS="$BINUTILS_PREFIX/bin/strings"

  UI_PATCH="$WORK/mupen64plus-ui-console-macos.patch"
  if [ ! -f "$UI_PATCH" ]; then
    curl -fL "$M64P_MACOS_UI_PATCH_URL" -o "$UI_PATCH.part"
    mv "$UI_PATCH.part" "$UI_PATCH"
  fi
  PATCH_SHA="$(sha256_path "$UI_PATCH")"
  if [ "$PATCH_SHA" != "$M64P_MACOS_UI_PATCH_SHA256" ]; then
    echo "ERROR: Mupen64Plus macOS UI patch digest mismatch: $PATCH_SHA" >&2
    exit 1
  fi
  UI_SRC="$SRC/source/mupen64plus-ui-console"
  if patch -d "$UI_SRC" -p1 --forward --dry-run <"$UI_PATCH" >/dev/null 2>&1; then
    patch -d "$UI_SRC" -p1 --forward <"$UI_PATCH"
  elif ! patch -d "$UI_SRC" -p1 --reverse --dry-run <"$UI_PATCH" >/dev/null 2>&1; then
    echo "ERROR: Mupen64Plus macOS UI patch is neither applicable nor already applied" >&2
    exit 1
  fi
fi

JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
(
  cd "$SRC"
  M64P_COMPONENTS="core rom ui-console audio-sdl input-sdl rsp-hle video-rice" \
    ./m64p_build.sh \
      DEBUGGER=1 \
      NO_SPEEX=1 \
      NO_SRC=1 \
      STRINGS="${M64P_STRINGS:-strings}" \
      INSTALL_STRIP_FLAG= \
      PREFIX="$SRC/test" \
      -j "$JOBS"
)

CORE="$SRC/test/libmupen64plus.dylib"
if [ ! -f "$CORE" ]; then
  CORE="$SRC/test/libmupen64plus.so.2"
fi
[ -f "$CORE" ] || {
  echo "ERROR: debugger core library was not produced" >&2
  exit 1
}

if [ "$(uname)" = "Darwin" ]; then
  CORE_SYMBOLS="$(nm -gU "$CORE")"
else
  CORE_SYMBOLS="$(nm -D "$CORE")"
fi
printf '%s\n' "$CORE_SYMBOLS" | grep -q '[ _]DebugSetCallbacks$'
printf '%s\n' "$CORE_SYMBOLS" | grep -q '[ _]DebugStep$'

ROM="$SRC/test/m64p_test_rom.v64"
ROM_SHA="$(sha256_path "$ROM")"
[ "$ROM_SHA" = "$M64P_TEST_ROM_SHA256" ] || {
  echo "ERROR: official N64 test ROM digest mismatch: $ROM_SHA" >&2
  exit 1
}

{
  printf '{\n'
  printf '  "upstream": "%s",\n' "$M64P_BUNDLE_URL"
  printf '  "version": "%s",\n' "$M64P_VERSION"
  printf '  "core_commit": "%s",\n' "$M64P_CORE_COMMIT"
  printf '  "host_api": %s,\n' "$M64P_HOST_API"
  printf '  "bundle_sha256": "%s",\n' "$M64P_BUNDLE_SHA256"
  printf '  "test_rom_sha256": "%s",\n' "$ROM_SHA"
  printf '  "debugger": true\n'
  printf '}\n'
} >"$SRC/test/emucap-mupen64plus-build.json"

echo "built: $SRC/test/mupen64plus"
echo "core: $CORE"
echo "test ROM: $ROM"
