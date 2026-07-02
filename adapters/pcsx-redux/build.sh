#!/usr/bin/env bash
# PCSX-Redux를 emucap용으로 빌드 — macOS 툴체인 핀고정(Apple clang).
#
# ⚠ 핵심: macOS에서 PCSX-Redux는 반드시 Apple clang(Xcode/CommandLineTools)으로 빌드한다.
#   사용자 환경에 CC/CXX=homebrew LLVM(/opt/homebrew/opt/llvm/bin/clang*)이 export돼 있으면
#   빌드가 실패한다 — homebrew LLVM(≥19) libc++가 비표준 std::char_traits<unsigned char>·
#   <std::byte>를 제거해 src/support/gnu-c++-demangler.cc 컴파일이 깨진다(implicit instantiation
#   of undefined template). PCSX-Redux CI도 LLVM을 설치하지 않고 Apple clang으로 빌드한다
#   (.github/scripts/install-brew-dependencies.sh = capstone curl ffmpeg freetype glfw libuv
#   pkg-config zlib, 컴파일러 없음).
#
# 즉 사용자 전역 CC/CXX를 무시하고 /usr/bin/clang(++)로 강제한다. brew 의존성은 위 패키지 필요.
#
# 사용: build.sh [PCSX_REDUX_REPO_PATH]  (기본: $HOME/pcsx-redux)
set -euo pipefail

REPO="${1:-$HOME/pcsx-redux}"
if [ ! -f "$REPO/Makefile" ]; then
  echo "ERROR: PCSX-Redux 소스가 없다: $REPO (grumpycoders/pcsx-redux 클론 경로를 인자로)" >&2
  exit 1
fi
cd "$REPO"

echo "→ 서브모듈 동기화"
git submodule update --init --recursive

JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
if [ "$(uname -s)" = "Darwin" ]; then
  # macOS: 전역 CC/CXX가 homebrew LLVM을 가리키면 libc++ 헤더가 깨지므로 Apple clang을 강제한다.
  echo "→ make (Apple clang: CC=/usr/bin/clang CXX=/usr/bin/clang++, -j$JOBS)"
  make -j"$JOBS" CC=/usr/bin/clang CXX=/usr/bin/clang++
else
  # Linux 등: 시스템 기본 컴파일러.
  echo "→ make (-j$JOBS)"
  make -j"$JOBS"
fi

echo ""
echo "✓ 빌드 완료: $REPO/bins/Release/pcsx-redux (Apple clang libc++)"
echo "  실행은 adapters/pcsx-redux/launch.sh — ⚠ GLFW라 실제 GUI 디스플레이 세션 필요(nohup/headless 불가)."
