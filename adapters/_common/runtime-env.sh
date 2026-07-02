emucap_windows_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

emucap_temp_dir() {
  case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW*|MSYS*|CYGWIN*)
      local v
      for v in "${TMPDIR:-}" "${TEMP:-}" "${TMP:-}"; do
        if [ -n "$v" ]; then
          emucap_windows_path "$v"
          return 0
        fi
      done
      printf '%s\n' "/tmp"
      ;;
    *)
      printf '%s\n' "/tmp"
      ;;
  esac
}

emucap_session_token_file() {
  local dir
  dir="$(emucap_temp_dir)"
  printf '%s/emucap_session_token_%s\n' "${dir%/}" "$1"
}
