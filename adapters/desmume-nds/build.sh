#!/usr/bin/env bash
# Build a headless DeSmuME CLI (Nintendo DS) with the ARM9/ARM7 GDB stubs, for emucap.
#
# The upstream desmume-cli creates an X11/SDL OpenGL window unconditionally, so it
# cannot run without a display. This build applies a repo-owned headless patch
# (patches/0001-headless-cli.patch) that drops X11 and gates window/renderer
# creation, leaving the emulation loop + GDB stub. The emucap NDS bridge then
# drives it over GDB-RSP.
#
# EMUCAP_DESMUME_SRC is treated as read-only input; the patch and build/ happen in
# an emucap-owned work tree.
#
# Optional environment:
#   EMUCAP_DESMUME_SRC=/path/to/desmume        read-only upstream checkout to copy from
#   EMUCAP_DESMUME_WORK=/path/to/worktree       default: adapters/desmume-nds/work
#   DESMUME_JOBS=<n>                            parallel build jobs (default: CPU count)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
PATCH="$HERE/patches/0001-headless-cli.patch"
PATCH2="$HERE/patches/0002-emucap-hooks.patch"
PATCH3="$HERE/patches/0003-emucap-state-disasm.patch"
PATCH4="$HERE/patches/0004-emucap-reset.patch"
PATCH5="$HERE/patches/0005-emucap-touch.patch"
PATCH6="$HERE/patches/0006-emucap-gdb-bufmax.patch"
PATCH7="$HERE/patches/0007-emucap-input-status.patch"
PATCH8="$HERE/patches/0008-emucap-gdb-io-deadline.patch"
PATCH9="$HERE/patches/0009-emucap-gdb-no-sigpipe.patch"
WORK_INPUT="${EMUCAP_DESMUME_WORK:-$HERE/work}"
[ ! -L "$WORK_INPUT" ] || { echo "ERROR: DeSmuME work path must not be a symlink: $WORK_INPUT" >&2; exit 1; }
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
SRC="$WORK/src"
POSIX="$SRC/desmume/src/frontend/posix"
BUILD="$POSIX/build-headless"
JOBS="${DESMUME_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"

[ -f "$PATCH" ] || { echo "ERROR: headless patch not found: $PATCH" >&2; exit 1; }
[ -f "$PATCH2" ] || { echo "ERROR: emucap hooks patch not found: $PATCH2" >&2; exit 1; }
[ -f "$PATCH3" ] || { echo "ERROR: emucap state/disasm patch not found: $PATCH3" >&2; exit 1; }
[ -f "$PATCH4" ] || { echo "ERROR: emucap reset patch not found: $PATCH4" >&2; exit 1; }
[ -f "$PATCH5" ] || { echo "ERROR: emucap touch patch not found: $PATCH5" >&2; exit 1; }
[ -f "$PATCH6" ] || { echo "ERROR: emucap gdb-bufmax patch not found: $PATCH6" >&2; exit 1; }
[ -f "$PATCH7" ] || { echo "ERROR: emucap input-status patch not found: $PATCH7" >&2; exit 1; }
[ -f "$PATCH8" ] || { echo "ERROR: emucap gdb I/O deadline patch not found: $PATCH8" >&2; exit 1; }
[ -f "$PATCH9" ] || { echo "ERROR: emucap gdb SIGPIPE patch not found: $PATCH9" >&2; exit 1; }

for tool in meson ninja git; do
  command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: missing build tool: $tool (macOS: brew install $tool)" >&2; exit 1; }
done

emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "DeSmuME"

# DeSmuME upstream is pinned to a known-good revision — the patch stack (0001-0009) is written
# against exactly this tree. Cloning a moving HEAD would silently build an untested revision, and any
# upstream edit near the patch hunks would break fresh installs with no repo change. Bump this
# deliberately (and re-verify the patch stack) when moving to a newer DeSmuME.
DESMUME_COMMIT="a7570473c0c0d3271bf652f534ab8fd584c6dfae"

# 1. Work-tree source: prefer a read-only EMUCAP_DESMUME_SRC copy, else clone the pinned upstream.
if [ ! -d "$SRC/.git" ]; then
  if [ -n "${EMUCAP_DESMUME_SRC:-}" ]; then
    [ -d "$EMUCAP_DESMUME_SRC/.git" ] || { echo "ERROR: EMUCAP_DESMUME_SRC is not a git checkout: $EMUCAP_DESMUME_SRC" >&2; exit 1; }
    echo "→ DeSmuME work tree from EMUCAP_DESMUME_SRC (read-only copy): $EMUCAP_DESMUME_SRC"
    rm -rf "$SRC"
    git clone --local "$EMUCAP_DESMUME_SRC" "$SRC"
    src_rev="$(git -C "$SRC" rev-parse HEAD)"
    [ "$src_rev" = "$DESMUME_COMMIT" ] || echo "WARNING: EMUCAP_DESMUME_SRC HEAD $src_rev != supported $DESMUME_COMMIT — the patch stack may not apply cleanly." >&2
  else
    echo "→ cloning DeSmuME upstream (pinned $DESMUME_COMMIT): TASEmulators/desmume"
    git init -q "$SRC"
    git -C "$SRC" fetch -q --depth 1 https://github.com/TASEmulators/desmume.git "$DESMUME_COMMIT"
    git -C "$SRC" checkout -q FETCH_HEAD
    got="$(git -C "$SRC" rev-parse HEAD)"
    [ "$got" = "$DESMUME_COMMIT" ] || { echo "ERROR: DeSmuME revision mismatch: got $got expected $DESMUME_COMMIT" >&2; exit 1; }
  fi
fi
[ -d "$POSIX" ] || { echo "ERROR: desmume posix frontend missing: $POSIX (bad checkout?)" >&2; exit 1; }

# 2. Apply the repo-owned patch stack from the pristine upstream baseline.
#    0001 = headless CLI (X11/window removal); 0002 = emucap GDB-stub hooks
#    (screenshot + input custom RSP commands); 0003 = emucap state/disasm hooks
#    (savestate + disassemble custom RSP commands). Later patches add reset, touch, buffer bounds,
#    input ownership, and bounded GDB socket I/O. They extend the same gdbstub.cpp regions, so the
#    patches form one ordered stack.
#    Because later hunks sit adjacent to earlier ones, a per-patch reverse-check cannot tell
#    "already applied" from "conflict" once the stack is on disk. Instead we reset the
#    tracked sources to the clone's HEAD (dropping any prior application; untracked
#    build-headless/ artifacts are kept) and re-apply the whole stack forward. This is
#    idempotent by construction — every build reproduces the same patched tree.
cd "$SRC"
git checkout -- .
for entry in \
  "$PATCH|headless patch (0001)" \
  "$PATCH2|emucap hooks patch (0002)" \
  "$PATCH3|emucap state/disasm patch (0003)" \
  "$PATCH4|emucap reset patch (0004)" \
  "$PATCH5|emucap touch patch (0005)" \
  "$PATCH6|emucap gdb-bufmax patch (0006)" \
  "$PATCH7|emucap input-status patch (0007)" \
  "$PATCH8|emucap gdb I/O deadline patch (0008)" \
  "$PATCH9|emucap gdb SIGPIPE patch (0009)"; do
  patch="${entry%%|*}"
  label="${entry#*|}"
  echo "→ applying $label"
  git apply --check "$patch" || { echo "ERROR: $label does not apply cleanly (upstream drift?)" >&2; exit 1; }
  git apply "$patch"
done

# 3. Native build env (strip homebrew-LLVM pollution; the fork needs Apple clang on macOS).
. "$HERE/../_common/build-env.sh"
emucap_scrub_build_env

# 4. Configure + build the headless CLI with the GDB stub (gdb-stub forces the interpreter).
if [ ! -f "$BUILD/build.ninja" ]; then
  echo "→ meson setup ($BUILD)"
  ( cd "$POSIX" && meson setup build-headless \
      -Dfrontend-cli=true -Dgdb-stub=true \
      -Dfrontend-gtk=false -Dfrontend-gtk2=false -Dwifi=false )
fi
echo "→ ninja (-j$JOBS)"
ninja -C "$BUILD" -j"$JOBS"

BIN="$BUILD/cli/desmume-cli"
[ -x "$BIN" ] || { echo "ERROR: build did not produce $BIN" >&2; exit 1; }
echo "OK: $BIN"
