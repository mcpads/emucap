#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
wrapper="$repo_root/adapters/mame-pc98/mame-headless.sh"
printf_bin=/usr/bin/printf

if [ ! -x "$printf_bin" ]; then
  printf 'ERROR: external printf executable is unavailable: %s\n' "$printf_bin" >&2
  exit 1
fi

has_sound_none() {
  awk 'previous == "-sound" && $0 == "none" { found = 1 } { previous = $0 } END { exit !found }'
}

silent="$({ EMUCAP_MAME_RAW_BIN="$printf_bin" "$wrapper" '%s\n' probe; })"
if ! printf '%s\n' "$silent" | has_sound_none; then
  printf 'ERROR: safe PC-98 wrapper did not enforce silent-by-default launch\n' >&2
  exit 1
fi

audible="$({ MAME_ALLOW_SOUND=1 EMUCAP_MAME_RAW_BIN="$printf_bin" "$wrapper" '%s\n' probe; })"
if printf '%s\n' "$audible" | has_sound_none; then
  printf 'ERROR: safe PC-98 wrapper reintroduced -sound none after audio authorization\n' >&2
  exit 1
fi
if ! printf '%s\n' "$audible" | grep -Fqx -- '-video'; then
  printf 'ERROR: audio authorization weakened the headless video policy\n' >&2
  exit 1
fi

printf 'PC-98 sound policy tests passed\n'
