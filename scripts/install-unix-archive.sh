#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: ./install.sh [options]

Install an extracted RMUX Unix archive while preserving the tiny/full layout.

Options:
  --prefix DIR   Installation prefix (default: $RMUX_INSTALL_PREFIX or ~/.local)
  --no-verify    Skip the post-install CLI layout smoke
  -h, --help     Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

archive_root() {
  local script_dir
  script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" && pwd)"
  printf '%s\n' "$script_dir"
}

same_file_state() {
  local left right
  left="$1"
  right="$2"

  if [ -L "$left" ] || [ -L "$right" ]; then
    [ -L "$left" ] && [ -L "$right" ] &&
      [ "$(readlink "$left")" = "$(readlink "$right")" ]
  else
    [ -f "$left" ] && [ -f "$right" ] && cmp -s "$left" "$right"
  fi
}

checkpoint() {
  local name
  name="$1"
  if [ "${RMUX_INSTALL_TEST_FAIL_AT:-}" = "$name" ]; then
    die "injected installer failure at $name"
  fi
  if [ "${RMUX_INSTALL_TEST_SIGNAL_AT:-}" = "$name" ]; then
    kill -TERM "$$"
    die "injected installer signal was not delivered at $name"
  fi
}

verify_layout() {
  local rmux stdout stderr status
  rmux="$1"
  stdout="$(mktemp "${TMPDIR:-/tmp}/rmux-install-stdout.XXXXXX")"
  stderr="$(mktemp "${TMPDIR:-/tmp}/rmux-install-stderr.XXXXXX")"
  if "$rmux" --help >"$stdout" 2>"$stderr"; then
    status=0
  else
    status=$?
  fi
  if { [ "$status" -ne 0 ] && [ "$status" -ne 1 ]; } || ! grep -q 'usage: rmux' "$stdout" "$stderr"; then
    cat "$stderr" >&2
    rm -f "$stdout" "$stderr"
    die "installed rmux could not reach its full CLI helper"
  fi
  rm -f "$stdout" "$stderr"
}

prefix="${RMUX_INSTALL_PREFIX:-${HOME:-}/.local}"
verify=1

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a value"
      prefix="$2"
      shift 2
      ;;
    --no-verify)
      verify=0
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

[ -n "$prefix" ] || die "prefix must not be empty"
case "$prefix" in
  /*) ;;
  *) prefix="$(pwd)/$prefix" ;;
esac
root="$(archive_root)"

for required in bin/rmux bin/rmux-daemon libexec/rmux/rmux; do
  [ -f "$root/$required" ] && [ -x "$root/$required" ] ||
    die "archive is missing executable $required"
done

sources=(
  "$root/libexec/rmux/rmux"
  "$root/bin/rmux-daemon"
  "$root/bin/rmux"
)
targets=(
  "$prefix/libexec/rmux/rmux"
  "$prefix/bin/rmux-daemon"
  "$prefix/bin/rmux"
)
labels=(helper daemon tiny)
staged_paths=("" "" "")
backup_paths=("" "" "")
target_existed=(0 0 0)
asset_targets=()
asset_staged_paths=()
asset_backup_paths=()
asset_target_existed=()
created_dirs=()
transaction_root=""
transaction_active=0
transaction_committed=0
install_lock_path=""
install_lock_nonce=""
install_lock_held=0
prefix_created=0

ensure_directory() {
  local directory
  directory="$1"
  if [ -e "$directory" ] || [ -L "$directory" ]; then
    [ -d "$directory" ] || die "install path exists but is not a directory: $directory"
    return
  fi
  mkdir "$directory"
  created_dirs+=("$directory")
}

ensure_directory_tree() {
  local directory index
  local missing=()
  directory="$1"
  while [ ! -e "$directory" ] && [ ! -L "$directory" ]; do
    missing+=("$directory")
    directory="$(dirname "$directory")"
  done
  [ -d "$directory" ] || die "install path exists but is not a directory: $directory"
  for ((index = ${#missing[@]} - 1; index >= 0; index--)); do
    mkdir "${missing[$index]}"
    created_dirs+=("${missing[$index]}")
  done
}

ensure_install_prefix() {
  if [ -e "$prefix" ] || [ -L "$prefix" ]; then
    [ -d "$prefix" ] || die "prefix exists but is not a directory: $prefix"
    return
  fi
  mkdir -p "$prefix"
  if [ "$prefix_created" -eq 0 ]; then
    created_dirs+=("$prefix")
    prefix_created=1
  fi
}

read_install_lock_owner() {
  local owner_file extra
  owner_file="$1/owner"
  lock_owner_pid=""
  lock_owner_nonce=""

  [ -f "$owner_file" ] && [ ! -L "$owner_file" ] || return 1
  {
    IFS= read -r lock_owner_pid &&
      IFS= read -r lock_owner_nonce &&
      ! IFS= read -r extra
  } <"$owner_file" || return 1
  case "$lock_owner_pid" in
    ''|*[!0-9]*) return 1 ;;
  esac
  case "$lock_owner_nonce" in
    ''|*[!A-Za-z0-9._-]*) return 1 ;;
  esac
}

reclaim_stale_install_lock() {
  local expected_pid expected_nonce reclaim_path quarantine suffix
  expected_pid="$1"
  expected_nonce="$2"
  reclaim_path="$install_lock_path/.reclaim"

  (umask 077 && mkdir "$reclaim_path") 2>/dev/null || return 1

  # A waiter may only move the exact stale lock that it inspected before
  # winning the reclaim marker. A live owner always keeps its lock.
  if ! read_install_lock_owner "$install_lock_path" ||
    [ "$lock_owner_pid" != "$expected_pid" ] ||
    [ "$lock_owner_nonce" != "$expected_nonce" ] ||
    kill -0 "$lock_owner_pid" 2>/dev/null; then
    rmdir "$reclaim_path" 2>/dev/null || :
    return 1
  fi

  quarantine="${install_lock_path}.quarantine.$$.$RANDOM.$RANDOM"
  suffix=0
  while [ -e "$quarantine" ] || [ -L "$quarantine" ]; do
    suffix=$((suffix + 1))
    quarantine="${install_lock_path}.quarantine.$$.$RANDOM.$suffix"
  done
  if ! mv "$install_lock_path" "$quarantine"; then
    rmdir "$reclaim_path" 2>/dev/null || :
    return 1
  fi
  rm -rf "$quarantine" ||
    die "failed to clean stale installer lock quarantine at $quarantine"
}

write_install_lock_owner() {
  local owner_file owner_tmp
  owner_file="$install_lock_path/owner"
  owner_tmp="$install_lock_path/.owner.$$"
  install_lock_nonce="$$.$SECONDS.$RANDOM.$RANDOM"
  if ! (umask 077 && printf '%s\n%s\n' "$$" "$install_lock_nonce" >"$owner_tmp") ||
    ! mv "$owner_tmp" "$owner_file"; then
    rm -f "$owner_tmp" 2>/dev/null || :
    rm -f "$owner_file" 2>/dev/null || :
    rmdir "$install_lock_path" 2>/dev/null || :
    return 1
  fi
}

acquire_install_lock() {
  local deadline reported_wait stale_pid stale_nonce
  install_lock_path="$prefix/.rmux-install.lock"
  deadline=$((SECONDS + 30))
  reported_wait=0

  while ! (umask 077 && mkdir "$install_lock_path") 2>/dev/null; do
    if [ -L "$install_lock_path" ] ||
      { [ -e "$install_lock_path" ] && [ ! -d "$install_lock_path" ]; }; then
      die "installer lock path exists but is not a directory: $install_lock_path"
    fi
    if [ ! -d "$prefix" ]; then
      ensure_install_prefix
    fi
    if read_install_lock_owner "$install_lock_path"; then
      stale_pid="$lock_owner_pid"
      stale_nonce="$lock_owner_nonce"
      if ! kill -0 "$stale_pid" 2>/dev/null; then
        if reclaim_stale_install_lock "$stale_pid" "$stale_nonce"; then
          continue
        fi
      fi
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      die "timed out waiting for another rmux installation to finish"
    fi
    if [ "$reported_wait" -eq 0 ]; then
      if [ -n "${RMUX_INSTALL_TEST_LOCK_WAIT_FILE:-}" ]; then
        : >"$RMUX_INSTALL_TEST_LOCK_WAIT_FILE" 2>/dev/null || :
      fi
      printf 'Another rmux installation is updating this destination; waiting up to 30 seconds.\n' >&2
      reported_wait=1
    fi
    sleep 0.05
  done
  write_install_lock_owner || die "could not record installer lock ownership"
  install_lock_held=1

  if [ -n "${RMUX_INSTALL_TEST_LOCK_ACQUIRED_FILE:-}" ]; then
    : >"$RMUX_INSTALL_TEST_LOCK_ACQUIRED_FILE"
  fi
  if [ -n "${RMUX_INSTALL_TEST_LOCK_RESUME_FILE:-}" ]; then
    while [ ! -e "$RMUX_INSTALL_TEST_LOCK_RESUME_FILE" ]; do
      sleep 0.01
    done
  fi
}

release_install_lock() {
  [ "$install_lock_held" -eq 1 ] || return 0
  read_install_lock_owner "$install_lock_path" || return 1
  [ "$lock_owner_pid" = "$$" ] || return 1
  [ "$lock_owner_nonce" = "$install_lock_nonce" ] || return 1
  rm "$install_lock_path/owner" || return 1
  rmdir "$install_lock_path" || return 1
  install_lock_held=0
}

restore_target() {
  local index target backup target_dir tmp
  index="$1"
  target="${targets[$index]}"
  backup="${backup_paths[$index]}"

  if [ "${target_existed[$index]}" -eq 0 ]; then
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      return 1
    fi
    rm -f "$target" || return 1
    [ ! -e "$target" ] && [ ! -L "$target" ]
    return 0
  fi

  target_dir="$(dirname "$target")"
  tmp="$(mktemp "$target_dir/.rmux-rollback-${labels[$index]}.XXXXXX")" || return 1
  rm -f "$tmp" || return 1
  if ! cp -pP "$backup" "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  if ! mv -f "$tmp" "$target"; then
    rm -f "$tmp"
    return 1
  fi
  same_file_state "$backup" "$target"
}

restore_asset_target() {
  local index target backup target_dir tmp
  index="$1"
  target="${asset_targets[$index]}"
  backup="${asset_backup_paths[$index]}"

  if [ "${asset_target_existed[$index]}" -eq 0 ]; then
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      return 1
    fi
    rm -f "$target" || return 1
    [ ! -e "$target" ] && [ ! -L "$target" ]
    return 0
  fi

  target_dir="$(dirname "$target")"
  tmp="$(mktemp "$target_dir/.rmux-rollback-asset.XXXXXX")" || return 1
  rm -f "$tmp" || return 1
  if ! cp -pP "$backup" "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  if ! mv -f "$tmp" "$target"; then
    rm -f "$tmp"
    return 1
  fi
  same_file_state "$backup" "$target"
}

rollback_transaction() {
  local index failed
  failed=0
  for ((index = ${#asset_targets[@]} - 1; index >= 0; index--)); do
    if ! restore_asset_target "$index"; then
      printf 'error: failed to restore %s during installer rollback\n' "${asset_targets[$index]}" >&2
      failed=1
    fi
  done
  for ((index = ${#targets[@]} - 1; index >= 0; index--)); do
    if ! restore_target "$index"; then
      printf 'error: failed to restore %s during installer rollback\n' "${targets[$index]}" >&2
      failed=1
    fi
  done
  return "$failed"
}

cleanup_transaction() {
  local status rollback_failed index
  status="$1"
  rollback_failed=0
  trap - EXIT HUP INT TERM
  set +e

  if [ "$transaction_active" -eq 1 ] && [ "$transaction_committed" -eq 0 ]; then
    rollback_transaction || rollback_failed=1
  fi

  for staged in "${staged_paths[@]}"; do
    [ -z "$staged" ] || rm -f "$staged"
  done
  if [ "${#asset_staged_paths[@]}" -gt 0 ]; then
    for staged in "${asset_staged_paths[@]}"; do
      [ -z "$staged" ] || rm -f "$staged"
    done
  fi
  if [ -n "$transaction_root" ] && [ "$rollback_failed" -eq 0 ]; then
    rm -rf "$transaction_root" || rollback_failed=1
  fi

  if [ "$transaction_committed" -eq 0 ]; then
    for ((index = ${#created_dirs[@]} - 1; index >= 0; index--)); do
      if [ "${created_dirs[$index]}" != "$prefix" ]; then
        rmdir "${created_dirs[$index]}" 2>/dev/null || :
      fi
    done
  fi

  if ! release_install_lock; then
    printf 'error: failed to release installer lock at %s\n' "$install_lock_path" >&2
    rollback_failed=1
  fi
  if [ "$transaction_committed" -eq 0 ] && [ "$prefix_created" -eq 1 ]; then
    rmdir "$prefix" 2>/dev/null || :
  fi

  if [ "$rollback_failed" -ne 0 ]; then
    [ -z "$transaction_root" ] ||
      printf 'error: installer recovery data preserved at %s\n' "$transaction_root" >&2
    printf 'error: installer rollback or cleanup was incomplete\n' >&2
    status=1
  fi
  exit "$status"
}

trap 'cleanup_transaction "$?"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Serialize the complete preflight, backup, publish, verification, rollback,
# and cleanup interval. Without this lock, an older failing transaction can
# restore its snapshot over a newer package that already reported success.
ensure_install_prefix
acquire_install_lock

# Reject every unusable destination before creating staging files or replacing
# any executable. Symlinks are allowed because a successful upgrade replaces
# them just as the previous installer did; backups preserve them on rollback.
for target in "${targets[@]}"; do
  if [ -e "$target" ] || [ -L "$target" ]; then
    [ -f "$target" ] || [ -L "$target" ] ||
      die "destination executable path exists but is not a file: $target"
  fi
done
checkpoint after-preflight

ensure_directory "$prefix/bin"
ensure_directory "$prefix/libexec"
ensure_directory "$prefix/libexec/rmux"

transaction_root="$(mktemp -d "$prefix/.rmux-install-transaction.XXXXXX")"

# Stage and snapshot every package-owned asset before replacing any installed
# file. Asset paths share system directories with other packages, so the
# transaction records individual files instead of swapping the whole share
# tree. This preserves unrelated files while still allowing a complete
# rollback after a partial asset install.
if [ -d "$root/share" ]; then
  while IFS= read -r -d '' source; do
    relative="${source#"$root/share/"}"
    target="$prefix/share/$relative"
    target_dir="$(dirname "$target")"
    ensure_directory_tree "$target_dir"
    if [ -e "$target" ] || [ -L "$target" ]; then
      [ -f "$target" ] || [ -L "$target" ] ||
        die "destination asset path exists but is not a file: $target"
    fi

    index="${#asset_targets[@]}"
    asset_targets+=("$target")
    asset_staged_paths[$index]="$(mktemp "$target_dir/.rmux-stage-asset.XXXXXX")"
    rm -f "${asset_staged_paths[$index]}"
    cp -pP "$source" "${asset_staged_paths[$index]}"
    same_file_state "$source" "${asset_staged_paths[$index]}" ||
      die "could not verify staged asset for $target"

    asset_backup_paths[$index]="$transaction_root/backup-asset-$index"
    asset_target_existed[$index]=0
    if [ -e "$target" ] || [ -L "$target" ]; then
      asset_target_existed[$index]=1
      cp -pP "$target" "${asset_backup_paths[$index]}"
      same_file_state "$target" "${asset_backup_paths[$index]}" ||
        die "could not verify backup for $target"
    fi
  done < <(find "$root/share" \( -type f -o -type l \) -print0)
fi

# Stage the entire new executable set before the first destination mutation.
# Each staging file lives beside its destination, keeping the later rename
# atomic even when a nested install directory is a separate mount point.
for ((index = 0; index < ${#sources[@]}; index++)); do
  target_dir="$(dirname "${targets[$index]}")"
  staged_paths[$index]="$(mktemp "$target_dir/.rmux-stage-${labels[$index]}.XXXXXX")"
  install -m 0755 "${sources[$index]}" "${staged_paths[$index]}"
  checkpoint "after-stage-${labels[$index]}"
done

# Snapshot every old destination before any replacement. cp -pP preserves both
# executable modes and symlinks, and the comparison catches a truncated backup.
for ((index = 0; index < ${#targets[@]}; index++)); do
  target="${targets[$index]}"
  backup_paths[$index]="$transaction_root/backup-${labels[$index]}"
  if [ -e "$target" ] || [ -L "$target" ]; then
    target_existed[$index]=1
    cp -pP "$target" "${backup_paths[$index]}"
    same_file_state "$target" "${backup_paths[$index]}" ||
      die "could not verify backup for $target"
  fi
  checkpoint "after-backup-${labels[$index]}"
done

# From this point through layout verification, every error or handled signal
# restores the complete previous executable set (or removes a fresh partial
# set). The tiny dispatcher remains last, but rollback no longer relies on that
# ordering for consistency.
transaction_active=1
for ((index = 0; index < ${#targets[@]}; index++)); do
  mv -f "${staged_paths[$index]}" "${targets[$index]}"
  staged_paths[$index]=""
  checkpoint "after-replace-${labels[$index]}"
done

if [ "$verify" -eq 1 ]; then
  verify_layout "$prefix/bin/rmux"
  checkpoint after-verify
fi

# Publish staged assets only while the executable rollback remains armed. A
# failure or handled signal after any individual replacement restores both the
# previous binaries and every package-owned asset.
for ((index = 0; index < ${#asset_targets[@]}; index++)); do
  mv -f "${asset_staged_paths[$index]}" "${asset_targets[$index]}"
  asset_staged_paths[$index]=""
  checkpoint after-replace-asset
done

transaction_committed=1
printf 'rmux installed to %s\n' "$prefix/bin/rmux"
