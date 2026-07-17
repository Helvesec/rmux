#!/usr/bin/env bash

# Shared identity helpers for perf-bench.sh and perf-baseline.sh. Callers own
# strict-mode setup so this file remains safe to source.

rmux_perf_host_fingerprint() {
    if [ -n "${RMUX_PERF_HOST_FINGERPRINT:-}" ]; then
        printf '%s' "$RMUX_PERF_HOST_FINGERPRINT"
        return
    fi
    local identity
    if [ -r /etc/machine-id ]; then
        identity="machine-id:$(cat /etc/machine-id)"
    else
        identity="hostname:$(hostname)"
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$identity" | sha256sum | cut -c1-16
    else
        printf '%s' "$identity" | shasum -a 256 | cut -c1-16
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

json_string() {
    printf '"%s"' "$(json_escape "$1")"
}

rmux_perf_capture_current_identity() {
    local binary_path="$1"
    local reused_binary="$2"

    RMUX_PERF_GIT_COMMIT="$(git rev-parse HEAD)"
    RMUX_PERF_GIT_DESCRIBE="$(git describe --tags --always --dirty)"
    if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
        echo "perf bench requires a clean tracked worktree" >&2
        return 2
    fi
    RMUX_PERF_PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')"
    RMUX_PERF_MACHINE="$(uname -m | tr '[:upper:]' '[:lower:]')"
    RMUX_PERF_HOST_FINGERPRINT_VALUE="$(rmux_perf_host_fingerprint)"
    RMUX_PERF_EXPECTED_COMMIT_VALUE="${RMUX_PERF_EXPECTED_GIT_SHA:-$RMUX_PERF_GIT_COMMIT}"
    RMUX_PERF_EXPECTED_PLATFORM_VALUE="${RMUX_PERF_EXPECTED_PLATFORM:-$RMUX_PERF_PLATFORM}"
    RMUX_PERF_PROVENANCE_VALUE="${RMUX_PERF_EXPECTED_PROVENANCE:-local}"

    if [ "$RMUX_PERF_EXPECTED_COMMIT_VALUE" != "$RMUX_PERF_GIT_COMMIT" ]; then
        echo "RMUX_PERF_EXPECTED_GIT_SHA=$RMUX_PERF_EXPECTED_COMMIT_VALUE does not match checkout $RMUX_PERF_GIT_COMMIT" >&2
        return 2
    fi
    if [ "$RMUX_PERF_EXPECTED_PLATFORM_VALUE" != "$RMUX_PERF_PLATFORM" ]; then
        echo "RMUX_PERF_EXPECTED_PLATFORM=$RMUX_PERF_EXPECTED_PLATFORM_VALUE does not match host $RMUX_PERF_PLATFORM" >&2
        return 2
    fi
    if [ -z "$RMUX_PERF_PROVENANCE_VALUE" ]; then
        echo "RMUX_PERF_EXPECTED_PROVENANCE must not be empty" >&2
        return 2
    fi
    case "$RMUX_PERF_PROVENANCE_VALUE" in
        *$'\n'*|*$'\r'*)
            echo "RMUX_PERF_EXPECTED_PROVENANCE must be a single line" >&2
            return 2
            ;;
    esac

    RMUX_PERF_BINARY_SHA256="$(sha256_file "$binary_path")"
    RMUX_PERF_BINARY_VERSION="$("$binary_path" -V)"
    RMUX_PERF_BUILD_MODE="rebuilt"
    if [ "$reused_binary" -eq 1 ]; then
        RMUX_PERF_BUILD_MODE="reused"
    fi
}
