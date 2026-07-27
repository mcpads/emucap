#!/usr/bin/env bash
# Build the pinned stock openMSX used by the separate XML-control bridge.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"

DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_OPENMSX_WORK:-$DEFAULT_WORK}"
CUSTOM_WORK=0
if [ -n "${EMUCAP_OPENMSX_WORK:-}" ]; then
  CUSTOM_WORK=1
fi
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: openMSX work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
}
WORK_CREATED=0
if [ ! -d "$WORK_INPUT" ]; then
  WORK_CREATED=1
fi
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
OWNER_FILE="$WORK/.emucap-openmsx-work"
work_has_entries() {
  [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]
}
if [ "$CUSTOM_WORK" = "1" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ "$WORK_CREATED" != "1" ] && work_has_entries; then
    echo "ERROR: EMUCAP_OPENMSX_WORK is not empty or emucap-owned: $WORK" >&2
    echo "       Use an empty build directory or one previously created by this script." >&2
    exit 2
  fi
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "openMSX"
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

download_checked() {
  local url="$1"
  local expected="$2"
  local output="$3"
  if [ ! -f "$output" ]; then
    curl -fL "$url" -o "$output.part"
    mv "$output.part" "$output"
  fi
  local actual
  actual="$(sha256_path "$output")"
  if [ "$actual" != "$expected" ]; then
    echo "ERROR: digest mismatch for $output" >&2
    echo "  expected=$expected" >&2
    echo "  actual=$actual" >&2
    exit 1
  fi
}

for tool in curl patch perl python3 make; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "ERROR: required build tool is missing: $tool" >&2
    exit 1
  }
done

ARCHIVE="$WORK/openmsx-$OPENMSX_VERSION.tar.gz"
SDL_PATCH="$WORK/openmsx-sdl2-compat.patch"
download_checked "$OPENMSX_URL" "$OPENMSX_SHA256" "$ARCHIVE"
download_checked \
  "$OPENMSX_SDL2_COMPAT_PATCH_URL" \
  "$OPENMSX_SDL2_COMPAT_PATCH_SHA256" \
  "$SDL_PATCH"

SRC="$WORK/openmsx-$OPENMSX_VERSION"
if [ ! -d "$SRC" ]; then
  tar -xzf "$ARCHIVE" -C "$WORK"
fi
[ -f "$SRC/configure" ] || {
  echo "ERROR: incomplete openMSX source tree: $SRC" >&2
  exit 1
}

if patch -d "$SRC" -p1 --forward --dry-run <"$SDL_PATCH" >/dev/null 2>&1; then
  patch -d "$SRC" -p1 --forward <"$SDL_PATCH"
elif ! patch -d "$SRC" -p1 --reverse --dry-run <"$SDL_PATCH" >/dev/null 2>&1; then
  echo "ERROR: SDL2 compatibility patch is neither applicable nor already applied" >&2
  exit 1
fi

INSTALL_BASE="$SRC/install"
perl -0pi -e "s{^INSTALL_BASE\\s*[:?+]?=.*\$}{INSTALL_BASE:=$INSTALL_BASE}m" \
  "$SRC/build/custom.mk"
grep -qF "INSTALL_BASE:=$INSTALL_BASE" "$SRC/build/custom.mk" || {
  echo "ERROR: failed to isolate the openMSX install prefix" >&2
  exit 1
}

if [ "$(uname)" = "Darwin" ]; then
  command -v brew >/dev/null 2>&1 || {
    echo "ERROR: the macOS build requires Homebrew dependencies" >&2
    exit 1
  }
  for formula in freetype glew libogg libpng libvorbis sdl2-compat sdl2_ttf tcl-tk theora; do
    brew --prefix "$formula" >/dev/null 2>&1 || {
      echo "ERROR: install the Homebrew dependency before building: $formula" >&2
      exit 1
    }
  done
  export TCL_CONFIG="$(brew --prefix tcl-tk)/lib"
fi

JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
(
  cd "$SRC"
  ./configure
  make -j "$JOBS"
)

if [ "$(uname)" = "Darwin" ]; then
  BINARY="$(find "$SRC/derived" -type f -path '*/openMSX.app/Contents/MacOS/openmsx' -print -quit)"
else
  BINARY="$(find "$SRC/derived" -type f -name openmsx -perm -u+x -print -quit)"
fi
[ -n "$BINARY" ] && [ -x "$BINARY" ] || {
  echo "ERROR: openMSX executable was not produced" >&2
  exit 1
}
"$BINARY" -testconfig

BUILD_DIR="$(dirname "$BINARY")"
SIDECAR="$BUILD_DIR/emucap-openmsx-build.json"
{
  printf '{\n'
  printf '  "upstream": "%s",\n' "$OPENMSX_URL"
  printf '  "version": "%s",\n' "$OPENMSX_VERSION"
  printf '  "host_api": %s,\n' "$OPENMSX_HOST_API"
  printf '  "archive_sha256": "%s",\n' "$OPENMSX_SHA256"
  printf '  "sdl2_compat_patch_sha256": "%s",\n' "$OPENMSX_SDL2_COMPAT_PATCH_SHA256"
  printf '  "native_patch": false\n'
  printf '}\n'
} >"$SIDECAR"

echo "built: $BINARY"
echo "metadata: $SIDECAR"
