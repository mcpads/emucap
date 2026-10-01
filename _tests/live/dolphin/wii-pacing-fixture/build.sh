#!/usr/bin/env bash
# Build the standalone WPAD observation guest in a pinned open-source toolchain.
set -euo pipefail
if [ "$#" -lt 1 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 NEW_OUTPUT_DIRECTORY [RETRACES_PER_IMAGE:1|2] [RENDERER:cpu|gx|gx-split|gx-texture|gx-texture-ram|gx-texture-ram-retained|gx-depth-retained]" >&2
  exit 2
fi
image_period="${2:-1}"
case "$image_period" in
  1|2) ;;
  *) echo "RETRACES_PER_IMAGE must be 1 or 2" >&2; exit 2 ;;
esac
renderer="${3:-cpu}"
texture_copy=1
clear_efb=1
use_depth=0
case "$renderer" in
  cpu) use_gx=0; gx_parts=1; use_texture=0 ;;
  gx) use_gx=1; gx_parts=1; use_texture=0 ;;
  gx-split) use_gx=1; gx_parts=2; use_texture=0 ;;
  gx-texture) use_gx=1; gx_parts=1; use_texture=1 ;;
  gx-texture-ram) use_gx=1; gx_parts=1; use_texture=1; texture_copy=0 ;;
  gx-texture-ram-retained) use_gx=1; gx_parts=1; use_texture=1; texture_copy=0; clear_efb=0 ;;
  gx-depth-retained) use_gx=1; gx_parts=1; use_texture=1; texture_copy=0; clear_efb=0; use_depth=1 ;;
  *) echo "RENDERER must be cpu, gx, gx-split, gx-texture, gx-texture-ram, gx-texture-ram-retained or gx-depth-retained" >&2; exit 2 ;;
esac
source_dir="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$(dirname "$1")"
mkdir "$1"
output_dir="$(cd "$1" && pwd)"
image=devkitpro/devkitppc@sha256:01fb170c2c3f32d54606a4c37b720f4f731f85a829340c5768a992d29e201512
printf '%s\n' "$image" > "$output_dir/toolchain-image.txt"
docker run --rm --env "FIXTURE_RETRACES_PER_IMAGE=$image_period" \
  --env "FIXTURE_USE_GX=$use_gx" --env "FIXTURE_RENDERER=$renderer" \
  --env "FIXTURE_GX_PARTS=$gx_parts" --env "FIXTURE_TEXTURE=$use_texture" \
  --env "FIXTURE_TEXTURE_COPY=$texture_copy" \
  --env "FIXTURE_CLEAR_EFB=$clear_efb" \
  --env "FIXTURE_DEPTH=$use_depth" \
  --mount "type=bind,source=$source_dir,target=/source,readonly" \
  --mount "type=bind,source=$output_dir,target=/build" -w /build \
  "$image" bash -lc '
    set -euo pipefail
    export PATH=/opt/devkitpro/devkitPPC/bin:$PATH
    dkp-pacman -Q > packages.txt
    powerpc-eabi-gcc --version > compiler.txt
    cp /source/main.c source.c
    printf "%s\n" "$FIXTURE_RETRACES_PER_IMAGE" > presentation-period.txt
    printf "%s\n" "$FIXTURE_RENDERER" > renderer.txt
    powerpc-eabi-gcc -g -O2 -Wall -DGEKKO -mrvl -mcpu=750 -meabi -mhard-float \
      -DFIXTURE_RETRACES_PER_IMAGE="$FIXTURE_RETRACES_PER_IMAGE" \
      -DFIXTURE_USE_GX="$FIXTURE_USE_GX" -DFIXTURE_GX_PARTS="$FIXTURE_GX_PARTS" \
      -DFIXTURE_TEXTURE="$FIXTURE_TEXTURE" -DFIXTURE_TEXTURE_COPY="$FIXTURE_TEXTURE_COPY" \
      -DFIXTURE_CLEAR_EFB="$FIXTURE_CLEAR_EFB" \
      -DFIXTURE_DEPTH="$FIXTURE_DEPTH" \
      -I/opt/devkitpro/libogc/include -c source.c -o fixture.o
    powerpc-eabi-gcc -g -DGEKKO -mrvl -mcpu=750 -meabi -mhard-float \
      -Wl,-Map,fixture.map fixture.o -L/opt/devkitpro/libogc/lib/wii \
      -lwiiuse -lbte -logc -lm -o fixture.elf
    powerpc-eabi-nm -n fixture.elf > symbols.txt
    powerpc-eabi-objdump -d fixture.elf > disassembly.txt
    sha256sum source.c fixture.elf > SHA256SUMS.txt
  '
