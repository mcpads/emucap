#!/usr/bin/env bash
# Build the pinned emucap-compatible xemu host.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/../_common/build-env.sh"
. "$HERE/upstream.lock"

DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_XEMU_WORK:-$DEFAULT_WORK}"
[ ! -L "$WORK_INPUT" ] || {
  echo "ERROR: xemu work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
}
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
MARKER="$WORK/.emucap-xemu-work"
if [ ! -f "$MARKER" ] && [ "$WORK_INPUT" != "$DEFAULT_WORK" ] &&
  [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "ERROR: EMUCAP_XEMU_WORK is not empty or emucap-owned: $WORK" >&2
  exit 1
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "xemu"
: >"$MARKER"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print tolower($1)}'
  else
    sha256sum "$1" | awk '{print tolower($1)}'
  fi
}

SRC="$WORK/xemu"
[ ! -L "$SRC" ] || {
  echo "ERROR: xemu source path must not be a symlink: $SRC" >&2
  exit 1
}
ORIGIN="${EMUCAP_XEMU_SRC:-$XEMU_REPO}"
if [ -n "${EMUCAP_XEMU_SRC:-}" ] && [ ! -d "$EMUCAP_XEMU_SRC/.git" ]; then
  echo "ERROR: EMUCAP_XEMU_SRC is not a Git checkout: $EMUCAP_XEMU_SRC" >&2
  exit 1
fi
if [ ! -d "$SRC/.git" ]; then
  if [ -n "$(find "$SRC" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)" ]; then
    echo "ERROR: xemu source exists but is not a Git checkout: $SRC" >&2
    exit 1
  fi
  mkdir -p "$SRC"
  git init -q "$SRC"
  git -C "$SRC" config --local core.fsmonitor false
  git -C "$SRC" remote add origin "$ORIGIN"
else
  git -C "$SRC" config --local core.fsmonitor false
  git -C "$SRC" remote set-url origin "$ORIGIN"
fi

echo "fetching xemu $XEMU_TAG ($XEMU_COMMIT)"
git -C "$SRC" fetch -q --depth 1 origin "$XEMU_COMMIT"
git -C "$SRC" checkout -q --detach "$XEMU_COMMIT"
[ "$(git -C "$SRC" rev-parse HEAD)" = "$XEMU_COMMIT" ] || {
  echo "ERROR: xemu revision does not match upstream.lock" >&2
  exit 1
}
git -C "$SRC" reset -q --hard "$XEMU_COMMIT"
git -C "$SRC" clean -fdq
# The user's global Git configuration may enable the built-in fsmonitor. Propagate an explicit
# override while recursive submodules are materialized so generated repositories do not leave one
# detached daemon per submodule holding this adapter tree open after the build.
git -c core.fsmonitor=false -C "$SRC" submodule update --init --recursive
git -C "$SRC" submodule foreach --quiet --recursive \
  'git config --local core.fsmonitor false'

PATCHES=(
  "$HERE/patches/0001-add-emucap-control.patch"
  "$HERE/patches/0002-preserve-bql-on-watchpoint-reentry.patch"
  "$HERE/patches/0003-defer-bql-held-watchpoint-stop.patch"
  "$HERE/patches/0004-retranslate-watchpoint-without-current-tb-invalidation.patch"
)
ACTUAL_PATCHSET_SHA256="$(for source_patch in "${PATCHES[@]}"; do cat "$source_patch"; done |
  if command -v shasum >/dev/null 2>&1; then shasum -a 256; else sha256sum; fi |
  awk '{print tolower($1)}')"
[ "$ACTUAL_PATCHSET_SHA256" = "$XEMU_PATCHSET_SHA256" ] || {
  echo "ERROR: xemu patch stack does not match upstream.lock" >&2
  echo "  expected=$XEMU_PATCHSET_SHA256" >&2
  echo "  actual=$ACTUAL_PATCHSET_SHA256" >&2
  exit 1
}
for source_patch in "${PATCHES[@]}"; do
  echo "applying $(basename "$source_patch")"
  git -C "$SRC" apply --check "$source_patch"
  git -C "$SRC" apply "$source_patch"
done

choose_python() {
  local candidate version major minor
  for candidate in "${EMUCAP_XEMU_PYTHON:-}" python3.13 python3.12 python3.11 python3; do
    [ -n "$candidate" ] || continue
    command -v "$candidate" >/dev/null 2>&1 || continue
    version="$("$candidate" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
    major="${version%%.*}"
    minor="${version#*.}"
    if [ "$major" -eq 3 ] && [ "$minor" -ge 8 ] && [ "$minor" -lt 14 ]; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

PYTHON="$(choose_python || true)"
[ -n "$PYTHON" ] || {
  echo "ERROR: xemu's dependency extractor needs Python 3.8 through 3.13" >&2
  echo "       Set EMUCAP_XEMU_PYTHON to a compatible python3 executable." >&2
  exit 1
}
if [ "$(uname -s)" = "Darwin" ] && ! command -v dylibbundler >/dev/null 2>&1; then
  echo "ERROR: xemu macOS packaging requires dylibbundler" >&2
  exit 1
fi

emucap_scrub_build_env
export PATH="$(dirname "$PYTHON"):$PATH"
JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
(
  cd "$SRC"
  ./build.sh -j"$JOBS"
)

if [ "$(uname -s)" = "Darwin" ]; then
  BIN="$SRC/dist/xemu.app/Contents/MacOS/xemu"
  [ -x "$BIN" ] || { echo "ERROR: xemu app was not produced" >&2; exit 1; }
  RPATH="@executable_path/../Libraries/$(uname -m)/"
  RPATH_COUNT="$(otool -l "$BIN" | awk '/cmd LC_RPATH/{getline; getline; print $2}' |
    awk -v wanted="$RPATH" '$0 == wanted {count++} END {print count + 0}')"
  while [ "$RPATH_COUNT" -gt 1 ]; do
    install_name_tool -delete_rpath "$RPATH" "$BIN"
    RPATH_COUNT=$((RPATH_COUNT - 1))
  done
  [ "$RPATH_COUNT" -eq 1 ] || {
    echo "ERROR: packaged xemu does not have exactly one managed library rpath" >&2
    exit 1
  }
  codesign --force --deep --preserve-metadata=entitlements,requirements,flags,runtime \
    --sign - "$BIN"
  codesign --verify --deep --strict "$SRC/dist/xemu.app"
else
  BIN="$SRC/dist/xemu"
  [ -x "$BIN" ] || { echo "ERROR: xemu executable was not produced" >&2; exit 1; }
fi

BINARY_SHA256="$(sha256_file "$BIN")"
METADATA="$SRC/dist/emucap-xemu-build.json"
printf '{\n  "upstream": "%s",\n  "tag": "%s",\n  "commit": "%s",\n  "host_api": %s,\n  "patchset_sha256": "%s",\n  "binary_sha256": "%s"\n}\n' \
  "$XEMU_REPO" "$XEMU_TAG" "$XEMU_COMMIT" "$XEMU_HOST_API" \
  "$XEMU_PATCHSET_SHA256" "$BINARY_SHA256" >"$METADATA"

echo "built: $BIN"
echo "metadata: $METADATA"
