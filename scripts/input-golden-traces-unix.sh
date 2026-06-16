#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-}"
if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/rmux-input-golden-unix.XXXXXX")"
fi
OUT_DIR="$(mkdir -p "$OUT_DIR" && cd "$OUT_DIR" && pwd)"

RMUX="${RMUX_BINARY:-$ROOT/target/debug/rmux}"
RUN_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-input-golden-run.XXXXXX")"
RMUX_TMPDIR="$RUN_ROOT/socket"
export RMUX_TMPDIR
mkdir -p "$RMUX_TMPDIR"

cleanup() {
    "$RMUX" -L "$LABEL" kill-server >/dev/null 2>&1 || true
    rm -rf "$RUN_ROOT"
}
trap cleanup EXIT

fail() {
    printf 'input golden unix failed: %s\n' "$*" >&2
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

wait_until() {
    local name="$1"
    shift
    local deadline=$((SECONDS + 8))
    while (( SECONDS < deadline )); do
        if "$@"; then
            return 0
        fi
        sleep 0.1
    done
    fail "timed out waiting for $name"
}

capture_contains() {
    "$RMUX" -L "$LABEL" capture-pane -p -t "$PANE" 2>/dev/null | grep -Fq "$1"
}

shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

attach_payload_and_detach() {
    local payload_file="$1"

    python3 - "$RMUX" "$LABEL" "$SESSION" "$payload_file" <<'PY'
import os
import pty
import select
import signal
import sys
import time

rmux_bin, label, session, payload_path = sys.argv[1:5]
with open(payload_path, "rb") as payload_file:
    payload = payload_file.read()

pid, fd = pty.fork()
if pid == 0:
    os.execlp(rmux_bin, rmux_bin, "-L", label, "attach-session", "-t", session)

seen = bytearray()


def pump_until(needle: bytes, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        readable, _, _ = select.select([fd], [], [], min(0.1, remaining))
        if fd in readable:
            try:
                data = os.read(fd, 8192)
            except OSError:
                return needle in seen
            if not data:
                return needle in seen
            os.write(1, data)
            seen.extend(data)
            if needle in seen:
                return True
        try:
            child_pid, _status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return needle in seen
        if child_pid == pid:
            return needle in seen
    return False


try:
    if not pump_until(session.encode(), 8):
        sys.exit(2)
    os.write(fd, payload)
    os.write(fd, b"\x04\x04")
    if not pump_until(b"RMUX_P4E_DONE", 8):
        sys.exit(3)
    os.write(fd, b"\x02d")
    if not pump_until(b"[detached", 8):
        sys.exit(4)
    sys.exit(0)
finally:
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass
    try:
        os.waitpid(pid, os.WNOHANG)
    except (ChildProcessError, OSError):
        pass
PY
}

require cargo
require python3
require shasum
require cmp

mkdir -p "$OUT_DIR"
HEAD="$(git -C "$ROOT" rev-parse HEAD)"
SHORT_HEAD="$(git -C "$ROOT" rev-parse --short=12 HEAD)"
LABEL="input-golden-unix-$$"
SESSION="p4e"
PANE="$SESSION:0.0"
EXPECTED="$OUT_DIR/expected-input.bin"
ATTACH_PAYLOAD="$OUT_DIR/attach-input.bin"
CAPTURED="$OUT_DIR/captured-input.bin"
SUMMARY="$OUT_DIR/summary.json"

printf '[input-golden] cargo build --locked\n'
cargo build --locked --manifest-path "$ROOT/Cargo.toml"

python3 - "$EXPECTED" "$ATTACH_PAYLOAD" <<'PY'
import pathlib
import sys

expected = (
    "RMUX_P4E_BEGIN\r\n".encode()
    + "ASCII marker survives input golden trace\r\n".encode()
    + "CRLF marker A\r\nCRLF marker B\r\n".encode()
    + "UTF8 CJK marker: 東京 한글\r\n".encode("utf-8")
    + "COMBINING marker: cafe\u0301\r\n".encode("utf-8")
    + b"CTRL_B literal: \x02not-prefix\r\n"
    + b"SGR-looking bytes: \x1b[<64;2;2M\r\n"
    + b"CSI-u-looking bytes: \x1b[9;2u\r\n"
    + b"Nested-start-looking bytes: \x1b[200~ stay payload\r\n"
    + "RMUX_P4E_END\r\n".encode()
)
attach_payload = b"\x1b[200~" + expected + b"\x1b[201~"
pathlib.Path(sys.argv[1]).write_bytes(expected)
pathlib.Path(sys.argv[2]).write_bytes(attach_payload)
PY

collector_path="$(shell_quote "$CAPTURED")"
collector="stty -echo -icrnl; cat > $collector_path; printf 'RMUX_P4E_DONE\n'; shasum -a 256 $collector_path | awk '{print \"RMUX_P4E_SHA \" \$1}'"

"$RMUX" -L "$LABEL" new-session -d -s "$SESSION" -x 80 -y 24
"$RMUX" -L "$LABEL" send-keys -t "$PANE" "$collector" Enter
wait_until 'collector file creation' test -f "$CAPTURED"

attach_payload_and_detach "$ATTACH_PAYLOAD"
wait_until 'collector done marker' capture_contains 'RMUX_P4E_DONE'
wait_until 'collector sha marker' capture_contains 'RMUX_P4E_SHA'

cmp -s "$EXPECTED" "$CAPTURED" || fail "captured bytes differ from expected payload"

python3 - "$SUMMARY" "$EXPECTED" "$ATTACH_PAYLOAD" "$CAPTURED" "$HEAD" "$SHORT_HEAD" <<'PY'
import hashlib
import json
import pathlib
import platform
import sys

summary_path, expected_path, attach_path, captured_path, head, short_head = sys.argv[1:7]
expected = pathlib.Path(expected_path).read_bytes()
attach_payload = pathlib.Path(attach_path).read_bytes()
captured = pathlib.Path(captured_path).read_bytes()

def has(data: bytes, needle: bytes) -> bool:
    return needle in data

summary = {
    "schema_version": 1,
    "phase": "P4E",
    "platform_family": "unix",
    "platform": platform.platform(),
    "head": head,
    "short_head": short_head,
    "verdict": "PASS_BYTE_PARITY",
    "expected_sha256": hashlib.sha256(expected).hexdigest(),
    "attach_payload_sha256": hashlib.sha256(attach_payload).hexdigest(),
    "captured_sha256": hashlib.sha256(captured).hexdigest(),
    "expected_bytes": len(expected),
    "attach_payload_bytes": len(attach_payload),
    "captured_bytes": len(captured),
    "facts": {
        "exact_input_bytes": expected == captured,
        "outer_bracketed_paste_wrappers_stripped": not captured.startswith(b"\x1b[200~")
        and not captured.endswith(b"\x1b[201~"),
        "ascii_marker": has(captured, b"ASCII marker survives input golden trace"),
        "crlf_marker_a": has(captured, b"CRLF marker A"),
        "crlf_marker_b": has(captured, b"CRLF marker B"),
        "ctrl_b_byte": b"\x02" in captured,
        "sgr_mouse_looking_esc": has(captured, b"\x1b[<64;2;2M"),
        "csi_u_looking_esc": has(captured, b"\x1b[9;2u"),
        "nested_start_looking_esc": captured.count(b"\x1b[200~") == 1,
        "cjk_marker": "東京".encode("utf-8") in captured and "한글".encode("utf-8") in captured,
        "combining_acute": "cafe\u0301".encode("utf-8") in captured,
    },
}
pathlib.Path(summary_path).write_text(
    json.dumps(summary, indent=2, ensure_ascii=False) + "\n",
    encoding="utf-8",
)
PY

cat >"$OUT_DIR/SHA256SUMS.txt" <<EOF
$(sha256_file "$EXPECTED")  expected-input.bin
$(sha256_file "$ATTACH_PAYLOAD")  attach-input.bin
$(sha256_file "$CAPTURED")  captured-input.bin
$(sha256_file "$SUMMARY")  summary.json
EOF

printf '[input-golden] summary=%s\n' "$SUMMARY"
printf '[input-golden] verdict=PASS_BYTE_PARITY\n'
