inventory_collected_entries=()

resolve_without_symlinks() {
  local path label absolute component candidate last_index
  local -a raw_components=()
  local -a resolved_components=()
  path="$1"
  label="$2"
  [ -n "$path" ] || die "$label path is empty"
  case "$path" in
    *$'\n'*|*$'\r'*) die "$label path contains a line break" ;;
  esac
  case "$path" in
    /*) absolute="$path" ;;
    *) absolute="$(pwd -P)/$path" ;;
  esac

  IFS=/ read -r -a raw_components <<< "$absolute"
  if [ "${#raw_components[@]}" -gt 0 ]; then
    for component in "${raw_components[@]}"; do
      case "$component" in
        ""|.) continue ;;
        ..)
          if [ "${#resolved_components[@]}" -gt 0 ]; then
            last_index=$((${#resolved_components[@]} - 1))
            unset "resolved_components[$last_index]"
          fi
          continue
          ;;
      esac

      resolved_components+=("$component")
      candidate=
      for component in "${resolved_components[@]}"; do
        candidate="$candidate/$component"
      done
      [ ! -L "$candidate" ] ||
        die "$label must not traverse symbolic links: $path"
    done
  fi

  candidate=
  if [ "${#resolved_components[@]}" -gt 0 ]; then
    for component in "${resolved_components[@]}"; do
      candidate="$candidate/$component"
    done
  fi
  printf '%s\n' "${candidate:-/}"
}

paths_overlap() {
  case "$1" in "$2"|"$2"/*) return 0 ;; esac
  case "$2" in "$1"|"$1"/*) return 0 ;; esac
  return 1
}

c_string_greater_than() {
  local LC_ALL=C
  [[ "$1" > "$2" ]]
}

collect_find_entries() {
  local label entry complete
  label="$1"
  shift
  inventory_collected_entries=()
  complete=0

  while IFS= read -r -d '' entry; do
    if [ -z "$entry" ]; then
      complete=1
      continue
    fi
    [ "$complete" -eq 0 ] || die "cannot enumerate $label"
    inventory_collected_entries+=("$entry")
  done < <(find "$@" -print0 && printf '\0')
  [ "$complete" -eq 1 ] || die "cannot enumerate $label"
}

collect_sorted_find_entries() {
  local label entry complete expected_count matched_index index
  local previous_entry= have_previous=0
  local unsorted=() remaining=()
  label="$1"
  shift
  collect_find_entries "$label" "$@"
  [ "${#inventory_collected_entries[@]}" -gt 0 ] || return 0
  unsorted=("${inventory_collected_entries[@]}")
  remaining=("${inventory_collected_entries[@]}")
  inventory_collected_entries=()
  expected_count="${#unsorted[@]}"
  complete=0

  while IFS= read -r -d '' entry; do
    if [ -z "$entry" ]; then
      complete=1
      continue
    fi
    [ "$complete" -eq 0 ] || die "cannot sort $label"
    matched_index=
    if [ "${#remaining[@]}" -gt 0 ]; then
      for index in "${!remaining[@]}"; do
        if [ "${remaining[$index]}" = "$entry" ]; then
          matched_index="$index"
          break
        fi
      done
    fi
    [ -n "$matched_index" ] ||
      die "cannot sort $label without preserving every record"
    unset "remaining[$matched_index]"
    if [ "$have_previous" -eq 1 ] &&
      c_string_greater_than "$previous_entry" "$entry"; then
      die "cannot sort $label in C byte order"
    fi
    previous_entry="$entry"
    have_previous=1
    inventory_collected_entries+=("$entry")
  done < <(printf '%s\0' "${unsorted[@]}" | LC_ALL=C sort -z && printf '\0')
  [ "$complete" -eq 1 ] &&
    [ "${#inventory_collected_entries[@]}" -eq "$expected_count" ] &&
    [ "${#remaining[@]}" -eq 0 ] ||
    die "cannot sort $label without record loss"
}

inventory_tree_entries() {
  local root label entry
  root="$1"
  label="$2"

  [ -d "$root" ] && [ ! -L "$root" ] ||
    die "$label root is missing or unsafe"
  collect_find_entries "$label" -P "$root" -mindepth 1
  [ "${#inventory_collected_entries[@]}" -gt 0 ] || return 0
  for entry in "${inventory_collected_entries[@]}"; do
    if [ -L "$entry" ]; then
      readlink -- "$entry" >/dev/null ||
        die "cannot read symbolic link in $label"
      die "$label must not contain symbolic links"
    elif [ -f "$entry" ]; then
      hash_file sha256 "$entry" >/dev/null ||
        die "cannot read regular file in $label"
    elif [ ! -d "$entry" ]; then
      die "$label contains an unsupported entry type: $entry"
    fi
  done
}

validate_apt_package_identity() {
  local package expected_architecture package_name package_architecture
  package="$1"
  expected_architecture="$2"
  dpkg-deb -f "$package" >/dev/null ||
    die "cannot read APT package metadata: $package"
  package_name="$(dpkg-deb -f "$package" Package)" ||
    die "cannot read APT Package metadata: $package"
  [ "$package_name" = rmux ] ||
    die "APT Package identity must be rmux: $package"
  package_architecture="$(dpkg-deb -f "$package" Architecture)" ||
    die "cannot read APT Architecture metadata: $package"
  [ "$package_architecture" = "$expected_architecture" ] ||
    die "APT Architecture metadata does not match $expected_architecture: $package"
}

validate_input_packages() {
  local architecture deb matches
  input_packages=()
  shopt -s nullglob
  for architecture in "${architectures[@]}"; do
    matches=("$input_dir"/rmux_*_"$architecture".deb)
    [ "${#matches[@]}" -gt 0 ] ||
      die "no rmux_*_${architecture}.deb files found in $input_dir"
    for deb in "${matches[@]}"; do
      [ -f "$deb" ] && [ ! -L "$deb" ] ||
        die "unsafe APT input package: $deb"
      case "${deb##*/}" in *$'\n'*) die "APT input package name contains a newline" ;; esac
      validate_apt_package_identity "$deb" "$architecture"
      input_packages+=("$deb")
    done
  done
  shopt -u nullglob
}
