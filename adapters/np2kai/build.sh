#!/usr/bin/env bash
# Build the pinned license-clean NP2kai libretro core used by the PC-98
# compatibility backend.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"

DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_NP2KAI_WORK:-$DEFAULT_WORK}"
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: NP2kai work path must not be a symlink: $WORK_INPUT" >&2
  exit 2
}
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
OWNER_FILE="$WORK/.emucap-np2kai-work"
if [ -n "${EMUCAP_NP2KAI_WORK:-}" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    echo "ERROR: EMUCAP_NP2KAI_WORK is not empty or emucap-owned: $WORK" >&2
    exit 2
  fi
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "NP2kai"
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

ARCHIVE="$WORK/np2kai-$NP2KAI_COMMIT.tar.gz"
if [ ! -f "$ARCHIVE" ]; then
  curl -fL "$NP2KAI_ARCHIVE_URL" -o "$ARCHIVE.part"
  mv "$ARCHIVE.part" "$ARCHIVE"
fi
ACTUAL_ARCHIVE_SHA256="$(sha256_path "$ARCHIVE")"
if [ "$ACTUAL_ARCHIVE_SHA256" != "$NP2KAI_ARCHIVE_SHA256" ]; then
  echo "ERROR: NP2kai source archive digest mismatch" >&2
  echo "  expected=$NP2KAI_ARCHIVE_SHA256" >&2
  echo "  actual=$ACTUAL_ARCHIVE_SHA256" >&2
  exit 1
fi

PATCHES=(
  "$HERE/patches/0001-use-redistributable-libretro-profile.patch"
  "$HERE/patches/0002-add-emucap-debug-api.patch"
)
ACTUAL_PATCHSET_SHA256="$(for source_patch in "${PATCHES[@]}"; do cat "$source_patch"; done | sha256_path /dev/stdin)"
if [ "$ACTUAL_PATCHSET_SHA256" != "$NP2KAI_PATCHSET_SHA256" ]; then
  echo "ERROR: NP2kai patch stack does not match upstream.lock" >&2
  echo "  expected=$NP2KAI_PATCHSET_SHA256" >&2
  echo "  actual=$ACTUAL_PATCHSET_SHA256" >&2
  exit 1
fi

SRC="$WORK/np2kai"
if [ -e "$SRC" ]; then
  case "$(cd "$(dirname "$SRC")" && pwd -P)/$(basename "$SRC")" in
    "$WORK"/*) find "$SRC" -depth -delete ;;
    *) echo "ERROR: refusing to replace source outside NP2kai work directory" >&2; exit 2 ;;
  esac
fi
mkdir -p "$SRC"
tar -xzf "$ARCHIVE" -C "$SRC" --strip-components=1

# The upstream makefiles use CRLF. Normalize only the pinned patch inputs so
# standard patch behavior is identical across hosts.
perl -pi -e 's/\r$//' \
  "$SRC/sdl/Makefile.common" \
  "$SRC/sdl/Makefile.libretro" \
  "$SRC/i386c/ia32/cpu.c" \
  "$SRC/i386c/cpumem.c" \
  "$SRC/pccore.c" \
  "$SRC/sdl/libretro/libretro.c" \
  "$SRC/sdl/libretro/libretro_core_options.h"
for source_patch in "${PATCHES[@]}"; do
  patch -d "$SRC" -p1 <"$source_patch"
done

case "$(uname -s)" in
  Darwin) PLATFORM=osx; CORE="$SRC/sdl/np2kai_libretro.dylib" ;;
  Linux) PLATFORM=unix; CORE="$SRC/sdl/np2kai_libretro.so" ;;
  *) echo "ERROR: the supported NP2kai build currently targets macOS and Linux" >&2; exit 2 ;;
esac

MAKE_ENV=(
  "platform=$PLATFORM"
  "NP2KAI_VERSION=emucap-pinned"
  "NP2KAI_HASH=${NP2KAI_COMMIT:0:12}"
  "SUPPORT_NET=0"
  "SUPPORT_ASYNC_CPU=1"
)
DEFINE_MANIFEST="$WORK/compiled-defines.txt"
SOURCES="$WORK/compiled-sources.txt"
LICENSE_MANIFEST="$WORK/license-notices.txt"
make -s -C "$SRC/sdl" -f Makefile.libretro "${MAKE_ENV[@]}" \
  "EMUCAP_DEFINE_MANIFEST=$DEFINE_MANIFEST" \
  "EMUCAP_SOURCE_MANIFEST=$SOURCES" \
  emucap-write-profile
DEFINES="$(<"$DEFINE_MANIFEST")"

require_define() {
  printf '%s\n' "$DEFINES" | tr ' ' '\n' | grep -Fx -- "$1" >/dev/null || {
    echo "ERROR: required NP2kai define is absent: $1" >&2
    exit 1
  }
}
reject_define() {
  if printf '%s\n' "$DEFINES" | tr ' ' '\n' | grep -Fx -- "$1" >/dev/null; then
    echo "ERROR: restricted NP2kai define is active: $1" >&2
    exit 1
  fi
}
require_define -DUSE_MAME_BSD
require_define -DSUPPORT_FPU_SOFTFLOAT3
require_define -DSUPPORT_EMUCAP_DEBUG
for forbidden_define in \
  -DSUPPORT_FMGEN \
  -DSUPPORT_FPU_DOSBOX \
  -DSUPPORT_FPU_DOSBOX2 \
  -DSUPPORT_FPU_DOSBOX2_COMPATIBLE \
  -DSUPPORT_FPU_SOFTFLOAT \
  -DSUPPORT_TRIDENT_TGUI; do
  reject_define "$forbidden_define"
done

if rg -n '/sound/fmgen/|/sound/mame/|/fpu/fpemul_dosbox|/fpu/fpemul_softfloat\.c$|/fpu/softfloat/|/wab/tgui9680\.c$' "$SOURCES"; then
  echo "ERROR: restricted source entered the NP2kai compile manifest" >&2
  exit 1
fi
rg -q '/sound/mamebsd/' "$SOURCES" || {
  echo "ERROR: BSD MAME sound implementation is absent from the compile manifest" >&2
  exit 1
}
rg -q '/fpu/softfloat3/' "$SOURCES" || {
  echo "ERROR: SoftFloat 3 is absent from the compile manifest" >&2
  exit 1
}

# Bind the complete notice inventory from the pinned source tree.  This is an
# audit record, not a claim that every listed component entered the binary;
# the compiled-source manifest establishes that separate fact.
(
  cd "$SRC"
  {
    [ ! -f LICENSE ] || printf '%s\n' LICENSE
    find LICENSES -type f -print
  } | LC_ALL=C sort | while IFS= read -r notice; do
    printf '%s  %s\n' "$(sha256_path "$notice")" "$notice"
  done
) >"$LICENSE_MANIFEST"
[ -s "$LICENSE_MANIFEST" ] || {
  echo "ERROR: NP2kai license notice inventory is empty" >&2
  exit 1
}

JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
make -s -C "$SRC/sdl" -f Makefile.libretro "${MAKE_ENV[@]}" -j "$JOBS"
[ -f "$CORE" ] || {
  echo "ERROR: NP2kai libretro core was not produced: $CORE" >&2
  exit 1
}
DEBUG_SYMBOLS="$(nm -g "$CORE")"
for required_symbol in \
  emucap_np2_debug_api_version \
  emucap_np2_read_memory \
  emucap_np2_write_memory \
  emucap_np2_get_regs \
  emucap_np2_step_instruction \
  emucap_np2_disassemble \
  emucap_np2_set_breakpoint \
  emucap_np2_clear_breakpoint \
  emucap_np2_clear_all_breakpoints \
  emucap_np2_poll_event \
  emucap_np2_take_dropped_events \
  emucap_np2_set_trace \
  emucap_np2_poll_trace \
  emucap_np2_take_dropped_trace \
  emucap_np2_stop_requested \
  emucap_np2_clear_stop \
  emucap_np2_change_hdd \
  emucap_np2_current_hdd; do
  if ! grep -Eq "[_[:space:]]${required_symbol}$" <<<"$DEBUG_SYMBOLS"; then
    echo "ERROR: required NP2kai debug ABI symbol is absent: $required_symbol" >&2
    exit 1
  fi
done

CORE_SHA256="$(sha256_path "$CORE")"
DEFINE_MANIFEST_SHA256="$(sha256_path "$DEFINE_MANIFEST")"
SOURCE_MANIFEST_SHA256="$(sha256_path "$SOURCES")"
LICENSE_MANIFEST_SHA256="$(sha256_path "$LICENSE_MANIFEST")"
SIDECAR="$SRC/emucap-np2kai-build.json"
{
  printf '{\n'
  printf '  "upstream": "%s",\n' "$NP2KAI_UPSTREAM"
  printf '  "commit": "%s",\n' "$NP2KAI_COMMIT"
  printf '  "archive_sha256": "%s",\n' "$NP2KAI_ARCHIVE_SHA256"
  printf '  "patchset_sha256": "%s",\n' "$NP2KAI_PATCHSET_SHA256"
  printf '  "build_profile": "%s",\n' "$NP2KAI_BUILD_PROFILE"
  printf '  "compiled_defines_sha256": "%s",\n' "$DEFINE_MANIFEST_SHA256"
  printf '  "compiled_sources_sha256": "%s",\n' "$SOURCE_MANIFEST_SHA256"
  printf '  "license_manifest_sha256": "%s",\n' "$LICENSE_MANIFEST_SHA256"
  printf '  "core_sha256": "%s",\n' "$CORE_SHA256"
  printf '  "required_defines": ["USE_MAME_BSD", "SUPPORT_FPU_SOFTFLOAT3", "SUPPORT_EMUCAP_DEBUG"],\n'
  printf '  "excluded_components": ["fmgen", "mame-gpl-sound", "dosbox-fpu", "softfloat-legacy", "trident-tgui"]\n'
  printf '}\n'
} >"$SIDECAR"

echo "built NP2kai core: $CORE"
echo "build identity: $SIDECAR"
