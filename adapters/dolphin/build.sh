#!/usr/bin/env bash
# Build the pinned Dolphin fork used by the native GameCube/Wii adapter.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"

WORK_INPUT="${EMUCAP_DOLPHIN_WORK:-$HERE/work}"
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: Dolphin work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
}
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "Dolphin"

SRC="$WORK/dolphin-src"
ORIGIN="${EMUCAP_DOLPHIN_SRC:-$DOLPHIN_REPO}"
if [ ! -d "$SRC/.git" ]; then
  git clone --filter=blob:none "$ORIGIN" "$SRC"
fi
git -C "$SRC" fetch --depth 1 origin "$DOLPHIN_COMMIT"
git -C "$SRC" checkout --detach "$DOLPHIN_COMMIT"

# These top-level submodules are Windows- or Android-only. Desktop Unix builds use the remaining
# pinned submodules plus any system libraries found by CMake.
git -C "$SRC" config -f .gitmodules --get-regexp path |
  awk '{print $2}' |
  while IFS= read -r module; do
    case "$module" in
      Externals/Qt|Externals/FFmpeg-bin|Externals/wil|Externals/libadrenotools) continue ;;
    esac
    printf '%s\n' "$module"
  done |
  xargs git -C "$SRC" submodule update --init --recursive --depth 1 --jobs "${EMUCAP_BUILD_JOBS:-8}" --

# Restore only files owned by this patch stack. Build directories and downloaded submodules remain
# reusable, but no stale adapter source can survive a rebuild.
git -C "$SRC" checkout -- \
  Source/Core/Core/CMakeLists.txt \
  Source/Core/Core/Core.cpp \
  Source/Core/Core/HW/GCPad.cpp \
  Source/Core/Core/PowerPC/PowerPC.cpp \
  Source/Core/DolphinLib.props
git -C "$SRC" clean -fdq -- Source/Core/Core/EmuCap.cpp Source/Core/Core/EmuCap.h
cp "$HERE/EmuCap.cpp" "$SRC/Source/Core/Core/EmuCap.cpp"
cp "$HERE/EmuCap.h" "$SRC/Source/Core/Core/EmuCap.h"
for patch in "$HERE"/patches/*.patch; do
  echo "applying $(basename "$patch")"
  git -C "$SRC" apply --check "$patch"
  git -C "$SRC" apply "$patch"
done

if [ "$(uname)" = "Darwin" ]; then
  export CC=/usr/bin/clang
  export CXX=/usr/bin/clang++
fi
JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
COMMON_ARGS=(
  -G Ninja
  -DCMAKE_BUILD_TYPE=Release
  -DENABLE_VULKAN=OFF
  -DUSE_BUNDLED_MOLTENVK=OFF
  -DMACOS_CODE_SIGNING=OFF
  -DUSE_SYSTEM_LIBS=AUTO
)

HEADLESS_BUILD="$SRC/build-emucap-headless"
cmake -S "$SRC" -B "$HEADLESS_BUILD" "${COMMON_ARGS[@]}" -DENABLE_HEADLESS=ON -DENABLE_QT=OFF
cmake --build "$HEADLESS_BUILD" --target dolphin-nogui -j "$JOBS"

PATCHSET_SHA256="$(
  cd "$HERE"
  {
    shasum -a 256 EmuCap.cpp EmuCap.h
    find patches -type f -name '*.patch' -print0 |
      sort -z |
      xargs -0 shasum -a 256
  } | shasum -a 256 | awk '{print $1}'
)"
if [ "$DOLPHIN_PATCHSET_SHA256" != "pending" ] &&
   [ "$PATCHSET_SHA256" != "$DOLPHIN_PATCHSET_SHA256" ]; then
  echo "ERROR: patchset digest differs from upstream.lock" >&2
  exit 1
fi

write_metadata() {
  destination="$1"
  mkdir -p "$(dirname "$destination")"
  {
    printf '{\n'
    printf '  "upstream": "%s",\n' "$DOLPHIN_REPO"
    printf '  "commit": "%s",\n' "$DOLPHIN_COMMIT"
    printf '  "host_api": %s,\n' "$DOLPHIN_HOST_API"
    printf '  "patchset_sha256": "%s"\n' "$PATCHSET_SHA256"
    printf '}\n'
  } >"$destination"
}

HEADLESS_BIN="$HEADLESS_BUILD/Binaries/dolphin-emu-nogui"
write_metadata "$HEADLESS_BUILD/Binaries/emucap-dolphin-build.json"
echo "built: $HEADLESS_BIN"

# A GUI build is optional so a headless build host remains useful. On macOS, install Qt 6 and leave
# EMUCAP_DOLPHIN_BUILD_GUI at its default to produce DolphinQt.app for display=true.
if [ "${EMUCAP_DOLPHIN_BUILD_GUI:-1}" != "0" ]; then
  GUI_BUILD="$SRC/build-emucap-gui"
  if cmake -S "$SRC" -B "$GUI_BUILD" "${COMMON_ARGS[@]}" -DENABLE_HEADLESS=OFF -DENABLE_QT=ON &&
     cmake --build "$GUI_BUILD" --target dolphin-emu -j "$JOBS"; then
    if [ -d "$GUI_BUILD/Binaries/DolphinQt.app" ]; then
      write_metadata "$GUI_BUILD/Binaries/DolphinQt.app/Contents/MacOS/emucap-dolphin-build.json"
      echo "built: $GUI_BUILD/Binaries/DolphinQt.app"
    else
      write_metadata "$GUI_BUILD/Binaries/emucap-dolphin-build.json"
      echo "built: $GUI_BUILD/Binaries"
    fi
  else
    echo "WARNING: Dolphin GUI build failed; the headless build is ready." >&2
  fi
fi
