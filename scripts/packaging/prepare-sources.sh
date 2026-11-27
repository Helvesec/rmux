# Preparing the sources a package builder stages, from the one layout
# descriptor. Sourced by the shell builders; they provide die and layout_tool.
#
# A generator names the files it writes and says what it wrote, one line per
# file: the origin it belongs to, a tab, and the path. The descriptor names
# what the package expects for that origin. Joining them here is what lets a
# source be renamed in one place, so no builder keeps a list of its own.

# The file name a source is prepared under: a plain name, never a path.
assert_prepared_name() {
  case "$1" in
    ""|*/*|*\\*|*[[:space:]]*) die "not a source file name: $1" ;;
  esac
}

# prepare_generated_sources <variant> <kind> <report> <destination>
prepare_generated_sources() {
  local variant kind report destination origin name generated status
  variant="$1"
  kind="$2"
  report="$3"
  destination="$4"

  [ -f "$report" ] || die "the $kind generator reported nothing: $report"
  while IFS="$(printf '\t')" read -r origin generated; do
    [ -n "$origin" ] || continue
    [ -n "$generated" ] ||
      die "the $kind generator reported a line this packaging cannot read: $origin"
  done < "$report"

  # The projection is taken as a value and its status is read. Feeding a
  # command substitution straight into the loop would turn a refused
  # descriptor into an empty list and prepare nothing, quietly.
  local expected
  expected="$("$(layout_tool)" source-names --variant "$variant" --kind "$kind" --with-origin)" ||
    die "could not project the $kind sources of $variant"
  [ -n "$expected" ] || die "the descriptor projects no $kind source for $variant"

  mkdir -p "$destination"
  while IFS="$(printf '\t')" read -r origin name; do
    [ -n "$origin" ] || continue
    assert_prepared_name "$name"
    # The builders run under errexit, where a simple assignment from a command
    # that answers non-zero ends the run before its status can be read and
    # before the diagnostic below. Asking inside an if keeps both.
    if generated="$(awk -F'\t' -v want="$origin" '
      $1 == want { path = $2; found++ }
      END { if (found == 1) print path; exit found == 1 ? 0 : (found > 1 ? 2 : 1) }' "$report")"; then
      status=0
    else
      status=$?
    fi
    case "$status" in
      0) ;;
      1) die "the $kind generator wrote no $origin source" ;;
      *) die "the $kind generator reported $origin more than once" ;;
    esac
    [ -f "$generated" ] ||
      die "the $kind generator reported a file it did not write: $generated"
    cp "$generated" "$destination/$name"
  done <<EOF
$expected
EOF
}

# prepare_cached_sources <variant> <kind> <from> <to> <message>
prepare_cached_sources() {
  local variant kind from to message name
  variant="$1"
  kind="$2"
  from="$3"
  to="$4"
  message="$5"

  local names
  names="$("$(layout_tool)" source-names --variant "$variant" --kind "$kind")" ||
    die "could not project the $kind sources of $variant"
  [ -n "$names" ] || die "the descriptor projects no $kind source for $variant"

  mkdir -p "$to"
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    assert_prepared_name "$name"
    [ -f "$from/$name" ] || die "$message: $from/$name"
    cp "$from/$name" "$to/$name"
  done <<EOF
$names
EOF
}

# stage_from_descriptor <variant> <stage> [stage-plan arguments...]
stage_from_descriptor() {
  local variant stage source destination mode transform
  local plan
  variant="$1"
  stage="$2"
  shift 2
  plan="$("$(layout_tool)" stage-plan --variant "$variant" "$@")" ||
    die "could not project the stage plan of $variant"
  [ -n "$plan" ] || die "the stage plan of $variant is empty"
  while IFS="$(printf '\t')" read -r source destination mode transform; do
    [ -n "$source" ] || continue
    [ -f "$source" ] || die "package source is missing: $source"
    mkdir -p "$stage/$(dirname "$destination")"
    case "$transform" in
      copy) install -m "$mode" "$source" "$stage/$destination" ;;
      gzip)
        gzip -n -c "$source" > "$stage/$destination"
        chmod "$mode" "$stage/$destination"
        ;;
      *) die "unknown package layout transform: $transform" ;;
    esac
  done <<EOF
$plan
EOF
}

# stage_rpm_sources <sources-directory> <repo> <completions>
#
# The RPM builder does not stage into a tree: it copies every source it needs
# into one SOURCES directory, under the name the descriptor prepares it with,
# and its spec then installs them from there. That is a different boundary
# from stage_from_descriptor, and this is it.
stage_rpm_sources() {
  local sources repo completions plan source destination mode transform staged
  sources="$1"
  repo="$2"
  completions="$3"

  plan="$("$(layout_tool)" stage-plan --variant rpm --staged-names \
    --repo "$repo" --completions "$completions" --generated "$sources" \
    --only-sources repo,completion)" ||
    die "could not project the RPM sources"
  [ -n "$plan" ] || die "the RPM source plan is empty"

  mkdir -p "$sources"
  while IFS="$(printf '\t')" read -r source destination mode transform staged; do
    [ -n "$source" ] || continue
    [ -f "$source" ] || die "package source is missing: $source"
    install -m "$mode" "$source" "$sources/$staged"
  done <<EOF
$plan
EOF
}
