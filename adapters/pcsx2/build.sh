#!/usr/bin/env bash
# Build the pinned PCSX2 fork used by the PlayStation 2 PINE adapter.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"

PATCHSET_SHA256="$(
  cd "$HERE"
  find patches -type f -name '*.patch' -print0 |
    sort -z |
    xargs -0 shasum -a 256 |
    shasum -a 256 |
    awk '{print $1}'
)"
if [ "$PCSX2_PATCHSET_SHA256" != "pending" ] &&
   [ "$PATCHSET_SHA256" != "$PCSX2_PATCHSET_SHA256" ]; then
  echo "ERROR: patchset digest differs from upstream.lock" >&2
  exit 1
fi

WORK_INPUT="${EMUCAP_PCSX2_WORK:-$HERE/work}"
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: PCSX2 work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
}
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "PCSX2"

SRC="$WORK/pcsx2"
ORIGIN="${EMUCAP_PCSX2_SRC:-$PCSX2_REPO}"
if [ ! -d "$SRC/.git" ]; then
  git clone --filter=blob:none "$ORIGIN" "$SRC"
fi
git -C "$SRC" fetch --depth 1 origin "$PCSX2_COMMIT"
git -C "$SRC" checkout --detach "$PCSX2_COMMIT"
git -C "$SRC" checkout -- \
  pcsx2/DebugTools/Breakpoints.cpp \
  pcsx2/DebugTools/Breakpoints.h \
  pcsx2/Interpreter.cpp \
  pcsx2/PINE.cpp \
  pcsx2/Pcsx2Config.cpp \
  pcsx2/SIO/Pad/Pad.cpp \
  pcsx2/SIO/Pad/Pad.h \
  pcsx2/VMManager.cpp \
  pcsx2/VMManager.h \
  pcsx2/x86/ix86-32/iR5900.cpp
for patch in "$HERE"/patches/*.patch; do
  echo "applying $(basename "$patch")"
  git -C "$SRC" apply --check "$patch"
  git -C "$SRC" apply "$patch"
done
git -C "$SRC" diff --check

PATCHES_SRC="$WORK/pcsx2-patches"
if [ ! -d "$PATCHES_SRC/.git" ]; then
  git clone --filter=blob:none --no-checkout "$PCSX2_PATCHES_REPO" "$PATCHES_SRC"
fi
git -C "$PATCHES_SRC" fetch --depth 1 origin "$PCSX2_PATCHES_COMMIT"
git -C "$PATCHES_SRC" checkout --detach "$PCSX2_PATCHES_COMMIT"
PATCHES_TREE="$(git -C "$PATCHES_SRC" rev-parse "$PCSX2_PATCHES_COMMIT:patches")"
[ "$PATCHES_TREE" = "$PCSX2_PATCHES_TREE" ] || {
  echo "ERROR: PCSX2 patches tree differs from upstream.lock" >&2
  exit 1
}
PATCHES_ARCHIVE="$SRC/bin/resources/patches.zip"
PATCHES_MTIME="$(git -C "$PATCHES_SRC" show -s --format=%cI "$PCSX2_PATCHES_COMMIT")"
git -C "$PATCHES_SRC" archive \
  --format=zip \
  --mtime="$PATCHES_MTIME" \
  "$PCSX2_PATCHES_COMMIT:patches" \
  >"$PATCHES_ARCHIVE.tmp"
PATCHES_ARCHIVE_SHA256="$(shasum -a 256 "$PATCHES_ARCHIVE.tmp" | awk '{print $1}')"
[ "$PATCHES_ARCHIVE_SHA256" = "$PCSX2_PATCHES_ARCHIVE_SHA256" ] || {
  echo "ERROR: generated PCSX2 patches archive differs from upstream.lock" >&2
  exit 1
}
mv "$PATCHES_ARCHIVE.tmp" "$PATCHES_ARCHIVE"

JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
BUILD="$SRC/build-emucap"
COMMON_ARGS=(
  -G Ninja
  -DCMAKE_BUILD_TYPE=Release
  -DENABLE_TESTS=OFF
  -DCMAKE_DISABLE_PRECOMPILE_HEADERS=ON
)

if [ "$(uname)" = "Darwin" ]; then
  if ! xcrun --find metal >/dev/null 2>&1; then
    echo "ERROR: Xcode's Metal Toolchain is required; run: xcodebuild -downloadComponent MetalToolchain" >&2
    exit 1
  fi
  # PCSX2 recommends an x86_64 build on Apple Silicon. Do not let shell-wide Homebrew flags
  # redirect the official dependency build to incompatible arm64 headers or libraries.
  unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS PKG_CONFIG_PATH PKG_CONFIG_LIBDIR
  export CC=/usr/bin/clang
  export CXX=/usr/bin/clang++
  DEPS="${EMUCAP_PCSX2_DEPS:-$WORK/deps-macos-x64}"
  if [ ! -f "$DEPS/lib/cmake/Qt6/Qt6Config.cmake" ]; then
    echo "building PCSX2's pinned x86_64 macOS dependencies"
    BUILD_FFMPEG="${EMUCAP_PCSX2_BUILD_FFMPEG:-1}" \
      "$SRC/.github/workflows/scripts/macos/build-dependencies.sh" "$DEPS"
  fi
  LINK_FFMPEG=OFF
  if [ -e "$DEPS/lib/libavcodec.dylib" ] &&
     lipo -archs "$DEPS/lib/libavcodec.dylib" 2>/dev/null | grep -qw x86_64; then
    LINK_FFMPEG=ON
  else
    COMMON_ARGS+=(-DCMAKE_DISABLE_FIND_PACKAGE_FFMPEG=TRUE)
  fi
  COMMON_ARGS+=(
    -DCMAKE_PREFIX_PATH="$DEPS"
    -DCMAKE_OSX_ARCHITECTURES=x86_64
    -DCMAKE_APPLE_SILICON_PROCESSOR=x86_64
    -DDISABLE_ADVANCE_SIMD=ON
    -DUSE_LINKED_FFMPEG="$LINK_FFMPEG"
  )
fi

cmake -S "$SRC" -B "$BUILD" "${COMMON_ARGS[@]}"
cmake --build "$BUILD" --target pcsx2-qt -j "$JOBS"
cargo build --manifest-path "$ROOT/Cargo.toml" --release --bin emucap-pcsx2-bridge

PCSX2_BIN=
for candidate in \
  "$BUILD/pcsx2-qt/PCSX2.app/Contents/MacOS/PCSX2" \
  "$BUILD/pcsx2-qt/pcsx2-qt.app/Contents/MacOS/pcsx2-qt" \
  "$BUILD/pcsx2-qt/PCSX2-Qt.app/Contents/MacOS/PCSX2-Qt" \
  "$BUILD/pcsx2-qt/pcsx2-qt" \
  "$BUILD/bin/pcsx2-qt" \
  "$BUILD/bin/pcsx2-qt.exe"; do
  if [ -f "$candidate" ]; then
    PCSX2_BIN="$candidate"
    break
  fi
done
[ -n "$PCSX2_BIN" ] || {
  echo "ERROR: PCSX2 executable was not found below $BUILD" >&2
  exit 1
}

if [[ "$PCSX2_BIN" == *.app/Contents/MacOS/* ]]; then
  PACKAGED_PATCHES="$(dirname "$(dirname "$PCSX2_BIN")")/Resources/patches.zip"
else
  PACKAGED_PATCHES="$(dirname "$PCSX2_BIN")/resources/patches.zip"
fi
[ -s "$PACKAGED_PATCHES" ] || {
  echo "ERROR: built-in game patches were not packaged at $PACKAGED_PATCHES" >&2
  exit 1
}
PACKAGED_PATCHES_SHA256="$(shasum -a 256 "$PACKAGED_PATCHES" | awk '{print $1}')"
[ "$PACKAGED_PATCHES_SHA256" = "$PCSX2_PATCHES_ARCHIVE_SHA256" ] || {
  echo "ERROR: packaged built-in game patches differ from upstream.lock" >&2
  exit 1
}

METADATA="$(dirname "$PCSX2_BIN")/emucap-pcsx2-build.json"
{
  printf '{\n'
  printf '  "upstream": "%s",\n' "$PCSX2_REPO"
  printf '  "commit": "%s",\n' "$PCSX2_COMMIT"
  printf '  "patches_upstream": "%s",\n' "$PCSX2_PATCHES_REPO"
  printf '  "patches_commit": "%s",\n' "$PCSX2_PATCHES_COMMIT"
  printf '  "patches_tree": "%s",\n' "$PCSX2_PATCHES_TREE"
  printf '  "patches_archive_sha256": "%s",\n' "$PCSX2_PATCHES_ARCHIVE_SHA256"
  printf '  "host_api": %s,\n' "$PCSX2_HOST_API"
  printf '  "patchset_sha256": "%s"\n' "$PATCHSET_SHA256"
  printf '}\n'
} >"$METADATA"

if [ "$(uname)" = "Darwin" ] && [[ "$PCSX2_BIN" == *.app/Contents/MacOS/* ]]; then
  PCSX2_APP="$(dirname "$(dirname "$(dirname "$PCSX2_BIN")")")"
  codesign \
    --force \
    --deep \
    --sign - \
    --entitlements "$SRC/pcsx2/Resources/PCSX2.entitlements" \
    "$PCSX2_APP"
  codesign --verify --deep --strict "$PCSX2_APP"
fi

echo "built: $PCSX2_BIN"
echo "bridge: $ROOT/target/release/emucap-pcsx2-bridge"
echo "set EMUCAP_PCSX2_BIOS to an absolute path before launch"
