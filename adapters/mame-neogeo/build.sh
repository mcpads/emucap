#!/usr/bin/env bash
set -euo pipefail

# Neo Geo uses the same pinned MAME source build and generic Lua debugger lifecycle as PC-98.
exec "$(cd "$(dirname "$0")/../mame-pc98" && pwd)/build.sh" "$@"
