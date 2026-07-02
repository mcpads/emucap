#!/usr/bin/env bash
# emucap 공용 빌드 환경 정규화 — 각 어댑터 build.sh가 configure 전에 `source` 후 emucap_scrub_build_env를 부른다.
#
# 근본원인: macOS에서 전역 CC/CXX/LDFLAGS/CPPFLAGS 등이 homebrew LLVM(/opt/homebrew/opt/llvm)을 가리키면
# libc++ 헤더 검색과 Cocoa/Objective-C(.mm) 빌드가 깨진다. 소스빌드 어댑터(Mednafen·PCSX·Flycast)가 공통으로
# 부딪히는 지점이라 어댑터마다 재발명하지 않고 여기 한 곳에서 걷어낸다. Apple clang이 bare `clang`으로 잡히도록
# /usr/bin을 PATH 앞에 둔다. Linux/기타는 시스템 기본 툴체인이 맞으니 무개입.
emucap_scrub_build_env() {
  [ "$(uname -s)" = "Darwin" ] || return 0
  unset CC CXX OBJC OBJCXX LD CPP \
        CFLAGS CXXFLAGS OBJCFLAGS OBJCXXFLAGS LDFLAGS CPPFLAGS \
        CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH \
        LIBRARY_PATH DYLD_LIBRARY_PATH CMAKE_PREFIX_PATH PKG_CONFIG_PATH
  export PATH="/usr/bin:$PATH"
}
