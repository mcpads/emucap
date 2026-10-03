#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:?usage: fetch-json.sh destination-header}"
URL="$(sed -n 's/^JSON_URL=//p' "$HERE/json.lock")"
EXPECTED="$(sed -n 's/^JSON_SHA256=//p' "$HERE/json.lock")"
[[ "$URL" == https://* ]] && [[ "$EXPECTED" =~ ^[0-9a-f]{64}$ ]] || exit 1
digest() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1";
  else sha256sum "$1"; fi | awk '{print $1}'
}
if [ -f "$DEST" ]; then
  [ "$(digest "$DEST")" = "$EXPECTED" ] || { echo 'ERROR: cached JSON header checksum mismatch' >&2; exit 1; }
  exit 0
fi
mkdir -p "$(dirname "$DEST")"
DOWNLOAD="$(mktemp "${DEST}.download.XXXXXX")"
trap 'rm -f "$DOWNLOAD"' EXIT
curl -fsSL "$URL" -o "$DOWNLOAD"
[ "$(digest "$DOWNLOAD")" = "$EXPECTED" ] || { echo 'ERROR: downloaded JSON header checksum mismatch' >&2; exit 1; }
mv "$DOWNLOAD" "$DEST"
