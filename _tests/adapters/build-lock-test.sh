#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
helper="$repo_root/adapters/_common/build-lock.sh"
tmp="$(mktemp -d)"
cleanup() {
  local child
  for child in "${holder_pid:-}" "${contender_pid:-}"; do
    [ -n "$child" ] || continue
    kill "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  done
  rm -rf "$tmp"
}
trap cleanup EXIT

wait_for_path() {
  local path="$1"
  local attempts=0
  while [ ! -e "$path" ]; do
    attempts=$((attempts + 1))
    [ "$attempts" -lt 300 ] || {
      echo "ERROR: timed out waiting for $path" >&2
      return 1
    }
    sleep 0.1
  done
}

lock="$tmp/concurrent.lock"
log="$tmp/order.log"
release="$tmp/release"
waiting="$tmp/waiting.log"

bash -c '
  set -euo pipefail
  . "$1"
  emucap_acquire_build_lock "$2" "test holder"
  printf "holder\n" >>"$3"
  while [ ! -e "$4" ]; do sleep 0.05; done
' bash "$helper" "$lock" "$log" "$release" &
holder_pid=$!
# The lock file exists before its owner metadata is written. The holder's log is
# published after acquisition, so it is the synchronization point for inspection.
wait_for_path "$log"
grep -Eq '^pid=[0-9]+$' "$lock"
grep -Eq '^start=.+$' "$lock"

# An old mtime must not make a live owner's lock reclaimable.
touch -t 200001010000 "$lock"
bash -c '
  set -euo pipefail
  . "$1"
  emucap_acquire_build_lock "$2" "test contender"
  printf "contender\n" >>"$3"
' bash "$helper" "$lock" "$log" >"$waiting" 2>&1 &
contender_pid=$!
attempts=0
until grep -q '^Waiting for test contender build' "$waiting"; do
  attempts=$((attempts + 1))
  [ "$attempts" -lt 300 ] || {
    cat "$waiting" >&2
    touch "$release"
    echo "ERROR: contender did not reach the held lock" >&2
    exit 1
  }
  sleep 0.1
done
if grep -q '^contender$' "$log"; then
  echo "ERROR: contender stole a live build lock" >&2
  exit 1
fi
touch "$release"
wait "$holder_pid"
holder_pid=
wait "$contender_pid"
contender_pid=
grep -q '^contender$' "$log"

# A dead owner record does not block acquisition because the kernel lock is authoritative.
printf 'pid=999999\nstart=not-this-process\nlabel=stale\n' >"$lock"
bash -c '
  set -euo pipefail
  . "$1"
  emucap_acquire_build_lock "$2" "stale recovery"
' bash "$helper" "$lock"
grep -q '^label=stale recovery$' "$lock"

# A failed critical section releases the kernel lock, so the immediate retry can acquire it.
set +e
bash -c '
  set -euo pipefail
  . "$1"
  emucap_acquire_build_lock "$2" "failed build"
  false
' bash "$helper" "$lock"
failed_status=$?
set -e
[ "$failed_status" -ne 0 ]
bash -c '
  set -euo pipefail
  . "$1"
  emucap_acquire_build_lock "$2" "retry"
' bash "$helper" "$lock"
grep -q '^label=retry$' "$lock"

echo "adapter build lock tests passed"
