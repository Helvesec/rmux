#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: ./install.sh [options]

Install an extracted RMUX Unix archive while preserving the tiny/full layout.

Options:
  --prefix DIR   Installation prefix (default: $RMUX_INSTALL_PREFIX or ~/.local)
  --no-verify    Skip the post-install CLI layout smoke
  --previous-package-root DIR  Verify the installed package before version rollback
  --uninstall   Remove this archive's verified installed files (explicit --prefix)
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

# The permission bits of one file, in octal. GNU stat answers -c, BSD stat
# answers -f, and GNU stat accepts -f for something else entirely, so the
# answer is only taken when it really is a mode.
stat_mode() {
  local mode
  mode="$(stat -c '%a' "$1" 2>/dev/null || true)"
  case "$mode" in
    ''|*[!0-7]*) mode="$(stat -f '%Lp' "$1" 2>/dev/null || true)" ;;
  esac
  case "$mode" in
    ''|*[!0-7]*) die "could not read the mode of $1" ;;
  esac
  printf '%s' "$mode"
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

assert_owned_file() {
  local source target installed_mode owned_path boundary
  source="$1" target="$2" installed_mode="$3"
  for owned_path in "$source" "$target"; do
    boundary="$(dirname "${previous_root:-$root}")"
    case "$owned_path" in "$prefix"/*) boundary="$(dirname "$prefix")" ;; esac
    while [ "$owned_path" != "$boundary" ] && [ "$owned_path" != / ]; do
      [ ! -L "$owned_path" ] || die "owned path is a symbolic link: $owned_path"
      owned_path="$(dirname "$owned_path")"
    done
  done
  [ -f "$target" ] && cmp -s "$source" "$target" &&
    [ "$installed_mode" = "$(stat_mode "$target")" ] ||
    die "installed package identity differs: $target"
}

assert_new_destinations_owned() {
  local destination owned_path owned target
  for destination in "${new_destinations[@]}"; do
    owned_path="$destination"
    while [ "$owned_path" != "$prefix" ]; do
      [ ! -L "$owned_path" ] || die "owned path is a symbolic link: $owned_path"
      owned_path="$(dirname "$owned_path")"
    done
    [ ! -e "$destination" ] && [ ! -L "$destination" ] && continue
    owned=0
    for target in "${owned_targets[@]}"; do [ "$target" != "$destination" ] || owned=1; done
    [ "$owned" -eq 1 ] || die "new destination collides with an unowned file: $destination"
  done
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
uninstall=0
prefix_explicit=0
previous_root=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a value"
      prefix="$2"
      prefix_explicit=1
      shift 2
      ;;
    --no-verify)
      verify=0
      shift
      ;;
    --previous-package-root)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die "--previous-package-root requires a value"
      previous_root="$2"
      shift 2
      ;;
    --uninstall)
      uninstall=1
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
if [ "$uninstall" -eq 1 ] || [ -n "$previous_root" ]; then
  [ "$prefix_explicit" -eq 1 ] && [ "$verify" -eq 1 ] ||
    die "owned removal requires explicit --prefix and enabled verification"
  case "$prefix" in
    /|""|*//*|*/|*/./*|*/../*|*/.|*/..) die "owned removal requires a canonical absolute prefix" ;;
    /*) ;;
    *) die "owned removal requires a canonical absolute prefix" ;;
  esac
  [ "$uninstall" -eq 0 ] || [ -z "$previous_root" ] ||
    die "--uninstall uses this archive; do not supply --previous-package-root"
fi
case "$prefix" in
  /*) ;;
  *) prefix="$(pwd)/$prefix" ;;
esac
root="$(archive_root)"

# The package formats this installer understands, and how each one is laid
# out. An archive declares its own format in its artifact metadata; nothing is
# inferred from what a package happens to contain.
#
#   rmux-package-v3/v4 carries share/rmux/package-layout.tsv, the projection of
#                    scripts/packaging/rmux-package-layout.json the package was
#                    built from, and is installed from it. The installer keeps
#                    only the transaction, never a second copy of the layout.
#   rmux-package-v2  predates that manifest. It is installed from the layout
#                    that produced it, listed below, so a release built before
#                    the manifest stays installable by this script.
#
# tests/package_layout_spec.py holds these identities against the descriptor,
# so the two cannot drift apart.
layout_current="rmux-package-v3"
layout_legacy="rmux-package-v2"
legacy_executables=(
  "libexec/rmux/rmux:helper"
  "bin/rmux-daemon:daemon"
  "bin/rmux:tiny"
)

sources=()
targets=()
# Executables are published with install -m 0755; assets retain source modes.
installed_modes=()
labels=()
staged_paths=()
backup_paths=()
target_existed=()
asset_sources=()
plan_path="$root/share/rmux/package-layout.tsv"

plan_from_manifest() {
  local plan_line=0 declared_paths declared_destinations declared_entries
  local destination_key index
  local plan_path_entry plan_mode plan_destination plan_required plan_entry plan_role
  local candidate actual_mode found
  # Every declared path is recorded between two newlines, so membership is an
  # exact comparison of complete entries: "bin/rmux" must never answer for a
  # file named "rmux" at the archive root.
  declared_paths=$'\n'
  # Destinations are compared without case: this package installs on
  # case-sensitive and case-insensitive filesystems alike.
  declared_destinations=$'\n'
  declared_entries=$'\n'
  while IFS="$(printf '\t')" read -r plan_path_entry plan_mode plan_destination \
      plan_required plan_entry plan_role; do
    plan_line=$((plan_line + 1))
    [ -n "$plan_path_entry" ] || continue
    [ -n "$plan_role" ] ||
      die "invalid layout manifest line $plan_line: $plan_path_entry"
    # Two spellings of one file are one file: bin/rmux, bin/./rmux and
    # bin//rmux name the same destination but compare as three. A path that
    # is not already canonical is refused here, before the membership and
    # destination comparisons that rely on it.
    for candidate in "$plan_path_entry" "$plan_destination"; do
      case "$candidate" in
        -) ;;
        ""|/*|*/|*//*|.|..|./*|../*|*/./*|*/../*|*/.|*/..|*\\*)
          die "non-canonical layout manifest path: $candidate (line $plan_line)" ;;
      esac
    done
    if [ ! -f "$root/$plan_path_entry" ]; then
      [ "$plan_required" = "optional" ] || die "archive is missing $plan_path_entry"
      continue
    fi
    # The entry id names a checkpoint and travels into file names, and two
    # entries sharing one id would share what is stored under it. It is
    # admitted here, before any of that: a plain lowercase token, declared
    # once.
    case "$plan_entry" in
      "" ) die "layout manifest line $plan_line declares no entry id" ;;
      *[!a-z0-9-]*|-*)
        die "layout manifest entry id is not usable: $plan_entry (line $plan_line)" ;;
    esac
    case "$declared_entries" in
      *$'\n'"$plan_entry"$'\n'*)
        die "layout manifest declares the entry id $plan_entry twice (line $plan_line)" ;;
    esac
    declared_entries="$declared_entries$plan_entry"$'\n'
    case "$declared_paths" in
      *$'\n'"$plan_path_entry"$'\n'*)
        die "layout manifest declares $plan_path_entry twice (line $plan_line)" ;;
    esac
    declared_paths="$declared_paths$plan_path_entry"$'\n'
    if [ "$plan_mode" != "-" ]; then
      actual_mode="$(printf '%04o' "$((8#$(stat_mode "$root/$plan_path_entry")))")"
      [ "$actual_mode" = "$plan_mode" ] ||
        die "archive entry mode is $actual_mode, expected $plan_mode: $plan_path_entry"
    fi
    [ "$plan_destination" != "-" ] || continue

    # Two entries landing on one destination would install onto each other.
    # The plan is refused here, before a single file is staged or replaced:
    # an asset given the dispatcher's destination used to be published after
    # the dispatcher had already been verified.
    destination_key="$(printf '%s' "$plan_destination" | tr 'A-Z' 'a-z')"
    case "$declared_destinations" in
      *$'\n'"$destination_key"$'\n'*)
        die "layout manifest installs two entries to $plan_destination (line $plan_line)" ;;
    esac
    declared_destinations="$declared_destinations$destination_key"$'\n'

    # What an entry is comes from the descriptor, not from its mode: a 0644
    # file is an asset because the manifest says it is a file, and the
    # executable set is exactly what the manifest calls executable.
    case "$plan_role" in
      executable)
        index="${#sources[@]}"
        sources[$index]="$root/$plan_path_entry"
        targets[$index]="$prefix/$plan_destination"
        installed_modes[$index]=755
        # The checkpoint and staging labels are the descriptor's entry ids, so
        # an injection point keeps its name when a destination moves.
        labels[$index]="$plan_entry"
        staged_paths[$index]=""
        backup_paths[$index]=""
        target_existed[$index]=0
        ;;
      file)
        asset_sources+=("$plan_path_entry$(printf '\t')$plan_destination")
        ;;
      *)
        die "unknown layout manifest role: $plan_role (line $plan_line)"
        ;;
    esac
  done < "$plan_path"

  # The archive must be exactly what its manifest declares: an entry nobody
  # declared is refused here, before any destination is touched.
  while IFS= read -r found; do
    [ -n "$found" ] || continue
    case "$declared_paths" in
      *$'\n'"$found"$'\n'*) ;;
      *) die "archive carries an undeclared file: $found" ;;
    esac
  done <<EOF
$(cd "$root" && find . -type f -o -type l | sed 's|^\./||' | LC_ALL=C sort)
EOF
}

plan_from_legacy_layout() {
  local entry relative label index
  for entry in "${legacy_executables[@]}"; do
    relative="${entry%%:*}"
    label="${entry##*:}"
    [ -f "$root/$relative" ] && [ -x "$root/$relative" ] ||
      die "archive is missing executable $relative"
    index="${#sources[@]}"
    sources[$index]="$root/$relative"
    targets[$index]="$prefix/$relative"
    installed_modes[$index]=755
    labels[$index]="$label"
    staged_paths[$index]=""
    backup_paths[$index]=""
    target_existed[$index]=0
  done
  # This format declares no manifest, so its assets are the files it carries
  # under share/, installed where the archive holds them, exactly as the
  # installer shipped with those releases did.
  if [ -d "$root/share" ]; then
    while IFS= read -r relative; do
      [ -n "$relative" ] || continue
      asset_sources+=("$relative$(printf '\t')$relative")
    done <<EOF
$(cd "$root" && find share -type f | LC_ALL=C sort)
EOF
  fi
}

# The package layout an artifact metadata document declares, if it declares
# one. The document is parsed, not searched: a member name is only a name
# where a name belongs, escapes in it are decoded before it is compared, and a
# string value that happens to read like the property is a value. awk is a
# POSIX tool this installer already relies on; nothing else is required.
#
# The answer is the exit status:
#   0  a package layout is declared, and printed on standard output
#   1  the document is a JSON object and declares none
#   2  the document is not valid JSON, or its root is not an object
#   3  the declaration is not a string
#   4  the document declares it more than once
package_layout_declaration() {
  awk '
    { document = document $0 "\n" }

    function at() { return substr(document, position, 1) }
    function skip_space() {
        while (position <= length(document) && match(at(), /[ \t\r\n]/)) position++
    }
    function take(character) {
        if (at() != character) return 0
        position++
        return 1
    }
    function hex_value(digit,    index_of) {
        index_of = index("0123456789abcdef", tolower(digit))
        if (index_of == 0) return -1
        return index_of - 1
    }
    # A JSON string, decoded. Returns "" and sets broken on a malformed string.
    function parse_string(    value, character, escape, code, digit, step) {
        if (!take("\"")) { broken = 1; return "" }
        value = ""
        while (1) {
            if (position > length(document)) { broken = 1; return "" }
            character = at()
            position++
            if (character == "\"") return value
            if (character == "\\") {
                escape = at()
                position++
                if (escape == "n") value = value "\n"
                else if (escape == "t") value = value "\t"
                else if (escape == "r") value = value "\r"
                else if (escape == "b") value = value "\b"
                else if (escape == "f") value = value "\f"
                else if (escape == "/" || escape == "\\" || escape == "\"") value = value escape
                else if (escape == "u") {
                    code = 0
                    for (step = 0; step < 4; step++) {
                        digit = hex_value(at())
                        if (digit < 0) { broken = 1; return "" }
                        code = code * 16 + digit
                        position++
                    }
                    # Only the ASCII range can spell the name of this property; anything
                    # above it is kept as a character that cannot match it.
                    if (code >= 32 && code < 127) value = value sprintf("%c", code)
                    else value = value "\001"
                }
                else { broken = 1; return "" }
            }
            else if (character < " ") { broken = 1; return "" }
            else value = value character
        }
    }
    function parse_number(    start) {
        start = position
        if (at() == "-") position++
        # JSON has no leading zeros: a number is 0, or a digit from 1 to 9
        # followed by any digits. 01 is not a number, so a document holding
        # one is not a document.
        if (at() == "0") position++
        else if (at() ~ /[1-9]/) { while (at() ~ /[0-9]/) position++ }
        else { broken = 1; return }
        if (at() == ".") { position++; if (at() !~ /[0-9]/) { broken = 1; return }
            while (at() ~ /[0-9]/) position++ }
        if (at() == "e" || at() == "E") { position++
            if (at() == "+" || at() == "-") position++
            if (at() !~ /[0-9]/) { broken = 1; return }
            while (at() ~ /[0-9]/) position++ }
        if (position == start) broken = 1
    }
    function parse_literal(word,    length_of) {
        length_of = length(word)
        if (substr(document, position, length_of) != word) { broken = 1; return }
        position += length_of
    }
    # Parses one value. At the root, top is 1 and the members of the object are
    # the ones this program reports on.
    function parse_value(depth, top,    character) {
        if (broken || depth > 64) { broken = 1; return }
        skip_space()
        character = at()
        if (character == "{") { parse_object(depth, top); return }
        if (character == "[") { parse_array(depth); return }
        if (character == "\"") { parse_string(); return }
        if (character == "t") { parse_literal("true"); return }
        if (character == "f") { parse_literal("false"); return }
        if (character == "n") { parse_literal("null"); return }
        if (character == "-" || character ~ /[0-9]/) { parse_number(); return }
        broken = 1
    }
    function parse_object(depth, top,    name, value_start) {
        if (!take("{")) { broken = 1; return }
        skip_space()
        if (take("}")) return
        while (1) {
            skip_space()
            name = parse_string()
            if (broken) return
            skip_space()
            if (!take(":")) { broken = 1; return }
            skip_space()
            value_start = position
            if (top && name == "package_layout") {
                declarations++
                if (at() == "\"") { declared = parse_string(); if (broken) return }
                else { not_a_string = 1; parse_value(depth + 1, 0); if (broken) return }
            } else {
                parse_value(depth + 1, 0)
                if (broken) return
            }
            skip_space()
            if (take(",")) continue
            if (take("}")) return
            broken = 1
            return
        }
    }
    function parse_array(depth) {
        if (!take("[")) { broken = 1; return }
        skip_space()
        if (take("]")) return
        while (1) {
            parse_value(depth + 1, 0)
            if (broken) return
            skip_space()
            if (take(",")) continue
            if (take("]")) return
            broken = 1
            return
        }
    }
    END {
        position = 1
        broken = 0
        declarations = 0
        not_a_string = 0
        skip_space()
        if (at() != "{") exit 2
        parse_value(0, 1)
        if (broken) exit 2
        skip_space()
        if (position <= length(document)) exit 2
        if (declarations > 1) exit 4
        if (declarations == 0) exit 1
        if (not_a_string) exit 3
        # No line terminator is added: the value is printed exactly as the
        # document declares it, and the caller keeps whatever it ends with.
        printf "%s", declared
        exit 0
    }
'
}

# An archive says which format it is in its artifact metadata. Four situations
# are distinguished here, because they are four different packages: the
# metadata declares nothing, it declares a format, it cannot be parsed, or it
# declares something unusable. Only the first can be an archive that predates
# the declaration; the others are answered on their own terms.
load_package_plan() {
  local metadata_path package_layout metadata_declares metadata_answer metadata_status
  local undeclared_legacy legacy_entry legacy_relative package_layout_shown
  sources=() targets=() installed_modes=() labels=() staged_paths=() backup_paths=() target_existed=() asset_sources=()
  plan_path="$root/share/rmux/package-layout.tsv"
  metadata_path="$root/share/rmux/artifact-metadata.json"
  package_layout=""
  metadata_declares=absent
  if [ -f "$metadata_path" ]; then
    # The value and the status come back together, and neither is trimmed. A
    # command substitution removes the newlines a command leaves at its end, and
    # a declared value may end with one; the marker below is what the shell
    # trims instead, so the value reaches the comparison exactly as it was
    # declared. The status follows the marker, which is why the substitution has
    # nothing to remove.
    metadata_answer="$(package_layout_declaration < "$metadata_path"; printf 'x%s' "$?")"
    metadata_status="${metadata_answer##*x}"
    package_layout="${metadata_answer%x*}"
    if [ "$metadata_status" = 0 ]; then
      metadata_declares=present
    else
      package_layout=""
      case "$metadata_status" in
        1) ;;
        2) die "archive metadata is not a valid JSON object: share/rmux/artifact-metadata.json" ;;
        3) die "archive metadata declares a package layout that is not a string: share/rmux/artifact-metadata.json" ;;
        4) die "archive metadata declares its package layout more than once: share/rmux/artifact-metadata.json" ;;
        *) die "archive metadata could not be read: share/rmux/artifact-metadata.json" ;;
      esac
    fi
  fi

  # An archive that declares no format at all predates the declaration. It is
  # installed as $layout_legacy only when it really is that layout: the complete
  # executable set, and no layout manifest to contradict it. That is a narrow,
  # named shape, not a rule that accepts whatever carries no manifest - a
  # truncated or inconsistent current package matches neither condition and is
  # refused below like any other.
  undeclared_legacy=0
  if [ "$metadata_declares" = absent ]; then
    [ ! -f "$plan_path" ] ||
      die "archive carries a layout manifest but declares no package layout"
    for legacy_entry in "${legacy_executables[@]}"; do
      legacy_relative="${legacy_entry%%:*}"
      [ -f "$root/$legacy_relative" ] && [ -x "$root/$legacy_relative" ] ||
        die "archive declares no package layout and does not carry the $layout_legacy layout: $legacy_relative is missing"
    done
    package_layout="$layout_legacy"
    undeclared_legacy=1
  fi

  case "$package_layout" in
    "$layout_current"|rmux-package-v4)
      [ -f "$plan_path" ] ||
        die "archive declares $layout_current but is missing its layout manifest: share/rmux/package-layout.tsv"
      plan_from_manifest
      ;;
    "$layout_legacy")
      if [ "$undeclared_legacy" -eq 0 ] && [ -f "$plan_path" ]; then
        die "archive declares $layout_legacy but carries a layout manifest: share/rmux/package-layout.tsv"
      fi
      plan_from_legacy_layout
      ;;
    *)
      # The comparison above is on the declared value exactly as it was read.
      # What the message shows replaces the characters that would break it over
      # several lines, so one refusal stays one line.
      package_layout_shown="$(printf '%s' "$package_layout" | tr '\n\r\t' '???')"
      die "unsupported package layout: $package_layout_shown (supported: $layout_current, $layout_legacy); also supported: rmux-package-v4"
      ;;
  esac
}
load_package_plan

[ "${#sources[@]}" -gt 0 ] || die "install plan declares no executable"
# The plan orders the dispatcher last, exactly where this transaction replaces
# it, so the CLI that is verified afterwards is the one it just committed.
dispatcher_target="${targets[$(( ${#targets[@]} - 1 ))]}"
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

# A destination the publication rename can replace in place. mv onto a link to
# a directory, or onto a directory, moves the staged file into that directory
# instead: the package would report success without the file, and would write
# outside the prefix. A link to a file, or a dangling link, is replaced itself.
assert_replaceable_destination() {
  local kind target
  kind="$1" target="$2"
  if [ -L "$target" ] && [ -d "$target" ]; then
    die "destination $kind path is a symbolic link to a directory: $target"
  fi
  if [ -e "$target" ] || [ -L "$target" ]; then
    [ -f "$target" ] || [ -L "$target" ] ||
      die "destination $kind path exists but is not a file: $target"
  fi
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

# Explicit rollback and uninstall use the same parsed archive plan and the
# same backups as installation. All ownership checks finish before mutation.
if [ "$uninstall" -eq 1 ] || [ -n "$previous_root" ]; then
  new_sources=("${sources[@]}") new_targets=("${targets[@]}") new_labels=("${labels[@]}")
  new_assets=()
  if [ "${#asset_sources[@]}" -gt 0 ]; then new_assets=("${asset_sources[@]}"); fi
  if [ -n "$previous_root" ]; then
    archive_root_saved="$root"
    [ -d "$previous_root" ] || die "previous package root does not exist"
    root="$(CDPATH= cd -- "$previous_root" && pwd -P)"
    previous_root="$root"
    load_package_plan
    [ "${#sources[@]}" -gt 0 ] || die "previous package plan declares no executable"
    root="$archive_root_saved"
  fi
  if [ "${#asset_sources[@]}" -gt 0 ]; then
    for asset_entry in "${asset_sources[@]}"; do
      sources+=("${previous_root:-$root}/${asset_entry%%$(printf '\t')*}")
      installed_modes+=("$(stat_mode "${sources[$(( ${#sources[@]} - 1 ))]}")")
      targets+=("$prefix/${asset_entry#*$(printf '\t')}")
      labels+=("owned-asset")
    done
  fi
  for ((index = 0; index < ${#targets[@]}; index++)); do
    assert_owned_file "${sources[$index]}" "${targets[$index]}" "${installed_modes[$index]}"
  done
  owned_sources=("${sources[@]}")
  owned_targets=("${targets[@]}")
  owned_installed_modes=("${installed_modes[@]}")
  if [ "$uninstall" -eq 0 ]; then
    new_destinations=("${new_targets[@]}")
    if [ "${#new_assets[@]}" -gt 0 ]; then
      for asset_entry in "${new_assets[@]}"; do new_destinations+=("$prefix/${asset_entry#*$'\t'}"); done
    fi
    remove_targets=()
    for target in "${owned_targets[@]}"; do
      retained=0
      for destination in "${new_destinations[@]}"; do
        [ "$destination" != "$target" ] || retained=1
      done
      [ "$retained" -eq 1 ] || remove_targets+=("$target")
    done
    assert_new_destinations_owned
    sources=("${new_sources[@]}") targets=("${new_targets[@]}") labels=("${new_labels[@]}")
    asset_sources=()
    if [ "${#new_assets[@]}" -gt 0 ]; then asset_sources=("${new_assets[@]}"); fi
    if [ "${#remove_targets[@]}" -gt 0 ]; then
      for target in "${remove_targets[@]}"; do
        sources+=("") targets+=("$target") labels+=("owned")
      done
    fi
  else
    for ((index = 0; index < ${#sources[@]}; index++)); do sources[$index]=""; done
    asset_sources=()
  fi
  for ((index = 0; index < ${#targets[@]}; index++)); do
    staged_paths[$index]="" backup_paths[$index]="" target_existed[$index]=0
  done
fi

# Reject every unusable destination, executable or asset, before creating a
# directory, a staging file or a backup. Symlinks to files are allowed because a
# successful upgrade replaces them just as the previous installer did; backups
# preserve them on rollback. A symlink to a directory is refused here.
for target in "${targets[@]}"; do
  assert_replaceable_destination executable "$target"
done
if [ "${#asset_sources[@]}" -gt 0 ]; then
  for asset_entry in "${asset_sources[@]}"; do
    assert_replaceable_destination asset "$prefix/${asset_entry#*$(printf '\t')}"
  done
fi
checkpoint after-preflight

# Every destination directory the projected plan needs, and nothing else.
for ((index = 0; index < ${#targets[@]}; index++)); do
  ensure_directory_tree "$(dirname "${targets[$index]}")"
done

transaction_root="$(mktemp -d "$prefix/.rmux-install-transaction.XXXXXX")"

# Stage and snapshot every package-owned asset before replacing any installed
# file. Asset paths share system directories with other packages, so the
# transaction records individual files instead of swapping the whole share
# tree. This preserves unrelated files while still allowing a complete
# rollback after a partial asset install.
if [ "${#asset_sources[@]}" -gt 0 ]; then
  for asset_entry in "${asset_sources[@]}"; do
    relative="${asset_entry%%$(printf '\t')*}"
    destination="${asset_entry#*$(printf '\t')}"
    source="$root/$relative"
    target="$prefix/$destination"
    target_dir="$(dirname "$target")"
    ensure_directory_tree "$target_dir"

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
  done
fi

# Stage the entire new executable set before the first destination mutation.
# Each staging file lives beside its destination, keeping the later rename
# atomic even when a nested install directory is a separate mount point.
for ((index = 0; index < ${#sources[@]}; index++)); do
  [ -n "${sources[$index]}" ] || continue
  target_dir="$(dirname "${targets[$index]}")"
  staged_paths[$index]="$(mktemp "$target_dir/.rmux-stage-${labels[$index]}.XXXXXX")"
  install -m 0755 "${sources[$index]}" "${staged_paths[$index]}"
  checkpoint "after-stage-${labels[$index]}"
done

# Snapshot every old destination before any replacement. cp -pP preserves both
# executable modes and symlinks, and the comparison catches a truncated backup.
for ((index = 0; index < ${#targets[@]}; index++)); do
  target="${targets[$index]}"
  # What a backup is stored under is its position in the plan, not the name
  # of its checkpoint: a checkpoint label identifies an injection point, it
  # does not have to be unique storage, and two destinations must never share
  # a snapshot whatever a manifest calls them.
  backup_paths[$index]="$transaction_root/backup-$index-${labels[$index]}"
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
if [ "$uninstall" -eq 1 ] || [ -n "$previous_root" ]; then
  for ((index = 0; index < ${#owned_targets[@]}; index++)); do
    assert_owned_file "${owned_sources[$index]}" "${owned_targets[$index]}" "${owned_installed_modes[$index]}"
  done
  [ "$uninstall" -eq 1 ] || assert_new_destinations_owned
fi
transaction_active=1
for ((index = 0; index < ${#targets[@]}; index++)); do
  if [ -n "${sources[$index]}" ]; then
    mv -f "${staged_paths[$index]}" "${targets[$index]}"
  else
    rm "${targets[$index]}"
    checkpoint after-remove-owned
  fi
  staged_paths[$index]=""
  checkpoint "after-replace-${labels[$index]}"
done

if [ "$verify" -eq 1 ] && [ "$uninstall" -eq 0 ]; then
  verify_layout "$dispatcher_target"
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
if [ "$uninstall" -eq 1 ]; then
  printf 'rmux package files uninstalled from %s\n' "$prefix"
else
  printf 'rmux installed to %s\n' "$dispatcher_target"
fi
