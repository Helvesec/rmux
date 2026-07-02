#!/usr/bin/env sh
set -eu

# debug_assert! is compiled out of release builds. Keep it free of obvious
# state-changing calls so release behavior cannot diverge from debug tests.
pattern='debug_assert!\s*\([^;\n]*\.(append|clear|drain|extend|insert|pop|push|remove|replace|retain|set_\w*|swap|take|truncate)\s*\('

scan_with_perl() {
  tmp="$(mktemp)"
  find src crates \
    -type f \
    -name '*.rs' \
    ! -path '*/target/*' \
    ! -path '*/tests/*' \
    ! -name '*tests*.rs' \
    -print0 |
    xargs -0 perl -ne '
      if (/debug_assert!\s*\([^;\n]*\.(append|clear|drain|extend|insert|pop|push|remove|replace|retain|set_\w*|swap|take|truncate)\s*\(/) {
        print "$ARGV:$.:$_";
      }
    ' > "$tmp"
  if [ -s "$tmp" ]; then
    cat "$tmp"
    rm -f "$tmp"
    return 0
  fi
  rm -f "$tmp"
  return 1
}

set +e
if command -v rg >/dev/null 2>&1 && rg --version >/dev/null 2>&1; then
  rg -n --pcre2 "$pattern" src crates \
    --glob '*.rs' \
    --glob '!**/target/**' \
    --glob '!**/tests/**' \
    --glob '!**/*tests*.rs'
else
  scan_with_perl
fi
status=$?
set -e

case "$status" in
  0)
    echo "debug_assert! must not contain state-changing calls; release builds remove them." >&2
    exit 1
    ;;
  1)
    ;;
  *)
    echo "debug_assert! side-effect scan failed while running rg (exit $status)." >&2
    exit "$status"
    ;;
esac

echo "debug_assert! side-effect check passed."
