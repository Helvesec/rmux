#!/usr/bin/env python3
"""Validate one provenance-bound current performance artifact."""

from __future__ import annotations

import argparse
import math
import sys
import tempfile
from pathlib import Path

from perf_artifact import (
    load_payload,
    sha256_file,
    validate_current_identity,
    validate_release_layout,
)


FIXED_METRICS = {
    "diagnose_json_cold",
    "daemon_startup",
    "new_session_detached_sh",
    "split_window_detached_sh",
    "send_keys_detached_round_trip",
    "resize_pane_round_trip",
    "status_format_heavy_expand",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("current", nargs="?", type=Path)
    parser.add_argument("--expected-current-commit")
    parser.add_argument("--expected-current-platform")
    parser.add_argument("--expected-current-machine")
    parser.add_argument("--expected-current-host-fingerprint")
    parser.add_argument("--expected-current-provenance")
    parser.add_argument("--expected-current-binary-version")
    parser.add_argument("--max-current-age-seconds", type=int)
    parser.add_argument("--require-absolute-budgets", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args()


def positive_parameter(parameters: object, name: str) -> int:
    if not isinstance(parameters, dict):
        raise ValueError("current perf artifact is missing its parameters object")
    value = parameters.get(name)
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"current perf artifact has invalid parameters.{name}")
    return value


def required_metric_names(source: dict[str, object]) -> set[str]:
    parameters = source.get("parameters")
    line_count = positive_parameter(parameters, "line_count")
    source_commands = positive_parameter(parameters, "source_command_count")
    hook_events = positive_parameter(parameters, "hook_storm_events")
    churn_cycles = positive_parameter(parameters, "daemon_churn_cycles")
    return FIXED_METRICS | {
        f"pane_output_{line_count}_lines_ready",
        f"capture_pane_{line_count}_lines",
        f"attach_render_{line_count}_line_scrollback",
        f"source_file_{source_commands}_commands",
        f"hook_storm_{hook_events}_after_set_option",
        f"daemon_churn_{churn_cycles}_create_kill",
    }


def validate_absolute_budgets(source: dict[str, object]) -> None:
    metrics = source.get("metrics")
    if not isinstance(metrics, list):
        raise ValueError("current perf artifact is missing its metrics array")
    by_name: dict[str, dict[str, object]] = {}
    for metric in metrics:
        if not isinstance(metric, dict):
            raise ValueError("current perf artifact contains a non-object metric")
        name = metric.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError("current perf artifact contains an unnamed metric")
        if name in by_name:
            raise ValueError(f"current perf artifact contains duplicate metric {name}")
        by_name[name] = metric

    expected = required_metric_names(source)
    missing = sorted(expected.difference(by_name))
    unexpected = sorted(set(by_name).difference(expected))
    if missing or unexpected:
        raise ValueError(
            "current perf metric inventory mismatch: "
            f"missing={missing or '<none>'} unexpected={unexpected or '<none>'}"
        )

    for name in sorted(expected):
        metric = by_name[name]
        try:
            p95 = float(metric["p95_ms"])
            limit = float(metric["budget_p95_ms"])
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"metric {name} has an invalid absolute budget") from error
        if not math.isfinite(p95) or p95 < 0 or not math.isfinite(limit) or limit <= 0:
            raise ValueError(f"metric {name} has a non-finite or non-positive budget value")
        if metric.get("status") != "pass" or p95 > limit:
            raise ValueError(
                f"metric {name} exceeded its absolute p95 budget: p95={p95} budget={limit}"
            )




def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="rmux-perf-current-") as directory:
        root = Path(directory) / "release"
        public = root / "rmux"
        helper = root / "libexec" / "rmux" / "rmux"
        daemon = root / "rmux-daemon"
        helper.parent.mkdir(parents=True)
        public.write_bytes(b"tiny")
        helper.write_bytes(b"private-helper")
        daemon.write_bytes(b"daemon")

        def member(path: Path) -> dict[str, object]:
            return {
                "path": str(path),
                "sha256": sha256_file(path),
                "size_bytes": path.stat().st_size,
            }

        parameters = {
            "iterations": 1,
            "line_count": 10,
            "source_command_count": 2,
            "hook_storm_events": 3,
            "daemon_churn_cycles": 4,
        }
        source: dict[str, object] = {
            "parameters": parameters,
            "layout": {
                "schema": 1,
                "public": member(public),
                "helper": member(helper),
                "daemon": member(daemon),
            },
        }
        source["binary"] = {
            **member(public),
            "version": "rmux 0.9.0",
            "configuration": "release",
        }
        source["metrics"] = [
            {
                "name": name,
                "p95_ms": 1,
                "budget_p95_ms": 2,
                "status": "pass",
            }
            for name in sorted(required_metric_names(source))
        ]
        validate_release_layout(source)
        validate_absolute_budgets(source)

        metrics = source["metrics"]
        assert isinstance(metrics, list)
        removed = metrics.pop()
        try:
            validate_absolute_budgets(source)
        except ValueError:
            metrics.append(removed)
        else:
            print("error: incomplete metric inventory was accepted", file=sys.stderr)
            return 1
        public.write_bytes(b"tampered")
        try:
            validate_release_layout(source)
        except ValueError:
            print("perf-current-validator-self-test=ok")
            return 0
        print("error: tampered release layout was accepted", file=sys.stderr)
        return 1


def main() -> int:
    args = parse_args()
    if args.self_test:
        return self_test()
    required = {
        "current": args.current,
        "expected current commit": args.expected_current_commit,
        "expected current platform": args.expected_current_platform,
        "expected current machine": args.expected_current_machine,
        "expected current host fingerprint": args.expected_current_host_fingerprint,
        "expected current provenance": args.expected_current_provenance,
        "expected current binary version": args.expected_current_binary_version,
        "max current age": args.max_current_age_seconds,
    }
    missing = [label for label, value in required.items() if value is None]
    if missing:
        print(f"error: missing required arguments: {', '.join(missing)}", file=sys.stderr)
        return 2
    assert args.current is not None
    assert args.max_current_age_seconds is not None
    try:
        _payload, source, identity = load_payload(args.current)
        validate_current_identity(
            identity,
            expected_commit=args.expected_current_commit,
            expected_platform=args.expected_current_platform,
            expected_machine=args.expected_current_machine,
            expected_host_fingerprint=args.expected_current_host_fingerprint,
            expected_provenance=args.expected_current_provenance,
            expected_binary_version=args.expected_current_binary_version,
            max_age_seconds=args.max_current_age_seconds,
        )
        validate_release_layout(source)
        if args.require_absolute_budgets:
            validate_absolute_budgets(source)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"perf-current={args.current} status=valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
