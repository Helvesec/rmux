#!/usr/bin/env python3
"""Validate one provenance-bound current performance artifact."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from perf_artifact import load_payload, validate_current_identity


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


def validate_absolute_budgets(source: dict[str, object]) -> None:
    metrics = source.get("metrics")
    if not isinstance(metrics, list):
        raise ValueError("current perf artifact is missing its metrics array")
    checked = 0
    for metric in metrics:
        if not isinstance(metric, dict):
            raise ValueError("current perf artifact contains a non-object metric")
        budget = metric.get("budget_p95_ms")
        if budget is None:
            continue
        checked += 1
        name = str(metric.get("name", "<unnamed>"))
        try:
            p95 = float(metric["p95_ms"])
            limit = float(budget)
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"metric {name} has an invalid absolute budget") from error
        if metric.get("status") != "pass" or p95 > limit:
            raise ValueError(
                f"metric {name} exceeded its absolute p95 budget: p95={p95} budget={limit}"
            )
    if checked < 6:
        raise ValueError(
            f"current perf artifact has only {checked} absolute budgets; expected at least 6"
        )


def main() -> int:
    args = parse_args()
    if args.self_test:
        passing = {
            "metrics": [
                {
                    "name": f"metric-{index}",
                    "p95_ms": 1,
                    "budget_p95_ms": 2,
                    "status": "pass",
                }
                for index in range(6)
            ]
        }
        validate_absolute_budgets(passing)
        passing["metrics"][0]["status"] = "fail"
        try:
            validate_absolute_budgets(passing)
        except ValueError:
            print("perf-current-validator-self-test=ok")
            return 0
        print("error: failing absolute budget was accepted", file=sys.stderr)
        return 1
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
        if args.require_absolute_budgets:
            validate_absolute_budgets(source)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"perf-current={args.current} status=valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
