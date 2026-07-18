#!/usr/bin/env bash

# Cross-process lock for adapter work trees. The shell keeps descriptor 9 open for the rest of the
# build, so the kernel releases the lock if the build exits or crashes. The file remains as a
# diagnostic owner record; its existence alone never means that a build is active.

emucap_build_process_start() {
  local pid="$1"
  if [ -r "/proc/$pid/stat" ] && [ -r /proc/sys/kernel/random/boot_id ]; then
    local start boot
    start="$(sed 's/^.*) //' "/proc/$pid/stat" 2>/dev/null | awk '{print $20}')"
    boot="$(tr -d '\r\n' </proc/sys/kernel/random/boot_id 2>/dev/null)"
    if [ -n "$start" ] && [ -n "$boot" ]; then
      printf '%s:%s\n' "$boot" "$start"
      return 0
    fi
  fi
  ps -p "$pid" -o lstart= 2>/dev/null |
    sed 's/^[[:space:]]*//; s/[[:space:]]*$//' |
    grep .
}

emucap_build_owner_field() {
  local file="$1"
  local field="$2"
  sed -n "s/^${field}=//p" "$file" 2>/dev/null | head -n 1
}

emucap_acquire_build_lock() {
  local lock="$1"
  local label="${2:-adapter}"
  local parent start owner_pid lock_kind
  parent="$(dirname "$lock")"
  [ -d "$parent" ] || {
    echo "ERROR: build lock parent does not exist: $parent" >&2
    return 2
  }
  [ ! -L "$lock" ] || {
    echo "ERROR: build lock path is a symlink: $lock" >&2
    return 2
  }
  [ ! -d "$lock" ] || {
    echo "ERROR: legacy build lock directory found: $lock" >&2
    echo "       Verify that no build owns it, then remove the directory and retry." >&2
    return 2
  }
  start="$(emucap_build_process_start "$$" 2>/dev/null || true)"
  [ -n "$start" ] || {
    echo "ERROR: cannot determine the current process start identity" >&2
    return 2
  }

  # Opening without truncation preserves the current owner record until this process owns the lock.
  # Descriptor 9 intentionally remains open until the build shell exits.
  exec 9<>"$lock"
  chmod 600 "$lock"

  if command -v flock >/dev/null 2>&1; then
    lock_kind=flock
    if ! flock -n 9; then
      owner_pid="$(emucap_build_owner_field "$lock" pid)"
      echo "Waiting for $label build owned by pid ${owner_pid:-unknown}" >&2
      flock 9
    fi
  elif command -v lockf >/dev/null 2>&1; then
    lock_kind=lockf
    if ! lockf -s -t 0 9; then
      owner_pid="$(emucap_build_owner_field "$lock" pid)"
      echo "Waiting for $label build owned by pid ${owner_pid:-unknown}" >&2
      lockf 9
    fi
  else
    echo "ERROR: lockf or flock is required for adapter builds" >&2
    exec 9>&-
    return 2
  fi

  # Write after locking. Readers use the first value for each field, so a shorter record cannot
  # expose a previous owner even if the filesystem leaves trailing bytes after this overwrite.
  printf 'pid=%s\nstart=%s\nlabel=%s\nlock=%s\n' \
    "$$" "$start" "$label" "$lock_kind" >&9
  EMUCAP_ACTIVE_BUILD_LOCK="$lock"
  EMUCAP_ACTIVE_BUILD_START="$start"
  export EMUCAP_ACTIVE_BUILD_LOCK EMUCAP_ACTIVE_BUILD_START
}
