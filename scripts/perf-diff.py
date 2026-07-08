#!/usr/bin/env python3
"""Compare RMUX perf JSON artifacts.

This is intentionally dependency-free so it can run in release worktrees and CI
without preparing a Python environment. It accepts the schema-1 output from
scripts/perf-bench.sh and the schema-2 wrapper output from scripts/perf-baseline.sh.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path


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
    schema: object
    kind: str | None
    platform: str | None
    machine: str | None
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
    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)

    source = payload.get("source", payload)
    if not isinstance(source, dict):
        raise ValueError(f"{path}: source payload is not an object")
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

    environment = payload.get("environment") if isinstance(payload, dict) else None
    if not isinstance(environment, dict):
        environment = {}
    platform = first_non_empty(environment.get("platform"), source.get("platform"), payload.get("platform"))
    machine = first_non_empty(environment.get("machine"), source.get("machine"), payload.get("machine"))
    return Artifact(
        path=path,
        schema=payload.get("schema"),
        kind=payload.get("kind"),
        platform=platform,
        machine=machine,
        metrics=result,
    )


def first_non_empty(*values: object) -> str | None:
    for value in values:
        if isinstance(value, str) and value.strip():
            return value.strip().lower()
    return None


def validate_artifact_pair(baseline: Artifact, current: Artifact) -> None:
    if baseline.path.resolve() == current.path.resolve():
        raise ValueError("baseline and current perf JSON paths must be distinct")
    if current.kind == "rmux-perf-baseline":
        raise ValueError("current perf JSON must be a fresh perf-bench artifact, not a baseline wrapper")
    if baseline.platform is None:
        raise ValueError(f"{baseline.path}: missing platform metadata")
    if current.platform is None:
        raise ValueError(f"{current.path}: missing platform metadata")
    if baseline.platform != current.platform:
        raise ValueError(
            "perf platform mismatch: "
            f"baseline={baseline.platform} current={current.platform}; regenerate a same-platform baseline/current pair"
        )
    if baseline.machine is not None and current.machine is not None and baseline.machine != current.machine:
        raise ValueError(
            "perf machine mismatch: "
            f"baseline={baseline.machine} current={current.machine}; regenerate a same-machine baseline/current pair"
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

    base_artifact = Artifact(
        path=Path("baseline.json"),
        schema=2,
        kind="rmux-perf-baseline",
        platform="darwin",
        machine="arm64",
        metrics={"stable": unchanged_base},
    )
    current_artifact = Artifact(
        path=Path("current.json"),
        schema=1,
        kind=None,
        platform="linux",
        machine="x86_64",
        metrics={"stable": unchanged_current},
    )
    try:
        validate_artifact_pair(base_artifact, current_artifact)
    except ValueError as error:
        assert "platform mismatch" in str(error)
    else:
        raise AssertionError("platform mismatch must fail closed")

    baseline_as_current = Artifact(
        path=Path("current.json"),
        schema=2,
        kind="rmux-perf-baseline",
        platform="darwin",
        machine="arm64",
        metrics={"stable": unchanged_current},
    )
    try:
        validate_artifact_pair(base_artifact, baseline_as_current)
    except ValueError as error:
        assert "not a baseline wrapper" in str(error)
    else:
        raise AssertionError("baseline wrapper used as current must fail closed")


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
