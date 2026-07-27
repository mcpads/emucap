#!/usr/bin/env bash
set -euo pipefail

# Neo Geo reuses the pinned MAME recipe and generic Lua debugger lifecycle, but
# owns a separate build tree so a PC-98 subset build cannot replace its driver.
HERE="$(cd "$(dirname "$0")" && pwd)"
export MAME_WORK="${MAME_WORK:-$HERE/work}"
export MAME_SOURCES="${MAME_SOURCES:-src/mame/neogeo/neogeo.cpp,src/mame/neogeo/neogeocd.cpp}"
export MAME_BUILD_LABEL="${MAME_BUILD_LABEL:-MAME Neo Geo}"
export MAME_WORK_OWNER_FILE="${MAME_WORK_OWNER_FILE:-.emucap-mame-neogeo-work}"
exec "$HERE/../mame-pc98/build.sh" "$@"
