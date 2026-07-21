#!/usr/bin/env bash
set -euo pipefail

# Compatibility entrypoint. A shadow candidate is subject to the same exact
# fast-run, source, intent, and planned-ref bindings as every other candidate.
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec "$script_dir/dispatch-release-candidate.sh" "$@" --release-kind shadow
