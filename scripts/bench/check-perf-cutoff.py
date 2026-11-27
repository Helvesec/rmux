#!/usr/bin/env python3
"""Exercise the retired perf controls and the existing Unix measurement harness.

Requires Python 3.9+, Cargo, a clean checkout and an explicitly identified,
locally built full rmux binary. This is a correctness smoke fixture, not a
release benchmark or an internal-span measurement.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import toml_reader

ROOT = Path(__file__).resolve().parents[2]
LEGACY_ENV = "RMUX_PERF_TRACE"
# Any INITIAL_TRACE name is the retired control, whatever its prefix.
INITIAL_ENV = "RMUX_INITIAL_TRACE_CONFIG"
OPERATIONS = ["list_sessions_default", "capture_pane_80x24"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(args, *, env=None, check=True):
    return subprocess.run(
        [str(arg) for arg in args], cwd=ROOT, env=env, check=check,
        capture_output=True, timeout=60,
    )


def check_product_cutoff(root=None):
    """Reject retired controls in product source, manifests and packaged assets.

    Complete removal is intentional: test-seams must not restore this recorder.
    The external harness and its canaries live outside the product directories.
    """
    root = ROOT if root is None else Path(root)
    markers = (INITIAL_ENV, LEGACY_ENV, "INITIAL_TRACE", "initial_trace", "initial-trace", "perf-instrument")
    paths = [root / "Cargo.toml"]
    if (root / "build.rs").is_file():
        paths.append(root / "build.rs")
    for directory in ("src", "crates", "resources", "scripts/packaging"):
        paths.extend(path for path in (root / directory).rglob("*") if path.is_file())
    checked = []
    violations = []
    for path in sorted(paths):
        relative = path.relative_to(root).as_posix()
        content = path.read_bytes()
        if path.name == "Cargo.toml":
            # Cargo interprets TOML escapes in feature names and forwarded values.
            manifest = toml_reader.loads(content.decode("utf-8"))
            content += b"\n" + json.dumps(manifest, ensure_ascii=False).encode("utf-8")
        found = [marker for marker in markers
                 if marker in relative or marker.encode() in content]
        if found:
            violations.append({"path": relative, "markers": found})
        checked.append(relative)
    require(not violations, f"product instrumentation exposure: {violations}")
    return {"files_checked": len(checked), "markers": list(markers),
            "scope": "product source/manifests/assets; no binary or native OS inference"}


def check_features(cargo):
    rejections = {}
    for package, path in [("rmux", "Cargo.toml"), ("rmux-server", "crates/rmux-server/Cargo.toml")]:
        manifest = toml_reader.loads((ROOT / path).read_bytes().decode('utf-8'))
        features = manifest["features"]
        require("perf-instrument" not in features, f"{package} still publishes perf-instrument")
        require(not any("perf-instrument" in value for values in features.values() for value in values),
                f"{package} still forwards perf-instrument")
        rejected = run([cargo, "check", "--offline", "--locked", "-p", package,
                        "--no-default-features", "--features", "perf-instrument"], check=False)
        error = rejected.stderr.decode()
        require(rejected.returncode != 0 and "does not contain this feature: perf-instrument" in error,
                f"{package}: expected missing-feature rejection, got {error}")
        rejections[package] = error.strip()
    return rejections


def cli_results(binary, env, *, socket_path=None, samples=None):
    """Compare real CLI output, a pane result and owned missing-target errors."""
    socket = f"perf-cutoff-{os.getpid()}-{time.time_ns()}"
    endpoint = ["-S", socket_path] if socket_path is not None else ["-L", socket]
    prefix = [binary, *endpoint, "-f", "/dev/null"]
    observations = []
    try:
        for args in [["-V"], ["--help"], ["list-commands"], ["cutoff-unknown-command"]]:
            result = run([binary, *args], env=env, check=False)
            observations.append((result.returncode, result.stdout, result.stderr))
        require(observations[-1][0] != 0, "unknown command unexpectedly succeeded")
        started = time.perf_counter_ns()
        run([*prefix, "new-session", "-d", "-s", "cutoff", "-x", "80", "-y", "24",
             "printf 'cutoff-ready\\n'; exec /bin/cat"], env=env)
        created = time.perf_counter_ns()
        # Renaming refreshes the session even without an attached terminal; this
        # reaches the old attach_refresh span in an instrumented parent binary.
        for source, target in [("cutoff", "cutoff-renamed"), ("cutoff-renamed", "cutoff")]:
            result = run([*prefix, "rename-session", "-t", source, target], env=env)
            observations.append((result.returncode, result.stdout, result.stderr))
        deadline = time.monotonic() + 10
        while True:
            capture = run([*prefix, "capture-pane", "-p", "-t", "cutoff"], env=env)
            if b"cutoff-ready" in capture.stdout:
                break
            require(time.monotonic() < deadline, "pane marker not observed")
            time.sleep(0.02)
        observed = time.perf_counter_ns()
        observations.append((capture.returncode, capture.stdout, capture.stderr))
        fields = "#{session_id} #{window_id} #{pane_id} #{pane_pid} #{pane_width} #{pane_height}"
        initial = run([*prefix, "display-message", "-p", "-t", "cutoff:0.0", fields], env=env)
        before = initial.stdout.decode().strip().split()
        require(len(before) == 6 and int(before[3]) > 0, "missing actual child identity")
        require(before[4:] == ["80", "24"], f"wrong initial pane size: {before}")
        os.kill(int(before[3]), 0)  # Read-only liveness check for this endpoint's child.
        resize_started = time.perf_counter_ns()
        run([*prefix, "resize-window", "-t", "cutoff:0", "-x", "90", "-y", "25"], env=env)
        resize_finished = time.perf_counter_ns()
        resized = run([*prefix, "display-message", "-p", "-t", "cutoff:0.0", fields], env=env)
        after = resized.stdout.decode().strip().split()
        require(after[:4] == before[:4] and after[4:] == ["90", "25"],
                f"resize changed identity or missed geometry: {before} -> {after}")
        # PIDs differ across runs; assert their continuity above, compare only
        # stable CLI facts, and retain the actual identities in external evidence.
        observations.append((resized.returncode, " ".join(after[4:]).encode(), resized.stderr))
        if samples is not None:
            samples.append({"initial": before, "resized": after,
                            "new_session_rpc_ms": (created - started) / 1e6,
                            "marker_observed_after_ms": (observed - started) / 1e6,
                            "resize_window_rpc_ms": (resize_finished - resize_started) / 1e6})
        for args in [["display-message", "-p", "-t", "cutoff", "#{session_name}"],
                     ["capture-pane", "-p", "-t", "cutoff-missing"]]:
            result = run([*prefix, *args], env=env, check=False)
            observations.append((result.returncode, result.stdout, result.stderr))
        require(observations[-2][1].strip() == b"cutoff", "wrong session result")
        require(observations[-1][0] != 0, "missing target unexpectedly succeeded")
    finally:
        run([*prefix, "kill-server"], env=env, check=False)
    return observations


def check_initial_env(binary, env, output, samples):
    """Feed the old valid Unix configuration to the actual private daemon."""
    # Keep Unix socket names below sun_path's limit regardless of output path.
    with tempfile.TemporaryDirectory(prefix="rmux-cutoff-") as sockets, \
            tempfile.TemporaryDirectory(prefix="initial-env-", dir=output) as directory:
        directory = Path(directory)
        socket = Path(sockets) / "daemon.sock"
        baseline = cli_results(binary, env, socket_path=socket, samples=samples)
        config = directory / "config.json"
        existing = directory / "existing.jsonl"
        sentinel = b"preserve-existing-initial-trace\n"
        existing.write_bytes(sentinel)
        paths = [directory / "fresh.jsonl", existing, directory / "missing-parent/trace.jsonl"]
        cases = []
        for index, path in enumerate(paths):
            socket = Path(sockets) / f"valid-{index}.sock"
            # The removed Unix implementation used a process-local Instant,
            # sampled from zero, with a 1 GHz frequency and a <=30 second window.
            content = json.dumps({"schema_version": 1, "case_id": "cutoff-migration",
                                  "endpoint": str(socket), "output": str(path),
                                  "start_qpc": 0, "end_qpc": 30_000_000_000,
                                  "qpc_frequency": 1_000_000_000}).encode()
            config.write_bytes(content)
            result = cli_results(binary, {**env, INITIAL_ENV: str(config)},
                                 socket_path=socket, samples=samples)
            require(result == baseline, "retired initial config changed CLI results")
            require(config.read_bytes() == content, "product changed the input configuration")
            require(path.read_bytes() == sentinel if path == existing else not path.exists(),
                    f"retired initial config wrote {path}")
            require(not any(Path(str(path) + suffix).exists()
                            for suffix in (".closing.json", ".closed.json")), "product wrote a trace receipt")
            cases.append(path.relative_to(directory).as_posix())
        require(not (directory / "missing-parent").exists(), "product created trace parent")
        for index, content in enumerate((b"not-json", None)):
            socket = Path(sockets) / f"invalid-{index}.sock"
            if content is None:
                config.unlink()
            else:
                config.write_bytes(content)
            require(cli_results(binary, {**env, INITIAL_ENV: str(config)}, socket_path=socket) == baseline,
                    "invalid or missing retired config changed CLI results")
        return {"cases": cases + ["malformed-config", "missing-config"],
                "valid_config": "retired Unix schema, exact private endpoint, 30-second process-local clock",
                "observation": "CLI equality, stable pane/child through resize, no trace or receipt writes"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--expected-sha256")
    parser.add_argument("--expected-git-commit")
    parser.add_argument("--expected-daemon-sha256", help="required when a sibling rmux-daemon exists")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--source-only", action="store_true", help="check product cutoff without invoking Cargo or a binary")
    parser.add_argument("--skip-cargo-feature-probes", action="store_true",
                        help="use manifest/source checks; do not run the two optional Cargo rejection probes")
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    args = parser.parse_args()
    source = check_product_cutoff()
    if args.source_only:
        print(json.dumps(source, indent=2))
        return
    require(all((args.binary, args.expected_sha256, args.expected_git_commit, args.output_dir)),
            "binary mode requires --binary, --expected-sha256, --expected-git-commit and --output-dir")
    binary = args.binary.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    require(digest == args.expected_sha256, "binary SHA-256 mismatch")
    daemon = binary.with_name("rmux-daemon")
    daemon_digest = hashlib.sha256(daemon.read_bytes()).hexdigest() if daemon.is_file() else None
    require(daemon_digest == args.expected_daemon_sha256,
            "sibling rmux-daemon must match --expected-daemon-sha256 (omit only when absent)")
    feature_rejections = (None if args.skip_cargo_feature_probes else check_features(args.cargo))

    env = dict(os.environ)
    for name in (LEGACY_ENV, INITIAL_ENV, "RMUX", "TMUX", "TERM_PROGRAM", "ZELLIJ",
                 "RMUX_INTERNAL_BINARY_PATH", "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE"):
        env.pop(name, None)
    env["RMUX_PERF_EXPECTED_GIT_SHA"] = args.expected_git_commit
    env["RMUX_PERF_EXPECTED_PROVENANCE"] = "perf-instrument-cutoff-correctness-smoke"
    # Reuse the existing clean-source, expected-SHA/platform and binary identity checks.
    identity = run(["bash", "-c", '''set -euo pipefail
source "$1"
rmux_perf_capture_current_identity "$2" 1
printf '%s\\n' "$RMUX_PERF_GIT_COMMIT" "$RMUX_PERF_BINARY_SHA256" "$RMUX_PERF_PLATFORM"
''', "perf-cutoff", ROOT / "scripts/perf-provenance.sh", binary], env=env).stdout.decode().splitlines()
    require(identity[0] == args.expected_git_commit and identity[1] == digest, "provenance mismatch")

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    pane_samples = []
    initial_env = check_initial_env(binary, env, output, pane_samples)
    baseline = cli_results(binary, env)
    # Fresh and existing paths distinguish creation from append/truncate side effects.
    with tempfile.TemporaryDirectory(prefix="legacy-env-", dir=output) as directory:
        directory = Path(directory)
        fresh = directory / "fresh.jsonl"
        existing = directory / "existing.jsonl"
        sentinel = b"preserve-existing-trace\n"
        existing.write_bytes(sentinel)
        for path in (fresh, existing, directory / "missing-parent" / "trace.jsonl"):
            legacy = {**env, LEGACY_ENV: str(path)}
            require(cli_results(binary, legacy) == baseline, "legacy env changed CLI results")
        require(not fresh.exists(), "legacy env created a product trace")
        require(existing.read_bytes() == sentinel, "legacy env modified an existing file")
        require(not (directory / "missing-parent").exists(), "legacy env created a directory")

        # The supported custom-binary collector executes the actual local process/RPC
        # operations. Its smoke samples must never be relabelled as internal spans.
        measured = output / "external-smoke.json"
        run([sys.executable, ROOT / "scripts/bench/bench_unix.py", "--binary", binary,
             "--out", measured, "--iterations", "1", "--only-tools", "rmux",
             "--only-operations", ",".join(OPERATIONS),
             "--rmux-layout", "perf-cutoff-correctness-smoke", "--quiet"],
            env={**env, LEGACY_ENV: str(fresh)})
        data = json.loads(measured.read_text())
        require(data["complete"] and not data["notes"], "external measurement failed or omitted samples")
        require(data["git"]["commit"] == identity[0], "collector source mismatch")
        require(data["tools"] == ["rmux"], "unexpected measured tools")
        require([row["id"] for row in data["operations"]] == OPERATIONS, "wrong operations")
        for row in data["operations"]:
            samples = row["metrics"]["rmux"]["samples_ms"]
            require(len(samples) == 1 and math.isfinite(samples[0]) and samples[0] >= 0,
                    "missing or invalid external sample")
        require(not fresh.exists(), "external harness activated product tracing")

    require(hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "binary changed during fixture")
    final_daemon_digest = hashlib.sha256(daemon.read_bytes()).hexdigest() if daemon.is_file() else None
    require(final_daemon_digest == daemon_digest, "sibling daemon changed during fixture")
    (output / "contract.json").write_text(json.dumps({
        "kind": "perf-instrument-cutoff-correctness-smoke", "git_commit": identity[0],
        "binary_sha256": digest, "daemon_sha256": daemon_digest, "platform": identity[2], "cli_cases_per_env": len(baseline),
        "cli_results_sha256": hashlib.sha256(repr(baseline).encode()).hexdigest(),
        "source_cutoff": source, "feature_rejections": feature_rejections,
        "initial_env": initial_env, "initial_pane_external_observations": pane_samples,
        "legacy_env_cases": ["unset", "fresh-file", "existing-file", "missing-parent"],
        "external_operations": OPERATIONS, "samples_per_operation": 1,
        "measurement": "external process/RPC wall time; not internal spans or release evidence",
    }, indent=2) + "\n")
    print(f"PASS: retired controls and executed external migration ({output / 'contract.json'})")


if __name__ == "__main__":
    main()
