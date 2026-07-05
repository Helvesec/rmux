#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

failures=0

report_failure() {
  printf 'worktree hygiene: %s\n' "$*" >&2
  failures=$((failures + 1))
}

tracked_dotfiles="$(git ls-files '.claude' '.claude/**' '.codex' '.codex/**')"
if [ -n "$tracked_dotfiles" ]; then
  report_failure "tracked local assistant metadata is forbidden:"
  printf '%s\n' "$tracked_dotfiles" >&2
fi

tracked_deploy_dirs="$(git ls-files '.release-deployment' '.release-deployment/**' '.rmux-audit' '.rmux-audit/**' 'dist' 'dist/**')"
if [ -n "$tracked_deploy_dirs" ]; then
  report_failure "tracked local deployment artifacts are forbidden:"
  printf '%s\n' "$tracked_deploy_dirs" >&2
fi

untracked_sockets="$(git ls-files --others --exclude-standard | grep -E '\.(sock|socket)$' || true)"
if [ -n "$untracked_sockets" ]; then
  report_failure "untracked socket files are forbidden in the worktree:"
  printf '%s\n' "$untracked_sockets" >&2
fi

filesystem_sockets="$(
  find . \
    \( -path ./.git -o -path ./target -o -path './target-*' \) -prune -o \
    -type s \( -name '*.sock' -o -name '*.socket' \) \
    -print
)"
if [ -n "$filesystem_sockets" ]; then
  report_failure "socket files are forbidden in the worktree:"
  printf '%s\n' "$filesystem_sockets" >&2
fi

if [ "${RMUX_STRICT_WORKTREE_HYGIENE:-0}" = "1" ]; then
  strict_matches="$(
    find . \
      \( -path ./.git -o -path ./target -o -path './target-*' \) -prune -o \
      \( -name '*.sock' -o -name '*.socket' -o -name '.claude' -o -name '.codex' -o -name '.release-deployment' -o -name '.rmux-audit' \) \
      -print
  )"
  if [ -n "$strict_matches" ]; then
    report_failure "strict hygiene found local-only artifacts:"
    printf '%s\n' "$strict_matches" >&2
  fi
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi

printf 'worktree-hygiene=ok\n'
