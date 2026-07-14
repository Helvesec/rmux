#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-tiny-release-review.sh [options]

Build or reuse a real release layout and run the tiny CLI release-review smoke:
  - bin/rmux is the tiny public CLI
  - libexec/rmux/rmux is the full helper
  - bin/rmux-daemon is the daemon
  - tmux 3.7b is used as the authority for known CLI ambiguity cases when available

Options:
  --layout DIR         Reuse an existing package layout.
  --target-dir DIR     Cargo target dir for release builds.
  --skip-build         Do not build; requires --layout.
  --tmux PATH          tmux binary to use for authority checks.
  --no-tmux            Skip tmux authority checks.
  -h, --help           Show this help.

Set RMUX_REQUIRE_TMUX=1 to fail instead of skipping when tmux is unavailable.
USAGE
}

DEFAULT_FROZEN_TMUX_PATH="/opt/rmux/reference/tmux-frozen/e802909de06012a4df6209d55e86487c56223163/tmux"
FROZEN_TMUX_VERSION="tmux 3.7b"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

log() {
  printf '[tiny-smoke] %s\n' "$*"
}

run_capture() {
  local prefix="$1"
  shift
  set +e
  RMUX_TINY_TRACE=1 "$RMUX" "$@" >"$SMOKE_ROOT/${prefix}.out" 2>"$SMOKE_ROOT/${prefix}.err"
  local status=$?
  set -e
  printf '%s' "$status" >"$SMOKE_ROOT/${prefix}.rc"
}

assert_rc() {
  local prefix="$1" expected="$2" actual
  actual="$(cat "$SMOKE_ROOT/${prefix}.rc")"
  if [ "$actual" != "$expected" ]; then
    printf 'stderr for %s:\n' "$prefix" >&2
    cat "$SMOKE_ROOT/${prefix}.err" >&2
    printf 'stdout for %s:\n' "$prefix" >&2
    cat "$SMOKE_ROOT/${prefix}.out" >&2
    die "$prefix exited $actual, expected $expected"
  fi
}

assert_trace() {
  local prefix="$1" needle="$2"
  if ! grep -Fq "$needle" "$SMOKE_ROOT/${prefix}.err"; then
    printf 'stderr for %s:\n' "$prefix" >&2
    cat "$SMOKE_ROOT/${prefix}.err" >&2
    die "$prefix did not include trace: $needle"
  fi
}

assert_stdout_line() {
  local prefix="$1" expected="$2"
  grep -Fqx "$expected" "$SMOKE_ROOT/${prefix}.out" ||
    die "$prefix stdout did not contain exact line: $expected"
}

assert_stdout_contains() {
  local prefix="$1" expected="$2"
  grep -Fq "$expected" "$SMOKE_ROOT/${prefix}.out" ||
    die "$prefix stdout did not contain: $expected"
}

assert_stderr_contains() {
  local prefix="$1" expected="$2"
  grep -Fq "$expected" "$SMOKE_ROOT/${prefix}.err" ||
    die "$prefix stderr did not contain: $expected"
}

assert_stdout_not_empty() {
  local prefix="$1"
  [ -s "$SMOKE_ROOT/${prefix}.out" ] || die "$prefix stdout was empty"
}

assert_stderr_has_no_user_output_except_trace() {
  local prefix="$1" trace="$2"
  if grep -Fvx "$trace" "$SMOKE_ROOT/${prefix}.err" >"$SMOKE_ROOT/${prefix}.user-err"; then
    if [ -s "$SMOKE_ROOT/${prefix}.user-err" ]; then
      printf 'stderr for %s:\n' "$prefix" >&2
      cat "$SMOKE_ROOT/${prefix}.err" >&2
      die "$prefix stderr included user-visible output"
    fi
  fi
}

build_layout() {
  local build_dir="$1" layout_dir="$2"
  export CARGO_TARGET_DIR="$build_dir"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

  log "building full helper"
  cargo build --locked --release --package rmux --bin rmux
  mkdir -p "$layout_dir/libexec/rmux"
  install -m 755 "$build_dir/release/rmux" "$layout_dir/libexec/rmux/rmux"

  log "building tiny public CLI"
  cargo build --locked --release --package rmux --features tiny-cli --bin rmux
  mkdir -p "$layout_dir/bin"
  install -m 755 "$build_dir/release/rmux" "$layout_dir/bin/rmux"

  log "building daemon"
  cargo build --locked --release --package rmux --bin rmux-daemon
  install -m 755 "$build_dir/release/rmux-daemon" "$layout_dir/bin/rmux-daemon"
}

tmux_available() {
  [ -n "$TMUX_BIN" ] && [ -x "$TMUX_BIN" ]
}

tmux_required() {
  case "$(printf '%s' "${RMUX_REQUIRE_TMUX:-0}" | tr '[:upper:]' '[:lower:]')" in
    1|true|yes|on) return 0 ;;
    *) return 1 ;;
  esac
}

resolve_tmux_authority() {
  if [ -n "$TMUX_BIN" ]; then
    return
  fi
  for candidate in "${RMUX_TMUX_ORACLE:-}" "${RMUX_FROZEN_TMUX:-}" "$DEFAULT_FROZEN_TMUX_PATH"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
      TMUX_BIN="$candidate"
      return
    fi
  done
}

assert_tmux_authority_version() {
  local version
  version="$("$TMUX_BIN" -V 2>/dev/null)" ||
    die "tmux authority '$TMUX_BIN' did not run -V"
  [ "$version" = "$FROZEN_TMUX_VERSION" ] ||
    die "tmux authority '$TMUX_BIN' reported '$version', expected '$FROZEN_TMUX_VERSION'"
}

run_tmux_authority_smoke() {
  if [ "$SKIP_TMUX" -eq 1 ]; then
    log "skipping tmux authority checks (--no-tmux)"
    return
  fi
  if ! tmux_available; then
    if tmux_required; then
      die "tmux 3.7b authority checks required but no pinned oracle was found"
    fi
    log "skipping tmux authority checks (pinned tmux 3.7b oracle not found)"
    return
  fi
  assert_tmux_authority_version

  local sock="$SMOKE_ROOT/tmux.sock"
  "$TMUX_BIN" -f /dev/null -S "$sock" kill-server >/dev/null 2>&1 || true
  cleanup_tmux() {
    "$TMUX_BIN" -f /dev/null -S "$sock" kill-server >/dev/null 2>&1 || true
  }
  trap cleanup_tmux RETURN

  "$TMUX_BIN" -f /dev/null -S "$sock" new-session -d -s alpha sleep 60
  "$TMUX_BIN" -f /dev/null -S "$sock" split-window -h -v -t alpha:0 sleep 60
  "$TMUX_BIN" -f /dev/null -S "$sock" resize-pane -R -x 80 -t alpha:0.0
  "$TMUX_BIN" -f /dev/null -S "$sock" resize-pane -Z -R -t alpha:0.0
  "$TMUX_BIN" -f /dev/null -S "$sock" resize-pane -R -L -t alpha:0.0
  "$TMUX_BIN" -f /dev/null -S "$sock" new-session -d -s beta -s gamma sleep 1
  "$TMUX_BIN" -f /dev/null -S "$sock" send-keys -t alpha:0.0 -t alpha:0.1 Enter
  "$TMUX_BIN" -f /dev/null -S "$sock" capture-pane -p -S 0 -S 1 >/dev/null
  [ "$("$TMUX_BIN" -f /dev/null -S "$sock" display-message -p -F a -F b)" = "b" ] ||
    die "tmux duplicate -F authority check did not use last value"
  set +e
  "$TMUX_BIN" -f /dev/null -S "$sock" --version >/dev/null 2>"$SMOKE_ROOT/tmux-version.err"
  local version_status=$?
  set -e
  [ "$version_status" -ne 0 ] || die "tmux accepted --version unexpectedly"

  set +e
  "$TMUX_BIN" -f /dev/null -S "$sock" resize-pane -R 5 -t alpha:0.0 >/dev/null 2>"$SMOKE_ROOT/tmux-resize.err"
  local status=$?
  set -e
  [ "$status" -ne 0 ] || die "tmux accepted resize-pane -R 5 -t target unexpectedly"
  cleanup_tmux
  trap - RETURN
}

run_rmux_smoke() {
  local sock="tiny-review-$$"
  local cold_new_sock="tiny-cold-new-$$"
  local cold_start_sock="tiny-cold-start-$$"
  local cold_attach_sock="tiny-cold-attach-$$"
  local cold_alias_config="$XDG_CONFIG_HOME/rmux/rmux.conf"
  local real="$SMOKE_ROOT/real.sock"
  local link="$SMOKE_ROOT/link.sock"
  local missing="$SMOKE_ROOT/missing.conf"
  local source_file="$SMOKE_ROOT/source.conf"

  "$RMUX" -L "$sock" kill-server >/dev/null 2>&1 || true
  "$RMUX" -S "$real" kill-server >/dev/null 2>&1 || true
  cleanup_rmux() {
    "$RMUX" -L "$sock" set-option -s -u 'command-alias[20]' >/dev/null 2>&1 || true
    "$RMUX" -L "$sock" kill-server >/dev/null 2>&1 || true
    "$RMUX" -L "$cold_new_sock" kill-server >/dev/null 2>&1 || true
    "$RMUX" -L "$cold_start_sock" kill-server >/dev/null 2>&1 || true
    "$RMUX" -L "$cold_attach_sock" kill-server >/dev/null 2>&1 || true
    "$RMUX" -S "$real" kill-server >/dev/null 2>&1 || true
    rm -f "$cold_alias_config"
  }
  trap cleanup_rmux RETURN

  mkdir -p "$(dirname "$cold_alias_config")"
  cat >"$cold_alias_config" <<'EOF'
set-option -s command-alias[30] 'new-session=new-session -d -s tiny-cold-new-aliased'
set-option -s command-alias[31] 'start-server=new-session -d -s tiny-cold-start-aliased'
set-option -s command-alias[32] 'attach-session=new-session -d -s tiny-cold-attach-aliased'
EOF

  run_capture cold_new_alias -L "$cold_new_sock" new-session -d
  assert_rc cold_new_alias 0
  assert_trace cold_new_alias "rmux tiny: fallback: runtime command-alias"
  [ "$("$RMUX" -L "$cold_new_sock" list-sessions -F '#{session_name}')" = \
    "tiny-cold-new-aliased" ] || die "cold tiny new-session bypassed its config alias"

  run_capture cold_start_alias -L "$cold_start_sock" start-server
  assert_rc cold_start_alias 0
  assert_trace cold_start_alias "rmux tiny: fallback: runtime command-alias"
  [ "$("$RMUX" -L "$cold_start_sock" list-sessions -F '#{session_name}')" = \
    "tiny-cold-start-aliased" ] || die "cold tiny start-server bypassed its config alias"

  run_capture cold_attach_alias -L "$cold_attach_sock" attach-session
  assert_rc cold_attach_alias 0
  assert_trace cold_attach_alias "rmux tiny: fallback: runtime command-alias"
  [ "$("$RMUX" -L "$cold_attach_sock" list-sessions -F '#{session_name}')" = \
    "tiny-cold-attach-aliased" ] || die "cold tiny attach-session bypassed its config alias"

  rm -f "$cold_alias_config"

  "$RMUX" -L "$sock" new-session -d -s alpha sleep 60 >/dev/null
  "$RMUX" -L "$sock" new-session -d -s prefixdemo sleep 60 >/dev/null
  "$RMUX" -L "$sock" new-session -d -s killtarget sleep 60 >/dev/null

  "$RMUX" -L "$sock" set-environment -gu TINY_ALIAS_PERSISTED >/dev/null 2>&1 || true
  "$RMUX" -L "$sock" set-option -s 'command-alias[20]' \
    'list-sessions=TINY_ALIAS_PERSISTED=from-tiny display-message -p tiny-alias' >/dev/null
  run_capture list_sessions_alias -L "$sock" list-sessions
  assert_rc list_sessions_alias 0
  assert_trace list_sessions_alias "rmux tiny: fallback: runtime command-alias"
  assert_stdout_line list_sessions_alias "tiny-alias"
  [ "$("$RMUX" -L "$sock" show-environment -g TINY_ALIAS_PERSISTED)" = \
      "TINY_ALIAS_PERSISTED=from-tiny" ] ||
    die "tiny runtime alias did not persist its parse-time assignment"
  "$RMUX" -L "$sock" set-environment -gu TINY_ALIAS_PERSISTED >/dev/null
  "$RMUX" -L "$sock" set-option -s -u 'command-alias[20]' >/dev/null

  "$RMUX" -L "$sock" set-environment -gu TINY_ALIAS_ATOMIC >/dev/null 2>&1 || true
  "$RMUX" -L "$sock" set-option -s 'command-alias[20]' \
    'list-sessions=TINY_ALIAS_ATOMIC=mutated kill-server unexpected' >/dev/null
  run_capture list_sessions_alias_invalid -L "$sock" list-sessions
  [ "$(cat "$SMOKE_ROOT/list_sessions_alias_invalid.rc")" != "0" ] ||
    die "invalid tiny runtime alias unexpectedly succeeded"
  assert_trace list_sessions_alias_invalid \
    "rmux tiny: fallback: runtime command-alias"
  if "$RMUX" -L "$sock" show-environment -g TINY_ALIAS_ATOMIC >/dev/null 2>&1; then
    die "invalid tiny runtime alias applied assignments before validation"
  fi
  "$RMUX" -L "$sock" has-session -t alpha >/dev/null ||
    die "invalid tiny runtime alias stopped the server"
  "$RMUX" -L "$sock" set-option -s -u 'command-alias[20]' >/dev/null

  "$RMUX" -L "$sock" set-option -s 'command-alias[20]' \
    'kill-server=display-message -p alias-survives' >/dev/null
  run_capture kill_server_alias -L "$sock" kill-server
  assert_rc kill_server_alias 0
  assert_trace kill_server_alias "rmux tiny: fallback: runtime command-alias"
  assert_stdout_line kill_server_alias "alias-survives"
  "$RMUX" -L "$sock" list-sessions >/dev/null ||
    die "tiny kill-server alias stopped the server"
  "$RMUX" -L "$sock" set-option -s -u 'command-alias[20]' >/dev/null

  run_capture has_session_prefix -L "$sock" has-session -t prefixd
  assert_rc has_session_prefix 0
  assert_trace has_session_prefix "rmux tiny: direct: has-session"

  run_capture list_windows_prefix -L "$sock" list-windows -t prefixd
  assert_rc list_windows_prefix 0
  assert_trace list_windows_prefix "rmux tiny: direct: list-windows"
  assert_stdout_not_empty list_windows_prefix

  run_capture list_panes_prefix -L "$sock" list-panes -t prefixd
  assert_rc list_panes_prefix 0
  assert_trace list_panes_prefix "rmux tiny: direct: list-panes"
  assert_stdout_not_empty list_panes_prefix

  run_capture display_message_prefix -L "$sock" display-message -p -t prefixd '#{session_name}'
  assert_rc display_message_prefix 0
  assert_trace display_message_prefix "rmux tiny: direct: display-message"
  assert_stdout_line display_message_prefix "prefixdemo"

  "$RMUX" -L "$sock" set-buffer -b tiny-send-hook seed >/dev/null
  "$RMUX" -L "$sock" set-hook -g command-error \
    'set-buffer -a -b tiny-send-hook x' >/dev/null

  run_capture send_keys_prefix -L "$sock" send-keys -t prefixd:0.0 C-l
  assert_rc send_keys_prefix 0
  assert_trace send_keys_prefix "rmux tiny: direct: send-keys"
  [ "$("$RMUX" -L "$sock" show-buffer -b tiny-send-hook)" = "seed" ] ||
    die "successful tiny send-keys prefix resolution emitted command-error"

  run_capture send_keys_missing -L "$sock" send-keys -t absent:0.0 C-l
  [ "$(cat "$SMOKE_ROOT/send_keys_missing.rc")" != "0" ] ||
    die "missing tiny send-keys target unexpectedly succeeded"
  assert_trace send_keys_missing "rmux tiny: direct: send-keys"
  [ "$("$RMUX" -L "$sock" show-buffer -b tiny-send-hook)" = "seedx" ] ||
    die "failed tiny send-keys did not emit command-error exactly once"
  "$RMUX" -L "$sock" set-hook -gu command-error >/dev/null

  run_capture if_shell_prefix -L "$sock" if-shell -F -t prefixd \
    '#{==:#{session_name},prefixdemo}' \
    'display-message -p target-resolved' \
    'display-message -p wrong-target'
  assert_rc if_shell_prefix 0
  assert_trace if_shell_prefix "rmux tiny: fallback: unsupported invocation"
  assert_stdout_line if_shell_prefix "target-resolved"

  run_capture kill_session_prefix -L "$sock" kill-session -t killtar
  assert_rc kill_session_prefix 0
  assert_trace kill_session_prefix "rmux tiny: direct: kill-session"

  local before after width_before width_after
  before="$("$RMUX" -L "$sock" list-panes -t alpha | wc -l | tr -d ' ')"
  run_capture split_lastwins -L "$sock" split-window -h -v -t alpha:0 sleep 60
  assert_rc split_lastwins 0
  assert_trace split_lastwins "rmux tiny: direct: split-window"
  after="$("$RMUX" -L "$sock" list-panes -t alpha | wc -l | tr -d ' ')"
  [ "$after" -gt "$before" ] || die "split-window -h -v did not create a pane"

  run_capture full_split_lastwins -L "$sock" split-window -h -v -t alpha:0 sleep 60
  assert_rc full_split_lastwins 0
  RMUX_DISABLE_TINY_CLI=1 RMUX_TINY_TRACE=1 \
    "$RMUX" -L "$sock" split-window -v -h -t alpha:0 sleep 60 \
    >"$SMOKE_ROOT/full_split_reverse.out" 2>"$SMOKE_ROOT/full_split_reverse.err" ||
    die "full helper rejected tmux-compatible split-window -v -h"

  run_capture capture_lastwins -L "$sock" capture-pane -p -S 0 -S 1
  assert_rc capture_lastwins 0
  assert_trace capture_lastwins "rmux tiny: direct: capture-pane"

  RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" capture-pane -p -t alpha:0.0 -S 0 -S 1 \
    >"$SMOKE_ROOT/full_capture_lastwins.out" 2>"$SMOKE_ROOT/full_capture_lastwins.err" ||
    die "full helper rejected tmux-compatible capture-pane duplicate bounds"

  run_capture display_lastwins -L "$sock" display-message -p -F a -F b
  assert_rc display_lastwins 0
  assert_trace display_lastwins "rmux tiny: direct: display-message"
  assert_stdout_line display_lastwins "b"

  [ "$(RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" display-message -p -F a -F b)" = "b" ] ||
    die "full helper did not apply display-message duplicate -F last-wins"

  run_capture display_missing_target -L "$sock" display-message -p -t missing '#{session_name}'
  assert_rc display_missing_target 0
  assert_trace display_missing_target "rmux tiny: direct: display-message"
  assert_stdout_line display_missing_target ""
  assert_stderr_has_no_user_output_except_trace \
    display_missing_target "rmux tiny: direct: display-message"
  RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" display-message -p -t missing '#{session_name}' \
    >"$SMOKE_ROOT/full_display_missing_target.out" 2>"$SMOKE_ROOT/full_display_missing_target.err" ||
    die "full helper rejected tmux-compatible display-message missing target empty context"
  grep -Fqx "" "$SMOKE_ROOT/full_display_missing_target.out" ||
    die "full display-message missing target should emit one empty tmux-compatible line"
  [ ! -s "$SMOKE_ROOT/full_display_missing_target.err" ] ||
    die "full display-message missing target unexpectedly wrote stderr"

  run_capture new_session_repeated_name -L "$sock" new-session -d -s beta -s gamma sleep 1
  assert_rc new_session_repeated_name 0
  assert_trace new_session_repeated_name "rmux tiny: direct: new-session"
  RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" new-session -d -s delta -s epsilon sleep 1 \
    >"$SMOKE_ROOT/full_new_session_repeat.out" 2>"$SMOKE_ROOT/full_new_session_repeat.err" ||
    die "full helper rejected tmux-compatible repeated new-session -s"

  run_capture send_keys_repeated_target -L "$sock" send-keys -t alpha:0.0 -t alpha:0.1 Enter
  assert_rc send_keys_repeated_target 0
  assert_trace send_keys_repeated_target "rmux tiny: direct: send-keys"
  RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" send-keys -t alpha:0.0 -t alpha:0.1 Enter \
    >"$SMOKE_ROOT/full_send_keys_repeat.out" 2>"$SMOKE_ROOT/full_send_keys_repeat.err" ||
    die "full helper rejected tmux-compatible repeated send-keys -t"

  width_before="$("$RMUX" -L "$sock" display-message -p -t alpha:0.0 '#{pane_width}')"
  run_capture resize_bad -L "$sock" resize-pane -R 5 -t alpha:0.0
  [ "$(cat "$SMOKE_ROOT/resize_bad.rc")" != "0" ] || die "invalid resize-pane succeeded"
  assert_trace resize_bad "rmux tiny: fallback: unsupported invocation"
  width_after="$("$RMUX" -L "$sock" display-message -p -t alpha:0.0 '#{pane_width}')"
  [ "$width_before" = "$width_after" ] || die "invalid resize-pane mutated width"

  run_capture resize_good -L "$sock" resize-pane -t alpha:0.0 -R 5
  assert_rc resize_good 0
  assert_trace resize_good "rmux tiny: direct: resize-pane"

  for prefix in resize_valueless_relative resize_zoom_relative; do
    case "$prefix" in
      resize_valueless_relative)
        run_capture "$prefix" -L "$sock" resize-pane -R -L -t alpha:0.0
        ;;
      resize_zoom_relative)
        run_capture "$prefix" -L "$sock" resize-pane -Z -R -t alpha:0.0
        ;;
    esac
    assert_rc "$prefix" 0
    assert_trace "$prefix" "rmux tiny: direct: resize-pane"
  done
  run_capture resize_relative_absolute -L "$sock" resize-pane -R -x 80 -t alpha:0.0
  assert_rc resize_relative_absolute 0
  assert_trace resize_relative_absolute "rmux tiny: direct: resize-pane"
  RMUX_DISABLE_TINY_CLI=1 "$RMUX" -L "$sock" resize-pane -R -L -t alpha:0.0 \
    >"$SMOKE_ROOT/full_resize_lastwins.out" 2>"$SMOKE_ROOT/full_resize_lastwins.err" ||
    die "full helper rejected tmux-compatible resize-pane valueless adjustment last-wins"

  run_capture queue_new -L "$sock" new-session -d -s q sleep 2 ';' display-message -p hi
  assert_rc queue_new 0
  assert_trace queue_new "rmux tiny: fallback: unsupported invocation"
  assert_stdout_line queue_new "hi"

  run_capture queue_suffix -L "$sock" split-window -h -t alpha:0 'echo;' list-sessions
  assert_rc queue_suffix 0
  assert_trace queue_suffix "rmux tiny: fallback: unsupported invocation"
  assert_stdout_contains queue_suffix "alpha:"

  run_capture target_error_surface -L "$sock" split-window -h -t alpha:0.99 /bin/sh
  [ "$(cat "$SMOKE_ROOT/target_error_surface.rc")" != "0" ] ||
    die "missing target split unexpectedly succeeded"
  grep -Fq "can't find pane: 99" "$SMOKE_ROOT/target_error_surface.err" ||
    die "tiny target error did not use tmux-style target message"
  ! grep -Fq "split-window: invalid target" "$SMOKE_ROOT/target_error_surface.err" ||
    die "tiny target error leaked command-prefixed invalid target message"

  run_capture long_version --version
  [ "$(cat "$SMOKE_ROOT/long_version.rc")" != "0" ] || die "--version unexpectedly succeeded"
  assert_trace long_version "rmux tiny: fallback: unsupported invocation"
  grep -Fq "usage: rmux" "$SMOKE_ROOT/long_version.err" ||
    die "--version fallback did not print tmux-compatible usage"

  cat >"$source_file" <<EOF
source-file $missing
display-message -p after
EOF
  run_capture source_status -L "$sock" source-file "$source_file"
  assert_rc source_status 1
  assert_trace source_status "rmux tiny: direct: source-file"
  assert_stderr_contains source_status "$missing"
  assert_stdout_line source_status "after"

  "$RMUX" -S "$real" new-session -d -s base sleep 60 >/dev/null
  ln -s "$real" "$link"
  run_capture symlink_new -S "$link" new-session -d -s via_link sleep 1
  [ "$(cat "$SMOKE_ROOT/symlink_new.rc")" != "0" ] || die "symlink startup succeeded"
  grep -Fq "refused to follow symlink" "$SMOKE_ROOT/symlink_new.err" ||
    die "symlink startup did not report refusal"

  local empty="tiny-empty-$$"
  run_capture empty_attach -L "$empty" attach-session
  [ "$(cat "$SMOKE_ROOT/empty_attach.rc")" != "0" ] || die "empty attach succeeded"
  grep -Fq "no sessions" "$SMOKE_ROOT/empty_attach.err" ||
    die "empty attach did not report no sessions"
  if "$RMUX" -L "$empty" list-sessions >/dev/null 2>&1; then
    "$RMUX" -L "$empty" kill-server >/dev/null 2>&1 || true
    die "empty attach left a usable daemon"
  fi

  set +e
  RMUX_DISABLE_CLI_TARGET_ACTIONS=1 RMUX_TINY_TRACE=1 \
    "$RMUX" -L "$sock" capture-pane -p >"$SMOKE_ROOT/disable.out" 2>"$SMOKE_ROOT/disable.err"
  local disabled_status=$?
  set -e
  [ "$disabled_status" -eq 0 ] || die "RMUX_DISABLE_CLI_TARGET_ACTIONS capture failed"
  grep -Fq "rmux tiny: fallback: unsupported invocation" "$SMOKE_ROOT/disable.err" ||
    die "target action kill-switch did not force fallback"

  cleanup_rmux
  trap - RETURN
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
layout=""
target_dir=""
skip_build=0
SKIP_TMUX=0
TMUX_BIN=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --layout)
      [ "$#" -ge 2 ] || die "--layout requires a directory"
      layout="$2"
      shift 2
      ;;
    --target-dir)
      [ "$#" -ge 2 ] || die "--target-dir requires a directory"
      target_dir="$2"
      shift 2
      ;;
    --skip-build)
      skip_build=1
      shift
      ;;
    --tmux)
      [ "$#" -ge 2 ] || die "--tmux requires a path"
      TMUX_BIN="$2"
      shift 2
      ;;
    --no-tmux)
      SKIP_TMUX=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

cd "$repo_root"

resolve_tmux_authority

if [ -z "$layout" ]; then
  layout="$(mktemp -d "${TMPDIR:-/tmp}/rmux-review-layout.XXXXXX")"
fi
case "$layout" in
  /*) ;;
  *) layout="$repo_root/$layout" ;;
esac

if [ "$skip_build" -eq 1 ]; then
  [ -x "$layout/bin/rmux" ] || die "--skip-build requires $layout/bin/rmux"
  [ -x "$layout/libexec/rmux/rmux" ] || die "--skip-build requires $layout/libexec/rmux/rmux"
  [ -x "$layout/bin/rmux-daemon" ] || die "--skip-build requires $layout/bin/rmux-daemon"
else
  if [ -z "$target_dir" ]; then
    target_dir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-review-target.XXXXXX")"
  fi
  case "$target_dir" in
    /*) ;;
    *) target_dir="$repo_root/$target_dir" ;;
  esac
  build_layout "$target_dir" "$layout"
fi

RMUX="$layout/bin/rmux"
RMUX_BINARY="$RMUX"
unset RMUX TMUX RMUX_PANE TMUX_PANE TERM_PROGRAM TERM_PROGRAM_VERSION
RMUX="$RMUX_BINARY"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-tiny-review.XXXXXX")"
export HOME="$SMOKE_ROOT/home"
export XDG_CONFIG_HOME="$SMOKE_ROOT/config"
mkdir -p "$HOME" "$XDG_CONFIG_HOME"

cleanup() {
  rm -rf "$SMOKE_ROOT"
}
trap cleanup EXIT

scripts/check-release-versions.sh --binary "$RMUX" >/dev/null
run_tmux_authority_smoke
run_rmux_smoke

printf 'layout=%s\n' "$layout"
printf 'tiny-smoke=ok\n'
