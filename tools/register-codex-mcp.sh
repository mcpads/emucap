#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

find_binary() {
  name="$1"
  for path in \
    "$repo_root/target/release/$name" \
    "$repo_root/target/release/$name.exe"
  do
    if [ -f "$path" ] && [ -x "$path" ]; then
      printf '%s\n' "$path"
      return 0
    fi
  done
  printf 'Missing %s. Run cargo build --release --bin emucap --bin emucap-mcp --bin emucap-track-mcp --bin emucap-broker --bin emucap-mame-pc98-bridge.\n' "$name" >&2
  return 1
}

codex_cli="${CODEX_CLI:-codex}"
if [ "${EMUCAP_REGISTER_DRY_RUN:-0}" != "1" ] && ! command -v "$codex_cli" >/dev/null 2>&1; then
  printf 'Codex CLI not found. Install Codex or set CODEX_CLI=/absolute/path/to/codex.\n' >&2
  exit 1
fi

control_mcp=$(find_binary emucap-mcp)
track_mcp=$(find_binary emucap-track-mcp)

if [ "${EMUCAP_REGISTER_DRY_RUN:-0}" = "1" ]; then
  printf 'Dry run: would register Codex MCP servers:\n'
else
  "$codex_cli" mcp add emucap --env "EMUCAP_REPO_ROOT=$repo_root" -- "$control_mcp"
  "$codex_cli" mcp add emucap-track -- "$track_mcp"
  printf 'Registered Codex MCP servers:\n'
fi

printf '  emucap       -> %s\n' "$control_mcp"
printf '  emucap-track -> %s\n' "$track_mcp"
if [ "${EMUCAP_REGISTER_DRY_RUN:-0}" != "1" ]; then
  printf 'Reconnect the agent session so the new MCP tool list is loaded.\n'
fi
