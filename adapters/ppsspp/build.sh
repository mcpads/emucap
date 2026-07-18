#!/usr/bin/env bash
# Build PPSSPPHeadless from a pinned PPSSPP commit for the emucap PSP adapter.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
WORK_INPUT="${EMUCAP_PPSSPP_WORK:-$HERE/work}"
[ ! -L "$WORK_INPUT" ] || { echo "ERROR: PPSSPP work path must not be a symlink: $WORK_INPUT" >&2; exit 1; }
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
emucap_acquire_build_lock "${EMUCAP_BUILD_LOCK:-$WORK/.build.lock}" "PPSSPP"
PPSSPP_COMMIT="${EMUCAP_PPSSPP_COMMIT:-56c694d88bbf82270e8b472fe63abd60f3f8e0a9}"
REPO="https://github.com/hrydgard/ppsspp.git"
# macOS: force Apple clang (homebrew LLVM breaks libc++).
if [ "$(uname)" = "Darwin" ]; then export CC=/usr/bin/clang CXX=/usr/bin/clang++; fi
# emucap ALWAYS builds inside its own work tree ($WORK/ppsspp), never in a caller-supplied checkout.
# EMUCAP_PPSSPP_SRC (like FLYCAST_SRC) is READ-ONLY input: it only supplies the `git clone` origin so
# the initial clone skips the network. The pinned checkout, the emucap patch stack, and the build all
# happen only in $SRC — the supplied checkout is never patched or built in.
SRC="$WORK/ppsspp"
ORIGIN="${EMUCAP_PPSSPP_SRC:-$REPO}"
if [ ! -d "$SRC/.git" ]; then git clone --recurse-submodules "$ORIGIN" "$SRC"; fi
git -C "$SRC" fetch --recurse-submodules origin
git -C "$SRC" checkout "$PPSSPP_COMMIT"
git -C "$SRC" submodule update --init --recursive
# Apply the repo-owned emucap patch stack (savestate.save/load + emucap.screenshot WebSocket
# commands, and a headless run-loop that survives GE stepping). The patches also add new source
# files, so a plain `git checkout -- .` (tracked files only) is not enough to reset: we restore
# tracked files to the pinned commit AND drop the patch-created untracked sources under Core/ and
# headless/ (build-headless/ artifacts live elsewhere and are kept), then re-apply forward. This is
# idempotent by construction — every build reproduces the same patched tree from the pinned commit.
git -C "$SRC" checkout -- .
git -C "$SRC" clean -fdq -- Core headless
for p in "$HERE"/patches/*.patch; do
  echo "→ applying $(basename "$p")"
  git -C "$SRC" apply --check "$p" || { echo "ERROR: patch does not apply cleanly (upstream drift?): $p" >&2; exit 1; }
  git -C "$SRC" apply "$p"
done
# HEADLESS=ON adds the PPSSPPHeadless target *alongside* the default desktop GUI target
# (PPSSPPSDL) — the two are not mutually exclusive, so one configure builds both from the same
# patched tree. PPSSPPHeadless is the default headless debugging path; PPSSPPSDL is the HITL
# `display:true` build (a real window a human sees and plays while the agent drives the same debugger
# WebSocket). Both carry the identical patch stack (loopback-bind + savestate/screenshot + the GUI
# --debugger=<port> honoring from 0005 + the isolated memstick from 0006), so the launcher can pick
# either binary with one build.
cmake -S "$SRC" -B "$SRC/build-headless" -DHEADLESS=ON -DCMAKE_BUILD_TYPE=Release
# PPSSPPHeadless is the guaranteed default target — a failure here fails the build (set -e).
cmake --build "$SRC/build-headless" --target PPSSPPHeadless -j
echo "built: $SRC/build-headless/PPSSPPHeadless"
# PPSSPPSDL (the HITL display:true window) needs SDL3 + sdl3_ttf, which a headless-only host may not
# have. Build it best-effort so a missing GUI toolchain never fails an otherwise-good headless build.
# Opt out entirely with EMUCAP_PPSSPP_BUILD_GUI=0.
if [ "${EMUCAP_PPSSPP_BUILD_GUI:-1}" != "0" ]; then
  if cmake --build "$SRC/build-headless" --target PPSSPPSDL -j; then
    echo "built: $SRC/build-headless/PPSSPPSDL.app (HITL display:true)"
  else
    echo "WARNING: PPSSPPSDL (GUI/display:true) target failed to build — PPSSPPHeadless is ready for headless debugging." >&2
    echo "         display:true (HITL window) needs SDL3 + sdl3_ttf; install them and re-run to enable it (or set EMUCAP_PPSSPP_BUILD_GUI=0 to skip)." >&2
  fi
else
  echo "skipped: PPSSPPSDL (EMUCAP_PPSSPP_BUILD_GUI=0) — headless only; display:true is unavailable until built"
fi
