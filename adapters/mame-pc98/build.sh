#!/usr/bin/env bash
# Reproducible MAME PC-98 adapter build entrypoint.
#
# A system MAME binary is only a bootstrap smoke fallback.  PC-98 parity work
# needs emulator-thread hooks that Lua/GDB cannot provide, so this script
# fetches a pinned MAME source release, applies repo-local patches, and exposes
# a safe headless wrapper at adapters/mame-pc98/work/mame for launch.sh to
# prefer.  The raw binary is linked as work/mame.raw for explicit diagnostics.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/upstream.lock"
VER="${MAME_VER:-$MAME_LOCK_VERSION}"
TAG="${MAME_TAG:-$MAME_LOCK_TAG}"
if [ -n "${MAME_URL:-}" ] || [ -n "${MAME_TAG:-}" ]; then
  URL="${MAME_URL:-https://github.com/mamedev/mame/archive/refs/tags/${TAG}.tar.gz}"
else
  URL="$MAME_LOCK_URL"
fi
SHA256="${MAME_SHA256:-$MAME_LOCK_SHA256}"
DEFAULT_WORK="$HERE/work"
WORK_INPUT="${MAME_WORK:-$DEFAULT_WORK}"
CUSTOM_WORK=0
if [ -n "${MAME_WORK:-}" ]; then
  CUSTOM_WORK=1
fi
WORK_CREATED=0
if [ ! -d "$WORK_INPUT" ]; then
  WORK_CREATED=1
fi
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
BUILD_LABEL="${MAME_BUILD_LABEL:-MAME PC-98}"
OWNER_FILE="$WORK/${MAME_WORK_OWNER_FILE:-.emucap-mame-pc98-work}"
SRC="$WORK/mame-src"
TARBALL="$WORK/${TAG}.tar.gz"
PATCH_DIR="${MAME_PATCH_DIR:-$HERE/patches}"
PATCHSET_SHA256="${MAME_PATCHSET_SHA256:-$MAME_LOCK_PATCHSET_SHA256}"
JOBS="${MAME_JOBS:-$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)}"
WRAPPER="$WORK/mame"
RAW_LINK="$WORK/mame.raw"

abs_child_path() {
  local path="$1"
  local parent base
  parent="$(dirname "$path")"
  base="$(basename "$path")"
  if [ ! -d "$parent" ]; then
    echo "ERROR: parent directory does not exist for $path" >&2
    exit 2
  fi
  printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$base"
}

safe_rm_rf_under_work() {
  local target="$1"
  local abs_target
  abs_target="$(abs_child_path "$target")"
  case "$abs_target" in
    "$WORK"/*) ;;
    *)
      echo "ERROR: refusing to remove path outside MAME_WORK: $target" >&2
      exit 2
      ;;
  esac
  if [ "$abs_target" = "$WORK" ] || [ "$abs_target" = "/" ] || [ -z "$abs_target" ]; then
    echo "ERROR: refusing to remove unsafe path: $target" >&2
    exit 2
  fi
  rm -rf -- "$abs_target"
}

work_has_entries() {
  [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]
}

if [ "$CUSTOM_WORK" = "1" ] && [ ! -f "$OWNER_FILE" ]; then
  if [ "$WORK_CREATED" != "1" ] && work_has_entries; then
    echo "ERROR: MAME_WORK is not empty or emucap-owned: $WORK" >&2
    echo "       Use an empty build directory or one previously created by this script." >&2
    exit 2
  fi
fi
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "$BUILD_LABEL"
: >"$OWNER_FILE"

if [ ! -f "$TARBALL" ]; then
  echo "-> Downloading MAME $VER source"
  curl -fsSL -o "$TARBALL" "$URL"
fi

if command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(sha256sum "$TARBALL" | awk '{print $1}')"
else
  echo "ERROR: shasum or sha256sum is required" >&2
  exit 1
fi
[ "$ACTUAL_SHA256" = "$SHA256" ] || {
  echo "ERROR: MAME source archive checksum mismatch" >&2
  echo "  expected=$SHA256" >&2
  echo "  actual=$ACTUAL_SHA256" >&2
  exit 1
}

PATCHES=()
if [ -d "$PATCH_DIR" ]; then
  while IFS= read -r source_patch; do
    [ -n "$source_patch" ] && PATCHES+=("$source_patch")
  done < <(find "$PATCH_DIR" -type f -name '*.patch' | sort)
fi
if command -v shasum >/dev/null 2>&1; then
  ACTUAL_PATCHSET_SHA256="$(for source_patch in "${PATCHES[@]}"; do cat "$source_patch"; done | shasum -a 256 | awk '{print $1}')"
else
  ACTUAL_PATCHSET_SHA256="$(for source_patch in "${PATCHES[@]}"; do cat "$source_patch"; done | sha256sum | awk '{print $1}')"
fi
[ "$ACTUAL_PATCHSET_SHA256" = "$PATCHSET_SHA256" ] || {
  echo "ERROR: MAME patch stack does not match upstream.lock" >&2
  echo "  expected=$PATCHSET_SHA256" >&2
  echo "  actual=$ACTUAL_PATCHSET_SHA256" >&2
  exit 1
}

echo "-> Extracting fresh source"
safe_rm_rf_under_work "$SRC"
mkdir -p "$SRC"
tar xf "$TARBALL" -C "$SRC" --strip-components=1

for source_patch in "${PATCHES[@]}"; do
  echo "-> Applying $(basename "$source_patch")"
  patch -d "$SRC" -p1 <"$source_patch"
done

echo "-> Building MAME $VER"
make_args=(NOWERROR=1)
if [ -n "${MAME_SUBTARGET:-}" ]; then
  make_args+=(SUBTARGET="$MAME_SUBTARGET")
fi
if [ -n "${MAME_SOURCES:-}" ]; then
  make_args+=(SOURCES="$MAME_SOURCES")
fi
if [ -n "${MAME_VERBOSE:-}" ]; then
  make_args+=(VERBOSE="$MAME_VERBOSE")
fi

(
  cd "$SRC"
  # MAME_EXTRA_MAKE_ARGS is intentionally split for advanced local build flags.
  # shellcheck disable=SC2086
  make -j "$JOBS" "${make_args[@]}" ${MAME_EXTRA_MAKE_ARGS:-}
)

raw_bin="$SRC/mame"
if [ ! -x "$raw_bin" ]; then
  raw_bin="$(find "$SRC" -maxdepth 2 -type f -perm -111 -name 'mame*' | sort | head -n 1 || true)"
fi
if [ -z "$raw_bin" ] || [ ! -f "$raw_bin" ] || [ ! -x "$raw_bin" ]; then
  echo "ERROR: built MAME binary not found under $SRC" >&2
  exit 1
fi

if [ -e "$RAW_LINK" ] || [ -L "$RAW_LINK" ]; then
  safe_rm_rf_under_work "$RAW_LINK"
fi
if [ -e "$WRAPPER" ] || [ -L "$WRAPPER" ]; then
  echo "-> Replacing stale wrapper path: $WRAPPER"
  safe_rm_rf_under_work "$WRAPPER"
fi
ln -s "$raw_bin" "$RAW_LINK"
cat >"$WRAPPER" <<EOF
#!/usr/bin/env bash
set -euo pipefail
DIR="\$(cd "\$(dirname "\$0")" && pwd)"
export EMUCAP_MAME_RAW_BIN="\${EMUCAP_MAME_RAW_BIN:-\$DIR/mame.raw}"
exec "$HERE/mame-headless.sh" "\$@"
EOF
chmod +x "$WRAPPER"
echo "$BUILD_LABEL build ready: $WRAPPER (safe wrapper), $RAW_LINK (raw binary), source=$SRC"
