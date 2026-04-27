#!/usr/bin/env sh
set -eu

limit="${1:-600}"
exceptions_file="${FILE_SIZE_EXCEPTIONS:-docs/file-size-exceptions.txt}"

if [ ! -f "$exceptions_file" ]; then
  echo "missing exceptions file: $exceptions_file" >&2
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

find src crates -type f -name '*.rs' 2>/dev/null \
  | grep -v '/target/' \
  | grep -v '/examples/' \
  | grep -v '/tests/' \
  | grep -v '/test/' \
  | grep -v '/src/.*tests' \
  | while IFS= read -r file; do
      lines="$(wc -l < "$file" | tr -d ' ')"
      if [ "$lines" -gt "$limit" ]; then
        printf '%s\t%s\n' "$lines" "$file" >> "$tmp"
      fi
    done

if [ ! -s "$tmp" ]; then
  echo "No production Rust files exceed $limit lines."
  exit 0
fi

sort -nr "$tmp"

missing=0
while IFS="$(printf '\t')" read -r lines file; do
  if ! grep -Fq "\`$file\`" "$exceptions_file"; then
    echo "missing file-size exception: $file ($lines lines)" >&2
    missing=$((missing + 1))
  fi
done < "$tmp"

if [ "$missing" -ne 0 ]; then
  echo "$missing production Rust file(s) exceed $limit lines without an exception." >&2
  exit 1
fi

echo "All oversized production Rust files are registered in $exceptions_file."
