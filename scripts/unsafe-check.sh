#!/usr/bin/env sh
set -eu

missing_file="$(mktemp)"
trap 'rm -f "$missing_file"' EXIT

find src crates -type f -name '*.rs' 2>/dev/null \
  | grep -v '/target/' \
  | grep -v '/examples/' \
  | grep -v '/tests/' \
  | grep -v '/test/' \
  | grep -v '/src/.*tests' \
  | while IFS= read -r file; do
      awk '
        { lines[NR] = $0 }
        /unsafe[[:space:]]*\{/ {
          unsafe_lines[++unsafe_count] = NR
        }
        END {
          for (u = 1; u <= unsafe_count; u++) {
            line = unsafe_lines[u]
            found = 0
            for (i = line - 6; i <= line + 4; i++) {
              if (i in lines && lines[i] ~ /SAFETY:/) found = 1
            }
            if (!found) {
              printf "%s:%d: unsafe block missing nearby SAFETY comment\n", FILENAME, line
              exit_code = 1
            }
          }
          if (exit_code) exit 1
        }
      ' "$file" || printf '%s\n' "$file" >> "$missing_file"
    done

missing="$(wc -l < "$missing_file" | tr -d ' ')"
if [ "$missing" -ne 0 ]; then
  echo "$missing file(s) contain unsafe blocks without nearby SAFETY comments." >&2
  exit 1
fi

echo "Unsafe check passed: every unsafe block has a nearby SAFETY comment."
