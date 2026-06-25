#!/bin/sh
set -eu

count="${1:-10000}"
i=1
while [ "$i" -le "$count" ]; do
    printf 'rmux-perf-line-%s\n' "$i"
    i=$((i + 1))
done
