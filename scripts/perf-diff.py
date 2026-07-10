#!/usr/bin/env python3
"""Compare RMUX perf JSON artifacts.

This is intentionally dependency-free so it can run in release worktrees and CI
without preparing a Python environment. It accepts the provenance-bound schema-2
output from scripts/perf-bench.sh and its schema-2 baseline wrapper.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
import tempfile
from dataclasses import dataclass, replace
from datetime import datetime, timedelta, timezone
from pathlib import Path

from perf_artifact import (
    ArtifactIdentity,
    CURRENT_GENERATOR,
    CURRENT_KIND,
    CURRENT_SCHEMA,
    load_payload,
    sha256_file,
    validate_current_identity,
)


DEFAULT_ALPHA = 0.01
DEFAULT_REGRESSION_PCT = 10.0
DEFAULT_METHOD = "auto"


@dataclass(frozen=True)
class Metric:
    name: str
    samples_ms: tuple[float, ...]
    p50_ms: float
    p95_ms: float
    mean_ms: float


@dataclass(frozen=True)
class Artifact:
    path: Path
    identity: ArtifactIdentity
    metrics: dict[str, Metric]


@dataclass(frozen=True)
class MetricDiff:
    name: str
    base: Metric
    current: Metric
    mean_delta_pct: float
    p50_delta_pct: float
    p95_delta_pct: float
    welch_t: float
    welch_df: float
    method: str
    p_value: float
    status: str


def load_artifact(path: Path) -> Artifact:
    _, source, identity = load_payload(path)
    metrics = source.get("metrics")
    if not isinstance(metrics, list):
        raise ValueError(f"{path}: missing metrics array")

    result: dict[str, Metric] = {}
    for item in metrics:
        if not isinstance(item, dict):
            raise ValueError(f"{path}: metric entry is not an object")
        name = str(item["name"])
        samples = tuple(float(value) for value in item.get("samples_ms", ()))
        if len(samples) < 2:
            raise ValueError(f"{path}: metric {name} needs at least two samples")
        result[name] = Metric(
            name=name,
            samples_ms=samples,
            p50_ms=float(item["p50_ms"]),
            p95_ms=float(item["p95_ms"]),
            mean_ms=float(item.get("mean_ms", statistics.fmean(samples))),
        )

    return Artifact(
        path=path,
        identity=identity,
        metrics=result,
    )


def validate_artifact_pair(baseline: Artifact, current: Artifact) -> None:
    if baseline.path.resolve() == current.path.resolve():
        raise ValueError("baseline and current perf JSON paths must be distinct")
    if baseline.identity.kind != "rmux-perf-baseline":
        raise ValueError(f"{baseline.path}: baseline kind must be rmux-perf-baseline")
    if baseline.identity.platform is None:
        raise ValueError(f"{baseline.path}: missing platform metadata")
    if current.identity.platform is None:
        raise ValueError(f"{current.path}: missing platform metadata")
    if baseline.identity.platform != current.identity.platform:
        raise ValueError(
            "perf platform mismatch: "
            f"baseline={baseline.identity.platform} current={current.identity.platform}; "
            "regenerate a same-platform baseline/current pair"
        )
    if baseline.identity.machine is None or current.identity.machine is None:
        raise ValueError("baseline and current perf artifacts must include machine metadata")
    if baseline.identity.machine != current.identity.machine:
        raise ValueError(
            "perf machine mismatch: "
            f"baseline={baseline.identity.machine} current={current.identity.machine}; "
            "regenerate a same-machine baseline/current pair"
        )
    if baseline.identity.host_fingerprint is None or current.identity.host_fingerprint is None:
        raise ValueError("baseline and current perf artifacts must include host fingerprints")
    if baseline.identity.host_fingerprint != current.identity.host_fingerprint:
        raise ValueError(
            "perf host fingerprint mismatch: "
            f"baseline={baseline.identity.host_fingerprint} "
            f"current={current.identity.host_fingerprint}; regenerate on the baseline owner host"
        )


def load_metrics(path: Path) -> dict[str, Metric]:
    return load_artifact(path).metrics


def pct_delta(base: float, current: float) -> float:
    if base == 0:
        return 0.0 if current == 0 else math.inf
    return ((current - base) / base) * 100.0


def normal_two_tailed_p(abs_t: float) -> float:
    # Normal approximation to the two-tailed t-test p-value. This is accurate
    # enough for the intended n>=30 PR0D gate and conservative for local smoke.
    return math.erfc(abs_t / math.sqrt(2.0))


def welch_statistic(base: Metric, current: Metric) -> tuple[float, float, float]:
    n1 = len(base.samples_ms)
    n2 = len(current.samples_ms)
    mean1 = statistics.fmean(base.samples_ms)
    mean2 = statistics.fmean(current.samples_ms)
    var1 = statistics.variance(base.samples_ms)
    var2 = statistics.variance(current.samples_ms)
    scaled1 = var1 / n1
    scaled2 = var2 / n2
    denom = math.sqrt(scaled1 + scaled2)
    if denom == 0:
        if mean1 == mean2:
            return 0.0, math.inf, 1.0
        return math.inf, math.inf, 0.0
    t_value = (mean2 - mean1) / denom
    df_denom = 0.0
    if n1 > 1 and scaled1:
        df_denom += (scaled1 * scaled1) / (n1 - 1)
    if n2 > 1 and scaled2:
        df_denom += (scaled2 * scaled2) / (n2 - 1)
    df = ((scaled1 + scaled2) ** 2) / df_denom if df_denom else math.inf
    return t_value, df, normal_two_tailed_p(abs(t_value))


def mann_whitney_p_value(base: Metric, current: Metric) -> float:
    n1 = len(base.samples_ms)
    n2 = len(current.samples_ms)
    combined = [(value, 0) for value in base.samples_ms] + [
        (value, 1) for value in current.samples_ms
    ]
    combined.sort(key=lambda item: item[0])

    rank_sum_base = 0.0
    tie_sum = 0.0
    index = 0
    while index < len(combined):
        end = index + 1
        while end < len(combined) and combined[end][0] == combined[index][0]:
            end += 1
        average_rank = (index + 1 + end) / 2.0
        tie_len = end - index
        tie_sum += tie_len**3 - tie_len
        for _, group in combined[index:end]:
            if group == 0:
                rank_sum_base += average_rank
        index = end

    u_base = rank_sum_base - (n1 * (n1 + 1) / 2.0)
    u_current = (n1 * n2) - u_base
    u_value = min(u_base, u_current)
    mean_u = n1 * n2 / 2.0
    total = n1 + n2
    if total < 2:
        return 1.0
    tie_correction = tie_sum / (total * (total - 1))
    variance = n1 * n2 * ((total + 1) - tie_correction) / 12.0
    if variance <= 0.0:
        return 1.0 if u_value == mean_u else 0.0
    z_value = (u_value - mean_u) / math.sqrt(variance)
    return normal_two_tailed_p(abs(z_value))


def is_tail_heavy(metric: Metric) -> bool:
    if metric.p50_ms <= 0:
        return False
    return metric.p95_ms / metric.p50_ms >= 3.0


def select_p_value(base: Metric, current: Metric, method: str) -> tuple[str, float, float, float]:
    t_value, df, welch_p = welch_statistic(base, current)
    if method == "welch":
        return "welch", t_value, df, welch_p
    if method == "mann-whitney":
        return "mann-whitney", t_value, df, mann_whitney_p_value(base, current)
    if method != "auto":
        raise ValueError(f"unknown comparison method: {method}")
    if is_tail_heavy(base) or is_tail_heavy(current):
        return "mann-whitney", t_value, df, mann_whitney_p_value(base, current)
    return "welch", t_value, df, welch_p


def compare_metrics(
    base_metrics: dict[str, Metric],
    current_metrics: dict[str, Metric],
    *,
    alpha: float,
    regression_pct: float,
    method: str,
) -> list[MetricDiff]:
    diffs: list[MetricDiff] = []
    missing_names = sorted(set(base_metrics) - set(current_metrics))
    if missing_names:
        raise ValueError(
            "current perf JSON is missing baseline metrics: "
            + ", ".join(missing_names)
            + "; a partial current run cannot stand in for the full benchmark"
        )
    common_names = sorted(set(base_metrics) & set(current_metrics))
    if not common_names:
        raise ValueError("no metric names overlap between inputs")

    for name in common_names:
        base = base_metrics[name]
        current = current_metrics[name]
        mean_delta = pct_delta(base.mean_ms, current.mean_ms)
        p50_delta = pct_delta(base.p50_ms, current.p50_ms)
        p95_delta = pct_delta(base.p95_ms, current.p95_ms)
        metric_method, t_value, df, p_value = select_p_value(base, current, method)
        significant = p_value <= alpha
        if significant and p95_delta >= regression_pct:
            status = "regression"
        elif significant and p95_delta <= -regression_pct:
            status = "improvement"
        else:
            status = "neutral"
        diffs.append(
            MetricDiff(
                name=name,
                base=base,
                current=current,
                mean_delta_pct=mean_delta,
                p50_delta_pct=p50_delta,
                p95_delta_pct=p95_delta,
                welch_t=t_value,
                welch_df=df,
                method=metric_method,
                p_value=p_value,
                status=status,
            )
        )
    return diffs


def format_float(value: float, digits: int = 3) -> str:
    if math.isinf(value):
        return "inf"
    return f"{value:.{digits}f}"


def print_table(diffs: list[MetricDiff]) -> None:
    print(
        "| Metric | base p50 | current p50 | p50 delta | base p95 | current p95 | p95 delta | p approx | Status |"
    )
    print("|---|---:|---:|---:|---:|---:|---:|---:|---|")
    for diff in diffs:
        print(
            "| {name} | {bp50:.3f} | {cp50:.3f} | {dp50:+.1f}% | "
            "{bp95:.3f} | {cp95:.3f} | {dp95:+.1f}% | {p} | {status} |".format(
                name=diff.name,
                bp50=diff.base.p50_ms,
                cp50=diff.current.p50_ms,
                dp50=diff.p50_delta_pct,
                bp95=diff.base.p95_ms,
                cp95=diff.current.p95_ms,
                dp95=diff.p95_delta_pct,
                p=format_float(diff.p_value, 4),
                status=diff.status,
            )
        )


def write_json(path: Path, diffs: list[MetricDiff], *, alpha: float, regression_pct: float) -> None:
    payload = {
        "schema": 1,
        "kind": "rmux-perf-diff",
        "method": "auto-welch-or-mann-whitney",
        "alpha": alpha,
        "regression_pct": regression_pct,
        "metrics": [
            {
                "name": diff.name,
                "status": diff.status,
                "base": {
                    "p50_ms": diff.base.p50_ms,
                    "p95_ms": diff.base.p95_ms,
                    "mean_ms": diff.base.mean_ms,
                    "samples": len(diff.base.samples_ms),
                },
                "current": {
                    "p50_ms": diff.current.p50_ms,
                    "p95_ms": diff.current.p95_ms,
                    "mean_ms": diff.current.mean_ms,
                    "samples": len(diff.current.samples_ms),
                },
                "delta_pct": {
                    "mean": diff.mean_delta_pct,
                    "p50": diff.p50_delta_pct,
                    "p95": diff.p95_delta_pct,
                },
                "welch": {
                    "t": diff.welch_t,
                    "df": diff.welch_df,
                    "p_approx": diff.p_value,
                },
                "test_method": diff.method,
            }
            for diff in diffs
        ],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def synthetic_metric(name: str, samples: list[float]) -> Metric:
    ordered = sorted(samples)
    p50 = statistics.median(ordered)
    p95_index = min(len(ordered) - 1, math.ceil(len(ordered) * 0.95) - 1)
    return Metric(
        name=name,
        samples_ms=tuple(samples),
        p50_ms=p50,
        p95_ms=ordered[p95_index],
        mean_ms=statistics.fmean(samples),
    )


def run_self_test() -> None:
    unchanged_base = synthetic_metric("stable", [10.0 + (i % 3) * 0.1 for i in range(40)])
    unchanged_current = synthetic_metric("stable", [10.0 + (i % 3) * 0.1 for i in range(40)])
    regressed_base = synthetic_metric("slow", [10.0 + (i % 5) * 0.1 for i in range(40)])
    regressed_current = synthetic_metric("slow", [13.0 + (i % 5) * 0.1 for i in range(40)])
    improved_base = synthetic_metric("fast", [10.0 + (i % 5) * 0.1 for i in range(40)])
    improved_current = synthetic_metric("fast", [7.0 + (i % 5) * 0.1 for i in range(40)])
    diffs = compare_metrics(
        {
            "stable": unchanged_base,
            "slow": regressed_base,
            "fast": improved_base,
        },
        {
            "stable": unchanged_current,
            "slow": regressed_current,
            "fast": improved_current,
        },
        alpha=DEFAULT_ALPHA,
        regression_pct=DEFAULT_REGRESSION_PCT,
        method=DEFAULT_METHOD,
    )
    by_name = {diff.name: diff for diff in diffs}
    assert by_name["stable"].status == "neutral", by_name["stable"]
    assert by_name["slow"].status == "regression", by_name["slow"]
    assert by_name["fast"].status == "improvement", by_name["fast"]

    shifted_base = synthetic_metric("shifted", [float(i) for i in range(1, 41)])
    shifted_current = synthetic_metric("shifted", [float(i) for i in range(20, 60)])
    assert mann_whitney_p_value(shifted_base, shifted_current) < DEFAULT_ALPHA

    tail_base = synthetic_metric("tail", [10.0] * 35 + [100.0] * 5)
    tail_current = synthetic_metric("tail", [10.0] * 35 + [150.0] * 5)
    method, _, _, _ = select_p_value(tail_base, tail_current, DEFAULT_METHOD)
    assert method == "mann-whitney"

    try:
        compare_metrics(
            {"stable": unchanged_base, "slow": regressed_base},
            {"stable": unchanged_current},
            alpha=DEFAULT_ALPHA,
            regression_pct=DEFAULT_REGRESSION_PCT,
            method=DEFAULT_METHOD,
        )
    except ValueError as error:
        assert "missing baseline metrics: slow" in str(error), error
    else:
        raise AssertionError("partial current with missing baseline metrics must fail")

    base_identity = synthetic_identity(
        Path("baseline.json"), kind="rmux-perf-baseline", platform="darwin", machine="arm64"
    )
    current_identity = synthetic_identity(
        Path("current.json"), kind=CURRENT_KIND, platform="linux", machine="x86_64"
    )
    base_artifact = Artifact(Path("baseline.json"), base_identity, {"stable": unchanged_base})
    current_artifact = Artifact(Path("current.json"), current_identity, {"stable": unchanged_current})
    try:
        validate_artifact_pair(base_artifact, current_artifact)
    except ValueError as error:
        assert "platform mismatch" in str(error)
    else:
        raise AssertionError("platform mismatch must fail closed")

    run_identity_self_test()


def synthetic_identity(path: Path, *, kind: str, platform: str, machine: str) -> ArtifactIdentity:
    return ArtifactIdentity(
        path=path,
        schema=CURRENT_SCHEMA,
        kind=kind,
        timestamp="2026-07-10T00:00:00Z",
        platform=platform,
        machine=machine,
        host_fingerprint="host-a",
        git_commit="a" * 40,
        git_describe="v0.9.0",
        git_dirty=False,
        binary_path="/tmp/rmux",
        binary_sha256="b" * 64,
        binary_version="rmux 0.9.0",
        binary_configuration="release",
        provenance_generator=CURRENT_GENERATOR,
        provenance_invocation="self-test",
        provenance_expected_commit="a" * 40,
        provenance_expected_platform=platform,
        provenance_build_mode="rebuilt",
    )


def run_identity_self_test() -> None:
    now = datetime(2026, 7, 10, tzinfo=timezone.utc)
    with tempfile.TemporaryDirectory(prefix="rmux-perf-artifact-") as directory:
        binary = Path(directory) / "rmux"
        binary.write_bytes(b"release-binary")
        current = replace(
            synthetic_identity(binary.with_suffix(".json"), kind=CURRENT_KIND, platform="linux", machine="x86_64"),
            timestamp=now.isoformat().replace("+00:00", "Z"),
            binary_path=str(binary.resolve()),
            binary_sha256=sha256_file(binary),
        )
        validation_args = {
            "expected_commit": "a" * 40,
            "expected_platform": "linux",
            "expected_machine": "x86_64",
            "expected_host_fingerprint": "host-a",
            "expected_provenance": "self-test",
            "expected_binary_version": "rmux 0.9.0",
            "max_age_seconds": 3600,
            "now": now,
        }
        validate_current_identity(current, **validation_args)

        invalid_cases = [
            (replace(current, git_commit="c" * 40), "Git SHA mismatch"),
            (replace(current, git_dirty=True), "dirty worktree"),
            (replace(current, host_fingerprint="host-b"), "host fingerprint mismatch"),
            (replace(current, provenance_invocation="stale-run"), "provenance invocation mismatch"),
            (replace(current, provenance_build_mode="reused"), "build mode mismatch"),
            (
                replace(current, timestamp=(now - timedelta(hours=2)).isoformat()),
                "artifact is stale",
            ),
            (replace(current, binary_sha256="d" * 64), "binary digest mismatch"),
        ]
        for invalid, expected_error in invalid_cases:
            try:
                validate_current_identity(invalid, **validation_args)
            except ValueError as error:
                assert expected_error in str(error), (expected_error, error)
            else:
                raise AssertionError(f"invalid current identity did not fail: {expected_error}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", nargs="?", type=Path)
    parser.add_argument("current", nargs="?", type=Path)
    parser.add_argument("--alpha", type=float, default=DEFAULT_ALPHA)
    parser.add_argument("--regression-pct", type=float, default=DEFAULT_REGRESSION_PCT)
    parser.add_argument(
        "--method",
        choices=("auto", "welch", "mann-whitney"),
        default=DEFAULT_METHOD,
    )
    parser.add_argument("--json-out", type=Path)
    parser.add_argument("--fail-on-regression", action="store_true")
    parser.add_argument("--expected-current-commit")
    parser.add_argument("--expected-current-platform")
    parser.add_argument("--expected-current-machine")
    parser.add_argument("--expected-current-host-fingerprint")
    parser.add_argument("--expected-current-provenance")
    parser.add_argument("--expected-current-binary-version")
    parser.add_argument("--max-current-age-seconds", type=int, default=21600)
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        run_self_test()
        return 0
    if args.baseline is None or args.current is None:
        raise SystemExit("baseline and current JSON paths are required")
    if args.alpha <= 0.0 or args.alpha >= 1.0:
        raise SystemExit("--alpha must be between 0 and 1")
    if args.regression_pct < 0.0:
        raise SystemExit("--regression-pct must be non-negative")

    baseline = load_artifact(args.baseline)
    current = load_artifact(args.current)
    required_identity_args = {
        "--expected-current-commit": args.expected_current_commit,
        "--expected-current-platform": args.expected_current_platform,
        "--expected-current-machine": args.expected_current_machine,
        "--expected-current-host-fingerprint": args.expected_current_host_fingerprint,
        "--expected-current-provenance": args.expected_current_provenance,
        "--expected-current-binary-version": args.expected_current_binary_version,
    }
    missing = [name for name, value in required_identity_args.items() if not value]
    if missing:
        raise SystemExit("missing required current identity option(s): " + ", ".join(missing))
    validate_current_identity(
        current.identity,
        expected_commit=args.expected_current_commit,
        expected_platform=args.expected_current_platform,
        expected_machine=args.expected_current_machine,
        expected_host_fingerprint=args.expected_current_host_fingerprint,
        expected_provenance=args.expected_current_provenance,
        expected_binary_version=args.expected_current_binary_version,
        max_age_seconds=args.max_current_age_seconds,
    )
    validate_artifact_pair(baseline, current)
    diffs = compare_metrics(
        baseline.metrics,
        current.metrics,
        alpha=args.alpha,
        regression_pct=args.regression_pct,
        method=args.method,
    )
    print_table(diffs)
    if args.json_out is not None:
        write_json(args.json_out, diffs, alpha=args.alpha, regression_pct=args.regression_pct)
    if args.fail_on_regression and any(diff.status == "regression" for diff in diffs):
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
    except BrokenPipeError:
        raise SystemExit(1)
