#!/usr/bin/env bash
# CI environment for the maintained Unix native recipes.
set -euo pipefail
adapter="$1"
source_root="$(cd "$2" && pwd)"
cd "$source_root"
export EMUCAP_BUILD_JOBS=3 MAME_JOBS=3 DESMUME_JOBS=3 PPSSPP_JOBS=3
export EMUCAP_FLYCAST_BUILD_HOME="$source_root/adapters/flycast/work"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
if [ "$adapter" = mame-pc98 ]; then export MAME_SOURCES=src/mame/nec/pc9801.cpp; fi
if [ "$(uname -s)" = Darwin ]; then
  export HOMEBREW_NO_AUTO_UPDATE=1
  packages=(flock cmake ninja pkgconf meson autoconf automake libtool dylibbundler python@3.13)
  case "$adapter" in
    mesen2) packages+=(sdl2-compat) ;;
    dolphin) packages+=(qt@6 libusb sfml ffmpeg miniupnpc pugixml) ;;
    ppsspp) packages+=(sdl3 sdl3_ttf fontconfig) ;;
    mednafen) packages+=(sdl2-compat libsndfile flac zlib lzo) ;;
    flycast) packages+=(sdl2-compat libzip) ;;
    np2kai) ;;
    mupen64plus) packages+=(sdl2-compat libpng freetype binutils) ;;
    openmsx) packages+=(freetype glew libogg libpng libvorbis sdl2-compat sdl2_ttf tcl-tk theora) ;;
    mame-pc98|mame-neogeo) packages+=(sdl2-compat sdl2_ttf) ;;
    pcsx2) packages+=(nasm) ;;
    desmume-nds) packages+=(sdl2-compat glib libpcap) ;;
    xemu) packages+=(nasm wget) ;;
    *) exit 2 ;;
  esac
  brew install "${packages[@]}"
  export PATH="$(brew --prefix python@3.13)/libexec/bin:$PATH"
  if [ "$adapter" = pcsx2 ]; then
    sudo softwareupdate --install-rosetta --agree-to-license
    xcrun --find metal
  fi
  brew list --versions > native-toolchain.txt
else
  packages=(build-essential clang cmake ninja-build pkg-config meson autoconf automake libtool
    curl git python3-dev patchelf gettext nasm zlib1g-dev libgl1-mesa-dev libegl1-mesa-dev)
  case "$adapter" in
    mesen2) packages+=(libsdl2-dev libasound2-dev) ;;
    dolphin) packages+=(qt6-base-dev qt6-tools-dev libqt6svg6-dev libusb-1.0-0-dev libevdev-dev
      libsfml-dev libavcodec-dev libavformat-dev libswscale-dev libminiupnpc-dev libpugixml-dev
      libudev-dev libcurl4-openssl-dev liblzma-dev libbz2-dev libxxhash-dev libfmt-dev libxrandr-dev libxi-dev) ;;
    ppsspp) packages+=(libsdl2-dev libfontconfig1-dev libfreetype-dev libvulkan-dev libglew-dev libsnappy-dev) ;;
    mednafen) packages+=(libsdl2-dev libsndfile1-dev libflac-dev liblzo2-dev libasound2-dev) ;;
    flycast) packages+=(libsdl2-dev libzip-dev libcurl4-openssl-dev libpulse-dev libudev-dev libvulkan-dev) ;;
    np2kai) ;;
    mupen64plus) packages+=(libsdl2-dev libpng-dev libfreetype-dev binutils-dev libglu1-mesa-dev libvulkan-dev) ;;
    openmsx) packages+=(libsdl2-dev libsdl2-ttf-dev libglew-dev libtheora-dev libvorbis-dev libogg-dev
      libpng-dev libfreetype-dev tcl8.6-dev libasound2-dev) ;;
    mame-pc98|mame-neogeo) packages+=(libsdl2-dev libsdl2-ttf-dev libfontconfig1-dev libexpat1-dev
      libflac-dev libportmidi-dev libasound2-dev qt6-base-dev) ;;
    pcsx2) packages+=(extra-cmake-modules libasound2-dev libaio-dev libcurl4-openssl-dev libdbus-1-dev
      libdecor-0-dev libevdev-dev libfontconfig-dev libfreetype-dev libgtk-3-dev libgudev-1.0-dev
      libharfbuzz-dev libinput-dev libopengl-dev libopus-dev libpcap-dev libpipewire-0.3-dev
      libpulse-dev libssl-dev libudev-dev libvpl-dev libva-dev libwayland-dev libx11-dev libx11-xcb-dev
      libx264-dev libxcb1-dev libxcb-composite0-dev libxcb-cursor-dev libxcb-damage0-dev
      libxcb-glx0-dev libxcb-icccm4-dev libxcb-image0-dev libxcb-keysyms1-dev libxcb-present-dev
      libxcb-randr0-dev libxcb-render0-dev libxcb-render-util0-dev libxcb-shape0-dev
      libxcb-shm0-dev libxcb-sync-dev libxcb-util-dev libxcb-xfixes0-dev libxcb-xinput-dev
      libxcb-xkb-dev libxext-dev libxkbcommon-x11-dev libxrandr-dev lld llvm) ;;
    desmume-nds) packages+=(libsdl2-dev libglib2.0-dev libpcap-dev) ;;
    xemu) packages+=(libsdl2-dev libepoxy-dev libpixman-1-dev libgtk-3-dev libssl-dev libsamplerate0-dev
      libpcap-dev libslirp-dev libvulkan-dev libusb-1.0-0-dev libpulse-dev libglib2.0-dev libcurl4-openssl-dev) ;;
    *) exit 2 ;;
  esac
  sudo apt-get update -qq
  sudo apt-get install -y --no-install-recommends "${packages[@]}"
  dpkg-query -W > native-toolchain.txt
  if [ "$adapter" = pcsx2 ]; then
    export CC=clang CXX=clang++
    . adapters/pcsx2/upstream.lock
    upstream="$source_root/adapters/pcsx2/work/pcsx2"
    mkdir -p "$upstream"
    git -C "$upstream" init -q
    git -C "$upstream" remote add origin "$PCSX2_REPO"
    git -C "$upstream" fetch --depth 1 origin "$PCSX2_COMMIT"
    git -C "$upstream" checkout -q FETCH_HEAD
    deps="$source_root/native-dependencies/pcsx2"
    mkdir -p "$source_root/native-dependency-sources/pcsx2"
    if [ ! -f "$deps/.emucap-complete" ]; then
      (cd "$source_root/native-dependency-sources/pcsx2"; BUILD_FFMPEG=1 bash "$upstream/.github/workflows/scripts/linux/build-dependencies-qt.sh" "$deps")
      touch "$deps/.emucap-complete"
    fi
    export CMAKE_PREFIX_PATH="$deps"
    export PKG_CONFIG_PATH="$deps/lib/pkgconfig"
    export LD_LIBRARY_PATH="$deps/lib:${LD_LIBRARY_PATH:-}"
  fi
fi
if [ "$adapter" = xemu ]; then
  # Match the Python chosen by the producer recipe and its Meson environment.
  export EMUCAP_XEMU_PYTHON="$(command -v python3.13)"
  "$EMUCAP_XEMU_PYTHON" -m venv "$source_root/native-python"
  export EMUCAP_XEMU_PYTHON="$source_root/native-python/bin/python3"
  "$EMUCAP_XEMU_PYTHON" -m pip install 'PyYAML==6.0.2'
fi
bash _tests/adapters/build-lock-test.sh
bash "adapters/$adapter/build.sh"
if [ "$adapter" = np2kai ]; then
  extension=so
  if [ "$(uname -s)" = Darwin ]; then extension=dylib; fi
  python3 _tests/native/np2kai-load-api.py "adapters/np2kai/work/np2kai/sdl/np2kai_libretro.$extension"
fi
