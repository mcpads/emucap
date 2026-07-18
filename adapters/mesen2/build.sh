#!/usr/bin/env bash
# Build the emucap-compatible MesenCE host from a pinned upstream revision.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
LOCK_FILE="$HERE/upstream.lock"
DEFAULT_WORK="$HERE/work"
WORK_INPUT="${EMUCAP_MESEN_WORK:-$DEFAULT_WORK}"

lock_value() {
  sed -n "s/^$1=//p" "$LOCK_FILE"
}

MESEN_REPO="$(lock_value MESEN_REPO)"
MESEN_TAG="$(lock_value MESEN_TAG)"
MESEN_COMMIT="$(lock_value MESEN_COMMIT)"
MESEN_HOST_API="$(lock_value MESEN_HOST_API)"
MESEN_PATCHSET_SHA256="$(lock_value MESEN_PATCHSET_SHA256)"

[ -n "$MESEN_REPO" ] && [ -n "$MESEN_COMMIT" ] && [ -n "$MESEN_HOST_API" ] &&
  printf '%s' "$MESEN_PATCHSET_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "ERROR: invalid Mesen upstream lock: $LOCK_FILE" >&2
  exit 1
}

if [ -L "$WORK_INPUT" ]; then
  echo "ERROR: Mesen work path must not be a symlink: $WORK_INPUT" >&2
  exit 1
fi
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
MARKER="$WORK/.emucap-mesen-work"
if [ ! -f "$MARKER" ]; then
  if [ "$WORK_INPUT" != "$DEFAULT_WORK" ] && [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    echo "ERROR: EMUCAP_MESEN_WORK is not empty or emucap-owned: $WORK" >&2
    exit 1
  fi
fi

emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "Mesen"
if [ ! -f "$MARKER" ]; then
  : >"$MARKER"
fi

SRC="$WORK/mesen"
[ ! -L "$SRC" ] || {
  echo "ERROR: Mesen work source must not be a symlink: $SRC" >&2
  exit 1
}
ORIGIN="${EMUCAP_MESEN_SRC:-$MESEN_REPO}"
if [ -n "${EMUCAP_MESEN_SRC:-}" ] && [ ! -d "$EMUCAP_MESEN_SRC/.git" ]; then
  echo "ERROR: EMUCAP_MESEN_SRC is not a git checkout: $EMUCAP_MESEN_SRC" >&2
  exit 1
fi
if [ ! -d "$SRC/.git" ]; then
  if [ -n "$(find "$SRC" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)" ]; then
    echo "ERROR: Mesen work source exists but is not a git checkout: $SRC" >&2
    exit 1
  fi
  mkdir -p "$SRC"
  git init -q "$SRC"
  git -C "$SRC" remote add origin "$ORIGIN"
else
  git -C "$SRC" remote set-url origin "$ORIGIN"
fi

echo "→ fetching MesenCE $MESEN_TAG ($MESEN_COMMIT)"
git -C "$SRC" fetch -q --depth 1 origin "$MESEN_COMMIT"
git -C "$SRC" checkout -q --detach "$MESEN_COMMIT"
got="$(git -C "$SRC" rev-parse HEAD)"
[ "$got" = "$MESEN_COMMIT" ] || {
  echo "ERROR: Mesen revision mismatch: got $got expected $MESEN_COMMIT" >&2
  exit 1
}

# The work tree is generated and marker-owned. Restore the pinned baseline and remove only untracked
# source additions before applying the complete ordered patch stack. Ignored compiler outputs remain.
git -C "$SRC" reset -q --hard "$MESEN_COMMIT"
git -C "$SRC" clean -fdq
if [ -f "$SRC/.gitmodules" ]; then
  git -C "$SRC" submodule update --init --recursive
fi

PATCHES=(
  "$HERE/patches/0001-fix-numeric-cli-settings.patch"
  "$HERE/patches/0002-add-code-break-idle-event.patch"
)
if command -v shasum >/dev/null 2>&1; then
  ACTUAL_PATCHSET_SHA256="$(for patch in "${PATCHES[@]}"; do cat "$patch"; done | shasum -a 256 | awk '{print $1}')"
else
  ACTUAL_PATCHSET_SHA256="$(for patch in "${PATCHES[@]}"; do cat "$patch"; done | sha256sum | awk '{print $1}')"
fi
[ "$ACTUAL_PATCHSET_SHA256" = "$MESEN_PATCHSET_SHA256" ] || {
  echo "ERROR: Mesen patch stack does not match upstream.lock" >&2
  echo "  expected=$MESEN_PATCHSET_SHA256" >&2
  echo "  actual=$ACTUAL_PATCHSET_SHA256" >&2
  exit 1
}
for patch in "${PATCHES[@]}"; do
  [ -f "$patch" ] || { echo "ERROR: missing Mesen patch: $patch" >&2; exit 1; }
  echo "→ applying $(basename "$patch")"
  git -C "$SRC" apply --check "$patch" || {
    echo "ERROR: Mesen patch does not apply cleanly: $patch" >&2
    exit 1
  }
  git -C "$SRC" apply "$patch"
done

. "$HERE/../_common/build-env.sh"
emucap_scrub_build_env

# MesenCE 2.2.1 pins the .NET 8 SDK. Homebrew installs dotnet@8 keg-only on macOS,
# so select it explicitly when available instead of accidentally invoking a newer
# globally linked SDK that global.json is required to reject.
if [ "$(uname -s)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
  DOTNET8_PREFIX="$(brew --prefix dotnet@8 2>/dev/null || true)"
  if [ -x "$DOTNET8_PREFIX/bin/dotnet" ]; then
    export DOTNET_ROOT="$DOTNET8_PREFIX/libexec"
    export PATH="$DOTNET8_PREFIX/bin:$PATH"
  fi
fi
# A detached Roslyn/MSBuild server can inherit make's job pipe after `dotnet publish` exits and keep
# make waiting forever. A reproducible one-shot adapter build has no benefit from those servers.
export DOTNET_CLI_USE_MSBUILD_SERVER=0
export MSBUILDDISABLENODEREUSE=1
export UseSharedCompilation=false
export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_NOLOGO=1
JOBS="${EMUCAP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
echo "→ building MesenCE locally with make (-j$JOBS)"
# A previous build's sidecar must never be copied into a fresh app bundle before the new patch stack
# is verified. The selected artifact receives exactly one current sidecar after the build succeeds.
find "$SRC/bin" -name emucap-mesen-build.json -type f -delete 2>/dev/null || true
make -C "$SRC" -j"$JOBS"

BIN=""
if [ "$(uname -s)" = "Darwin" ]; then
  BIN="$(find "$SRC/bin" -path '*/publish/Mesen.app/Contents/MacOS/Mesen' -type f -perm -111 -print 2>/dev/null | head -1)"
fi
if [ -z "$BIN" ]; then
  BIN="$(find "$SRC/bin" -path '*/publish/Mesen' -type f -perm -111 -print 2>/dev/null | head -1)"
fi
if [ -z "$BIN" ]; then
  BIN="$(find "$SRC/bin" -type f \( -name Mesen -o -name Mesen.exe \) -perm -111 -print 2>/dev/null | head -1)"
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || {
  echo "ERROR: Mesen build completed without a runnable UI binary under $SRC/bin" >&2
  exit 1
}

METADATA="$(dirname "$BIN")/emucap-mesen-build.json"
printf '{\n  "upstream": "%s",\n  "tag": "%s",\n  "commit": "%s",\n  "host_api": %s,\n  "patchset_sha256": "%s"\n}\n' \
  "$MESEN_REPO" "$MESEN_TAG" "$MESEN_COMMIT" "$MESEN_HOST_API" "$MESEN_PATCHSET_SHA256" >"$METADATA"

echo "OK: $BIN"
echo "metadata: $METADATA"
