#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-config-corpus.sh <corpus-dir> [options]

Parse-check a directory of tmux configuration files through rmux source-file.
The corpus is supplied by the caller so third-party configs do not need to be
vendored into the repository.

Options:
  --rmux PATH       rmux binary to exercise (default: target/debug/rmux)
  --mode MODE       parse-only or startup-fallback (default: parse-only)
  --max-files N     Stop after N files
  --keep-going      Continue after failures and report a failing summary
  --results PATH    Write TSV results to PATH (default: /tmp/rmux-config-corpus-results-$$.tsv)
  -h, --help        Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

corpus_dir=""
rmux="target/debug/rmux"
mode="parse-only"
max_files=""
keep_going=0
results=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --rmux)
      [ "$#" -ge 2 ] || die "--rmux requires a value"
      rmux="$2"
      shift 2
      ;;
    --mode)
      [ "$#" -ge 2 ] || die "--mode requires a value"
      mode="$2"
      case "$mode" in
        parse-only|startup-fallback) ;;
        *) die "--mode must be parse-only or startup-fallback" ;;
      esac
      shift 2
      ;;
    --max-files)
      [ "$#" -ge 2 ] || die "--max-files requires a value"
      max_files="$2"
      case "$max_files" in
        ''|*[!0-9]*) die "--max-files must be a positive integer" ;;
      esac
      [ "$max_files" -gt 0 ] || die "--max-files must be positive"
      shift 2
      ;;
    --keep-going)
      keep_going=1
      shift
      ;;
    --results)
      [ "$#" -ge 2 ] || die "--results requires a value"
      results="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -n "$corpus_dir" ]; then
        die "unexpected extra argument: $1"
      fi
      corpus_dir="$1"
      shift
      ;;
  esac
done

[ -n "$corpus_dir" ] || die "corpus directory is required"
[ -d "$corpus_dir" ] || die "corpus directory does not exist: $corpus_dir"
[ -x "$rmux" ] || die "rmux binary is not executable: $rmux"
corpus_dir="$(cd "$corpus_dir" && pwd)"

workdir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-config-corpus.XXXXXX")"

if [ -z "$results" ]; then
  results="${TMPDIR:-/tmp}/rmux-config-corpus-results-$$.tsv"
fi
mkdir -p "$(dirname "$results")"
printf 'status\tpath\texit\tmessage\n' >"$results"

label="config-corpus-$$"
session="config_corpus_$$"
home="$workdir/home"
xdg="$workdir/xdg"
mkdir -p "$home" "$xdg"

run_rmux() {
  HOME="$home" XDG_CONFIG_HOME="$xdg" "$rmux" -L "$label" "$@"
}

run_rmux -f /dev/null kill-server >/dev/null 2>&1 || true
cleanup() {
  run_rmux kill-server >/dev/null 2>&1 || true
  rm -rf "$workdir"
}
trap cleanup EXIT

if [ "$mode" = "parse-only" ]; then
  run_rmux -f /dev/null new-session -d -s "$session" >/dev/null
fi

run_parse_only() {
  local path="$1" stdout="$2" stderr="$3"
  run_rmux source-file -n -v "$path" >"$stdout" 2>"$stderr"
}

is_gpakosz_config() {
  local path="$1"
  grep -q 'TMUX_CONF_LOCAL' "$path" && grep -q '_apply_configuration' "$path"
}

run_startup_fallback() {
  local path="$1" stdout="$2" stderr="$3" marker="$4"
  local tmux_dir="$xdg/tmux"
  local startup_config="$workdir/startup-$marker.conf"
  rm -rf "$tmux_dir"
  mkdir -p "$tmux_dir" || return $?
  cat "$path" >"$startup_config" || return $?
  printf '\nset -g @rmux-corpus-loaded %s\n' "$marker" >>"$startup_config" || return $?
  ln -s "$startup_config" "$tmux_dir/tmux.conf" || return $?
  run_rmux kill-server >/dev/null 2>&1 || true
  run_rmux new-session -d -s "$session" >"$stdout" 2>"$stderr"
  local status=$?
  if [ "$status" -eq 0 ]; then
    local observed=""
    observed="$(run_rmux show-options -gqv @rmux-corpus-loaded 2>>"$stderr")"
    local show_status=$?
    if [ "$show_status" -ne 0 ]; then
      printf 'failed to read startup fallback marker\n' >>"$stderr"
      status="$show_status"
    elif [ "$observed" != "$marker" ]; then
      printf 'startup fallback marker mismatch: expected %s, got %s\n' "$marker" "$observed" >>"$stderr"
      status=1
    fi
    local messages=""
    messages="$(run_rmux show-messages 2>>"$stderr")"
    local messages_status=$?
    if [ "$messages_status" -ne 0 ]; then
      printf 'failed to read startup fallback show-messages\n' >>"$stderr"
      status="$messages_status"
    elif printf '%s\n' "$messages" | grep -Eq 'config error|:[0-9]+: unmatched \}'; then
      printf 'startup fallback emitted config diagnostics in show-messages:\n%s\n' "$messages" >>"$stderr"
      status=1
    fi
    if is_gpakosz_config "$path"; then
      local extended_keys=""
      extended_keys="$(run_rmux show-options -gqv extended-keys 2>>"$stderr")"
      local extended_status=$?
      if [ "$extended_status" -ne 0 ]; then
        printf 'failed to read gpakosz extended-keys option\n' >>"$stderr"
        status="$extended_status"
      elif [ "$extended_keys" != "on" ]; then
        printf 'gpakosz startup fallback expected extended-keys=on, got %s\n' "$extended_keys" >>"$stderr"
        status=1
      fi
    fi
  fi
  run_rmux kill-server >/dev/null 2>&1 || true
  return "$status"
}

total=0
failed=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  total=$((total + 1))
  stdout="$workdir/$total.out"
  stderr="$workdir/$total.err"
  set +e
  if [ "$mode" = "startup-fallback" ]; then
    run_startup_fallback "$path" "$stdout" "$stderr" "$total"
  else
    run_parse_only "$path" "$stdout" "$stderr"
  fi
  status=$?
  set -e

  if [ "$status" -eq 0 ]; then
    message="$(tr '\n' ' ' <"$stderr" | sed 's/[[:space:]][[:space:]]*/ /g; s/^\ //; s/\ $//')"
    printf 'ok\t%s\t%s\t%s\n' "$path" "$status" "$message" >>"$results"
  else
    failed=$((failed + 1))
    message="$(cat "$stdout" "$stderr" | tr '\n' ' ' | sed 's/[[:space:]][[:space:]]*/ /g; s/^\ //; s/\ $//')"
    printf 'fail\t%s\t%s\t%s\n' "$path" "$status" "$message" >>"$results"
    if [ "$keep_going" -eq 0 ]; then
      printf 'failed config: %s\n' "$path" >&2
      if [ -s "$stdout" ]; then
        printf '%s\n' '--- stdout ---' >&2
        cat "$stdout" >&2
      fi
      if [ -s "$stderr" ]; then
        printf '%s\n' '--- stderr ---' >&2
        cat "$stderr" >&2
      fi
      die "config corpus failed; partial results: $results"
    fi
  fi

  if [ -n "$max_files" ] && [ "$total" -ge "$max_files" ]; then
    break
  fi
done < <(find "$corpus_dir" -type f \( -name '*.conf' -o -name '.tmux.conf' -o -name 'tmux.conf' \) -print | LC_ALL=C sort)

printf 'mode=%s\n' "$mode"
printf 'total=%s\n' "$total"
printf 'failed=%s\n' "$failed"
printf 'results=%s\n' "$results"

if [ "$total" -eq 0 ]; then
  die "no tmux config files found in corpus: $corpus_dir"
fi
if [ "$failed" -ne 0 ]; then
  die "$failed of $total configs failed"
fi
