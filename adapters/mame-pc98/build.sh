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
VER="${MAME_VER:-0.288}"
TAG="${MAME_TAG:-mame0288}"
URL="${MAME_URL:-https://github.com/mamedev/mame/archive/refs/tags/${TAG}.tar.gz}"
SHA256="${MAME_SHA256:-244d916eb3fb8bcd71f2ac51ae71ab6af8cf99869ea7b85d7efc7339ea56c563}"
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
OWNER_FILE="$WORK/.emucap-mame-pc98-work"
SRC="$WORK/mame-src"
TARBALL="$WORK/${TAG}.tar.gz"
PATCH_DIR="${MAME_PATCH_DIR:-$HERE/patches}"
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
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "MAME PC-98"
: >"$OWNER_FILE"

if [ ! -f "$TARBALL" ]; then
  echo "-> Downloading MAME $VER source"
  curl -fsSL -o "$TARBALL" "$URL"
fi

if command -v shasum >/dev/null 2>&1; then
  printf '%s  %s\n' "$SHA256" "$TARBALL" | shasum -a 256 -c -
fi

echo "-> Extracting fresh source"
safe_rm_rf_under_work "$SRC"
mkdir -p "$SRC"
tar xf "$TARBALL" -C "$SRC" --strip-components=1

if [ -d "$PATCH_DIR" ]; then
  while IFS= read -r patch; do
    [ -n "$patch" ] || continue
    echo "-> Applying $(basename "$patch")"
    patch -d "$SRC" -p1 <"$patch"
  done < <(find "$PATCH_DIR" -type f -name '*.patch' | sort)
fi

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
echo "MAME PC-98 build ready: $WRAPPER (safe wrapper), $RAW_LINK (raw binary), source=$SRC"
