"""R5-B population, deterministic schedule and fail-closed attempt accounting.

This gate establishes accounting only, never functional or performance validity.
It has no process, timing, terminal or cleanup implementation of its own.
"""
from __future__ import annotations

import ast
import frozen_ast
from collections import Counter
from contextlib import contextmanager
from contextvars import ContextVar
import hashlib
import json
import math
import random
import statistics
import sys
import tool_identity
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import provenance_archive
import pin_revisions

MANIFEST = Path(__file__).resolve().parents[2] / "benches/perf/qualification/population.json"
_POPULATION_EPOCH = ContextVar("population_epoch", default=None)
BATCHES = {"batched_list_windows_20", "batched_list_panes_80", "batched_zellij_list_windows",
           "batched_zellij_list_panes", "batched_screen_list_windows"}
TOOL_ORDER = ("rmux", "tmux", "zellij", "screen")
IDENTITY_FIELDS = ("run_id", "operation", "tool", "pair", "order", "sequence",
                   "attempt_id", "attempt_number")


class QualificationError(RuntimeError):
    pass


def sample_statistics(samples: list[float], *, seed: int, resamples: int = 2000) -> dict[str, Any]:
    """Linear empirical quantiles and a seeded percentile bootstrap of the median.

    These descriptive values cannot qualify a noisy run or a small-sample tail.
    No floor is subtracted from the primary elapsed values.
    """
    if not samples or type(seed) is not int or not 100 <= resamples <= 10000:
        raise QualificationError("invalid statistical population or bootstrap policy")
    values = sorted(elapsed_value(value) for value in samples)

    def quantile(items, fraction):
        index = (len(items) - 1) * fraction
        lower = math.floor(index)
        return items[lower] + (items[math.ceil(index)] - items[lower]) * (index - lower)

    rng = random.Random(seed)
    boot = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(resamples))
    return {"n": len(values), "raw_ms": list(samples), "minimum_ms": min(values),
            "p50_ms": quantile(values, .5), "p95_ms": quantile(values, .95),
            "p99_ms": quantile(values, .99),
            "median_ci95_ms": [quantile(boot, .025), quantile(boot, .975)],
            "method": "linear empirical quantiles; percentile bootstrap median",
            "bootstrap_seed": seed, "bootstrap_resamples": resamples,
            "primary": "unadjusted_spawn_through_waitpid_ms",
            "tail_precision_claim": False}


def qualification_plan(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    """Qualification population, separately named from the public 33-cell run."""
    if manifest.get("schema") != "darwin-harness-qualification-v1":
        raise QualificationError("unknown qualification manifest schema")
    if manifest.get("kind") != "functional_qualification" or manifest.get("performance_claim") is not False:
        raise QualificationError("qualification entry cannot certify a comparative benchmark")
    if manifest.get("tool") not in ("tmux", "rmux") or not manifest.get("run_id"):
        raise QualificationError("missing exact qualification tool/run")
    operations = manifest["operations"]
    # This qualification recipe deliberately has both regimes. It does not
    # replace, shorten or certify the separately pinned public population.
    if [o["id"] for o in operations] != ["new_session_cold_sh", "new_session_warm_sh"]:
        raise QualificationError("qualification cold/warm population changed")
    count = manifest["samples_per_cell"]
    if type(count) is not int or not 4 <= count <= 128:
        raise QualificationError("qualification sample count outside frozen bounds")
    if manifest["timeout_seconds"] <= 0 or manifest["cleanup_seconds"] > 45:
        raise QualificationError("invalid qualification deadline")
    for op in operations:
        for field in ("regime", "setup", "reset", "timed_argv", "window", "postcondition"):
            if not op.get(field):
                raise QualificationError("manifest operation lacks " + field)
        if set(op["timed_argv"]) != {"rmux", "tmux"}:
            raise QualificationError("both products' exact argument templates required")
        if op["regime"] != ("cold" if "cold" in op["id"] else "warm"):
            raise QualificationError("regime relabelled")
    return [{"run_id": manifest["run_id"], "tool": manifest["tool"],
             "operation": op["id"], "regime": op["regime"], "sample": i + 1,
             "sequence": n * count + i + 1, "attempt_number": 1}
            for n, op in enumerate(operations) for i in range(count)]


def qualification_errors(payload: dict[str, Any], frozen_manifest_sha256: str,
                         *, require_r6: bool = True) -> list[str]:
    """One saved-artifact gate for real observations and intentional canaries.

    The caller supplies the prospective manifest digest, never one selected by
    the artifact being checked. Functional evidence and full R5/R6 closure are
    distinct: an unknown exec history can pass neither R6 nor a public claim.
    """
    errors = []
    try:
        manifest = payload["manifest"]
        if digest(manifest) != frozen_manifest_sha256:
            errors.append("frozen manifest changed")
        if require_r6 and (manifest.get("identity_observation") != "complete_exec_fork_history"
                           or not tool_identity.DARWIN_HISTORY_CAPABILITY["complete_exec_fork_history"]):
            errors.append("R6 frozen observer cannot attest complete transient execution history")
        if require_r6:
            errors.extend(quiet_window_errors(payload.get("quiet_window"), manifest,
                                              payload.get("attempts", [])))
        roles = manifest.get("source_role_graph")
        if roles is not None:
            errors.extend(role_graph_errors(manifest))
        expected = qualification_plan(manifest)
        attempts = payload["attempts"]
        if [a["identity"] for a in attempts] != expected:
            errors.append("missing, duplicate, reordered, relabelled or retried attempt")
        if payload.get("sampling_finished") is not True or payload.get("cleanup_finished") is not True:
            errors.append("run incomplete or cancelled")
        if payload.get("errors"):
            errors.append("run errors retained")
        challenges = []
        grouped: dict[str, list[float]] = {o["id"]: [] for o in manifest["operations"]}
        for a in attempts:
            label = str(a["identity"])
            if a.get("status") != "pass" or a.get("returncode") != 0 or a.get("timeout") is not False:
                errors.append("failed/timeout attempt: " + label)
                continue
            value = elapsed_value(a["elapsed_ms"])
            grouped[a["identity"]["operation"]].append(value)
            driver = a["driver"]
            if (driver.get("driver") != "darwin-posix-spawn-kqueue-waitpid"
                    or driver.get("polling") is not False or driver.get("raw_wait_status") != 0
                    or driver.get("reaped_pid") != a["client_pid"]
                    or not driver["prepared_ns"] <= driver["start_ns"] < driver["end_ns"]):
                errors.append("driver, timing, polling or actual wait differs: " + label)
            if a["environment_prepared_ns"] > driver["start_ns"]:
                errors.append("environment prepared inside measurement")
            if value != (driver["end_ns"] - driver["start_ns"]) / 1_000_000:
                errors.append("primary elapsed value differs from raw clock window")
            recipe = next(op for op in manifest["operations"] if op["id"] == a["identity"]["operation"])
            argv = [arg.format(**a["path_context"]) for arg in recipe["timed_argv"][a["identity"]["tool"]]]
            if a["timed_argv"] != argv:
                errors.append("measured operation/arguments differ from manifest")
            before, after = a["state_before"], a["state_after"]
            if a["identity"]["regime"] == "cold":
                if before["endpoint_present"] or before["daemon"] is not None:
                    errors.append("cold sample used residual daemon")
            elif (not before["endpoint_present"] or not before["daemon"]
                  or before["daemon"] != after["daemon"]):
                errors.append("warm daemon missing or changed")
            work = a["work"]
            challenges.append(work["challenge"])
            if (len(work["challenge"]) != 64 or work["acknowledged"] is not True
                    or any(c not in "0123456789abcdef" for c in work["challenge"])
                    or work["readiness"] != "fresh_fifo_kqueue" or work["session_count"] != 1
                    or work["pane_count"] != 1 or work["size"] != [80, 24]
                    or after["endpoint_present"] is not True
                    or work["daemon_pid"] != after["daemon"]["pid"]
                    or work["pane_pid"] != work["observed_pane"]["pid"]
                    or work["observed_pane"].get("status") != "observed_text_vnode"
                    or not work["observed_pane"].get("before")
                    or work["observed_pane"].get("before") != work["observed_pane"].get("after")):
                errors.append("independent fresh work postcondition failed")
            if (a["cleanup"].get("closed") is not True
                    or a["cleanup"].get("rescue") is not False
                    or a["cleanup"]["seconds"] > manifest["cleanup_seconds"]):
                errors.append("fixture closure missing, rescued or late")
            identity = a["image_identity"]
            if identity.get("disk_unchanged") is not True:
                errors.append("declared artifact changed")
            if require_r6:
                client_roles = roles["client_roles"] if roles else [manifest["tool"]]
                expected_clients = [manifest["code_directory_hashes"][r] for r in client_roles]
                if not any(e.get("status") == "observed_text_vnode"
                           and e.get("mapped_cdhash") in expected_clients
                           and e.get("before") == e.get("after") and e.get("before")
                           for e in identity.get("client_events", [])):
                    errors.append("R6 client text-vnode observation missing or differs")
                daemon_role = roles["daemon_role"] if roles else (
                    "rmux_daemon" if manifest["tool"] == "rmux" else "tmux")
                observed = identity.get("daemon", {})
                if (observed.get("status") != "observed_text_vnode"
                        or observed.get("mapped_cdhash") != manifest["code_directory_hashes"][daemon_role]
                        or observed.get("before") != after["daemon"]
                        or observed.get("after") != after["daemon"]):
                    errors.append("R6 daemon text-vnode observation missing or differs")
                if identity.get("all_measured_images_bound") is not True:
                    errors.append("R6 measured process image binding missing")
                if identity.get("exec_fork_history_complete") is not True:
                    errors.append("R6 transient helper/delegation history unknown")
                if identity.get("unexpected_images"):
                    errors.append("R6 undeclared executable/delegation")
            thermal = a["thermal"]
            if len(thermal) != 2 or any(type(x.get("state")) is not int or x["state"] not in (0, 1, 2, 3) for x in thermal):
                errors.append("thermal state observation missing")
        if len(challenges) != len(set(challenges)):
            errors.append("fresh-work challenge reused or fixed cache")
        for operation, values in grouped.items():
            if values and payload["statistics"].get(operation) != sample_statistics(
                    values, seed=manifest["statistics"]["seed"],
                    resamples=manifest["statistics"]["resamples"]):
                errors.append("published statistics differ from raw attempts")
        floor = payload["floor"]
        if (floor.get("diagnostic_only") is not True or not floor["raw_ms"]
                or any(elapsed_value(x) < 0 for x in floor["raw_ms"])):
            errors.append("floor missing or promoted to primary")
        if require_r6 and payload.get("installed_package_bound") is not True:
            errors.append("R6 installed package provenance missing")
        if payload.get("performance_claim") is not False:
            errors.append("comparative performance not qualified by this entry")
    except (KeyError, TypeError, ValueError, OverflowError, QualificationError) as error:
        errors.append("invalid qualification artifact: " + str(error))
    return list(dict.fromkeys(errors))


QUIET_POLICY = {"duration_seconds": 60, "max_gap_seconds": 5,
                "mean_cpu_percent_max": 20, "sample_cpu_percent_max": 30,
                "thermal_state": 0, "products": ["tmux", "rmux"],
                "retry_allowed": False}


def role_graph_errors(manifest: dict[str, Any]) -> list[str]:
    """Validate the prospectively pinned role declaration, never infer from a run.

    Source/layout reasoning still requires independent review. An observed image
    cannot select its own expected role or upgrade snapshot history to complete.
    """
    try:
        graph = manifest["source_role_graph"]
        if (graph["schema"] != 1 or graph["tool"] != manifest["tool"]
                or graph["public_role"] != manifest["tool"]
                or graph["source_commit"] != manifest["package_source"]
                or not graph["source_pins"] or not graph["route_reason"]
                or graph["binary_overrides"] is not False
                or graph["forced_standalone"] is not False
                or graph["web_required"] is not False):
            return ["R6 source/package role graph invalid"]
        roles = graph["client_roles"]
        if (not isinstance(roles, list) or not roles or len(set(roles)) != len(roles)
                or roles[0] != graph["public_role"]
                or any(r not in manifest["binaries"] for r in roles + [graph["daemon_role"]])
                or any(r not in manifest["code_directory_hashes"] for r in roles + [graph["daemon_role"]])):
            return ["R6 source/package role bindings missing"]
        if manifest["tool"] == "tmux" and (roles != ["tmux"] or graph["daemon_role"] != "tmux"):
            return ["R6 unexpected tmux executable role"]
        if manifest["tool"] == "rmux" and (not set(roles) <= {"rmux", "rmux_helper"}
                or graph["daemon_role"] not in ("rmux", "rmux_helper", "rmux_daemon")):
            return ["R6 unexpected RMUX helper/fallback role"]
    except (KeyError, TypeError, ValueError):
        return ["R6 source/package role graph missing"]
    return []


def quiet_window_errors(window: Any, manifest: dict[str, Any],
                        attempts: list[dict[str, Any]]) -> list[str]:
    """Recompute the fixed operational window from raw aggregate counters.

    A status/confirmation flag never substitutes for samples. This proves only
    the stated CPU/NSProcessInfo contract, not unobserved hardware temperatures.
    """
    errors = []
    try:
        if manifest["quiet_policy"] != QUIET_POLICY or window["policy"] != QUIET_POLICY:
            errors.append("R5 quiet policy differs from fixed contract")
        if (window["cohort_id"] != manifest["cohort_id"]
                or window["source"] != "host_statistics.HOST_CPU_LOAD_INFO+NSProcessInfo.thermalState"
                or window["attempt_number"] != 1 or window["errors"]
                or window["own_builds"] != 0 or not window["reservation"]
                or window["products"] != ["tmux", "rmux"]):
            errors.append("R5 quiet window identity/ownership/retry invalid")
        samples = window["samples"]
        if not isinstance(samples, list) or not 13 <= len(samples) <= 128:
            return errors + ["R5 continuous quiet sample population missing"]
        busy = total = 0
        for i, row in enumerate(samples):
            if (type(row["ns"]) is not int or type(row["thermal_state"]) is not int
                    or row["thermal_state"] != 0
                    or len(row["cpu_ticks"]) != 4
                    or any(type(x) is not int or not 0 <= x < 2**32 for x in row["cpu_ticks"])):
                errors.append("R5 thermal/aggregate CPU observation invalid")
                continue
            if i:
                previous = samples[i - 1]
                gap = row["ns"] - previous["ns"]
                if not 0 < gap <= 5_000_000_000:
                    errors.append("R5 quiet observation gap exceeds5s")
                delta = [(n - p) % 2**32 for n, p in zip(row["cpu_ticks"], previous["cpu_ticks"])]
                ticks = sum(delta)
                active = ticks - delta[2]
                if ticks <= 0 or active * 100 > ticks * 30:
                    errors.append("R5 CPU sample exceeds30percent or empty")
                busy += active
                total += ticks
        if samples[-1]["ns"] - samples[0]["ns"] < 60_000_000_000:
            errors.append("R5 continuous qualification shorter than60s")
        if total <= 0 or busy * 100 > total * 20:
            errors.append("R5 mean aggregate CPU exceeds20percent or empty")
        if window["start_ns"] != samples[0]["ns"] or window["end_ns"] != samples[-1]["ns"]:
            errors.append("R5 quiet window clock endpoints differ")
        for attempt in attempts:
            if attempt["driver"]["start_ns"] < window["end_ns"]:
                errors.append("R5 sample preceded common stabilization")
            if attempt.get("quiet_window_id") != window["cohort_id"]:
                errors.append("R5 sample not bound to common quiet window")
            if any(type(t.get("state")) is not int or t["state"] != 0 for t in attempt["thermal"]):
                errors.append("R5 thermal pressure changed during sample")
    except (KeyError, TypeError, ValueError, IndexError, OverflowError):
        errors.append("R5 symmetric thermal stabilization evidence missing")
    return list(dict.fromkeys(errors))


def digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def population_bytes() -> bytes:
    """Read and verify the original population before parsing or hashing it."""
    try:
        return provenance_archive.read(MANIFEST, Path(__file__).resolve().parents[2])
    except (OSError, ValueError) as error:
        raise QualificationError(f"invalid population archive: {error}") from error


@contextmanager
def population_epoch(commit: str):
    """Import under an explicit Git epoch; restore the current default on exit."""
    if commit is None:
        raise QualificationError("population epoch must be an exact commit SHA")
    population(epoch=commit)
    token = _POPULATION_EPOCH.set(commit)
    try:
        yield
    finally:
        _POPULATION_EPOCH.reset(token)


def population(*, epoch=None) -> dict[str, Any]:
    """Read current pins by default, or pins at an explicitly selected commit."""
    selected = epoch if epoch is not None else _POPULATION_EPOCH.get()
    try:
        data = json.loads(provenance_archive.read(
            MANIFEST, Path(__file__).resolve().parents[2],
            **({"current": True} if selected is None else {"epoch": selected})))
    except (OSError, ValueError) as error:
        raise QualificationError(f"invalid population archive: {error}") from error
    ids = [entry["id"] for entry in data["operations"]]
    public = data["public_operations"]
    if len(ids) != 33 or len(set(ids)) != 33 or len(public) != 10 or len(set(public)) != 10:
        raise QualificationError("population must contain exactly 33 unique IDs and 10 public IDs")
    if [entry["id"] for entry in data["operations"] if entry["public"]] != public:
        raise QualificationError("public mapping differs from frozen population")
    if data["tools"]["rmux"] != ids or data["tools"]["tmux"] != ids:
        raise QualificationError("paired tool population differs")
    for tool, supported in data["tools"].items():
        if tool not in TOOL_ORDER or len(supported) != len(set(supported)) or not set(supported) <= set(ids):
            raise QualificationError("unknown or duplicate tool operation")
    return data


def verify_source(source: str, bindings: list[tuple[str, Any]]) -> None:
    """Keep all existing operation/setup/timer/cleanup bodies byte-independent of edits.

    A batch factory may only wrap its final returned callable with a cancellation
    registration. Its nested operation and setup bodies must retain the old AST.
    """
    data = population()
    if [(label, action.__name__) for label, action in bindings] != [
            (entry["id"], entry["function"]) for entry in data["operations"]]:
        raise QualificationError("operation ID/function mapping changed")
    definitions = {node.name: node for node in ast.parse(source).body
                   if isinstance(node, (ast.FunctionDef, ast.ClassDef))}
    for name, pin in data["preserved_definitions"].items():
        node = definitions[name]
        if name == "main":
            # The R6 entry adds one optional argument and preserves the declared
            # binary spelling. Normalize only those exact changes to the R5 AST.
            addition = ast.parse('parser.add_argument("--tool-provenance", type=Path, help="require declared tool identity using pinned producer receipts")').body[0]
            node.body = [statement for statement in node.body
                         if ast.dump(statement) != ast.dump(addition)]
            for call in ast.walk(node):
                if isinstance(call, ast.Call) and isinstance(call.func, ast.Name) and call.func.id == "collect":
                    call.keywords = [keyword for keyword in call.keywords
                                     if not (keyword.arg == "tool_provenance" and ast.unparse(keyword.value) == "args.tool_provenance")]
                    if ast.unparse(call.args[2]) == "args.binary":
                        call.args[2] = ast.parse("args.binary.resolve()", mode="eval").body
        if name in BATCHES:
            returned = node.body[-1]
            if isinstance(returned, ast.Return) and isinstance(returned.value, ast.Call):
                call = returned.value
                if (ast.unparse(call.func) != "qualification.cancellable" or len(call.args) != 2
                        or ast.unparse(call.args[0]) != "operation"):
                    raise QualificationError("unexpected batch factory registration")
                returned.value = ast.Name(id="operation", ctx=ast.Load())
        actual = hashlib.sha256(frozen_ast.dump(node).encode()).hexdigest()
        if actual != pin["ast_sha256"]:
            raise QualificationError("frozen recipe/setup changed: " + name)


def cancellable(operation, cancel):
    """Register existing fixture cleanup for a batch interrupted between samples."""
    operation.cancel = cancel
    return operation


def schedule(run_id: str, operations: list[str], tools: list[str], iterations: int) -> list[dict[str, Any]]:
    # Scheduling uses only the byte-exact historical operation population. Source
    # execution still requires the latest declared AST pins in verify_source.
    raw = population_bytes()
    try:
        if hashlib.sha256(raw).hexdigest() != pin_revisions.ARCHIVE_SHA256:
            raise ValueError("schedule has no verified frozen population")
        # Raw historical archives also require the sealed revision history. They
        # do not pass through the public-projection branch of the byte reader.
        pin_revisions.entries(Path(__file__).resolve().parents[2])
    except ValueError as error:
        raise QualificationError(f"invalid population archive: {error}") from error
    data = json.loads(raw)
    ids = [entry["id"] for entry in data["operations"]]
    if not run_id or isinstance(iterations, bool) or not isinstance(iterations, int) or iterations < 1:
        raise QualificationError("invalid run identity or iterations")
    if not operations or len(set(operations)) != len(operations) or not set(operations) <= set(ids):
        raise QualificationError("empty, unknown or duplicate selected operation")
    if operations != [label for label in ids if label in operations]:
        raise QualificationError("selected operations reordered")
    if not tools or len(set(tools)) != len(tools) or not set(tools) <= set(TOOL_ORDER):
        raise QualificationError("empty, unknown or duplicate selected tool")
    result = []
    for label in operations:
        available = [tool for tool in TOOL_ORDER if tool in tools and label in data["tools"][tool]]
        for pair in range(1, iterations + 1):
            ordered = list(available)
            if "rmux" in available and "tmux" in available and (ids.index(label) + pair - 1) % 2:
                ordered[:2] = ["tmux", "rmux"]
            for order, tool in enumerate(ordered):
                result.append({"run_id": run_id, "operation": label, "tool": tool,
                               "pair": pair, "order": order, "sequence": len(result) + 1,
                               "attempt_id": f"{run_id}/{label}/{pair}/{order}/{tool}",
                               "attempt_number": 1})
    if not result:
        raise QualificationError("selection contains no supported tool/operation attempts")
    return result


def elapsed_value(value: Any) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
        raise QualificationError("missing or invalid sampler result; finite nonnegative milliseconds required")
    return float(value)


def finish_attempt(attempt: dict[str, Any], value: Any) -> float:
    value = elapsed_value(value)
    attempt.update(status="pass", elapsed_ms=value)
    return value


def completion_errors(expected: list[dict[str, Any]], attempts: list[dict[str, Any]],
                      rows: list[dict[str, Any]], notes: list[Any]) -> list[str]:
    """Validate the immutable plan against observed attempts and published metrics."""
    errors = []
    if not expected:
        errors.append("empty expected schedule")
    expected_by_id = {item["attempt_id"]: item for item in expected}
    if len(expected_by_id) != len(expected):
        errors.append("duplicate expected identity")
    counts = Counter(item.get("attempt_id") for item in attempts)
    for key, count in counts.items():
        if count != 1:
            errors.append(f"duplicate/retry attempt: {key}")
        if key not in expected_by_id:
            errors.append(f"unknown attempt: {key}")
    missing = set(expected_by_id) - set(counts)
    if missing:
        errors.append(f"missing attempts: {len(missing)}")
    if len(attempts) != len(expected):
        errors.append(f"attempt cardinality {len(attempts)} != {len(expected)}")
    if [item.get("attempt_id") for item in attempts] != [item["attempt_id"] for item in expected]:
        errors.append("attempt order differs from frozen schedule")
    samples: dict[tuple[str, str], list[float]] = {}
    for attempt in attempts:
        identity = expected_by_id.get(attempt.get("attempt_id"))
        if identity is None:
            continue
        if any(attempt.get(key) != identity[key] or type(attempt.get(key)) is not type(identity[key])
               for key in IDENTITY_FIELDS):
            errors.append("attempt identity/retry fields differ: " + identity["attempt_id"])
        if attempt.get("status") != "pass" or "error_type" in attempt or "cleanup_error" in attempt:
            errors.append("unsuccessful attempt: " + identity["attempt_id"])
            continue
        try:
            value = elapsed_value(attempt.get("elapsed_ms"))
        except QualificationError as error:
            errors.append(str(error))
            continue
        samples.setdefault((identity["operation"], identity["tool"]), []).append(round(value, 3))
    row_ids = [row.get("id") for row in rows]
    if len(row_ids) != len(set(row_ids)):
        errors.append("duplicate operation row")
    planned_counts = Counter((item["operation"], item["tool"]) for item in expected)
    observed_metrics = {}
    for row in rows:
        for tool, metric in row.get("metrics", {}).items():
            observed_metrics[(row["id"], tool)] = metric
    if set(observed_metrics) != set(planned_counts):
        errors.append("missing or unexpected metric")
    for key, count in planned_counts.items():
        metric = observed_metrics.get(key)
        if not isinstance(metric, dict) or len(samples.get(key, [])) != count:
            errors.append(f"incomplete metric samples: {key}")
        elif metric.get("samples_ms") != samples[key]:
            errors.append(f"metric/attempt samples differ: {key}")
    if notes:
        errors.append("run or cleanup errors retained")
    return errors


def validate_artifact(payload: dict[str, Any]) -> list[str]:
    """Reconstruct the plan when reviewing a saved artifact, rather than trusting complete."""
    try:
        plan = payload["sampling_plan"]
        expected = schedule(plan["run_id"], plan["operations"], plan["tools"], plan["iterations"])
        errors = completion_errors(expected, payload["attempts"], payload["operations"], payload["notes"])
        if expected != plan["expected"] or digest(expected) != plan["expected_sha256"]:
            errors.append("saved expected schedule changed")
        if payload["population_sha256"] != hashlib.sha256(population_bytes()).hexdigest():
            errors.append("population digest differs")
        if [row["id"] for row in payload["operations"]] != plan["operations"]:
            errors.append("operation rows differ from selection")
        if any(event.get("status") != "pass" for event in payload["sampler_cleanup"]):
            errors.append("sampler cancellation failed or incomplete")
        if payload.get("sampler_errors"):
            errors.append("sampler errors retained")
        if payload.get("sampling_finished") is not True or payload.get("cleanup_finished") is not True:
            errors.append("sampler finalization not established")
        if "tool_identity" in payload:
            errors.extend(tool_identity.saved_errors(payload["tool_identity"], plan["run_id"],
                          payload["requested_tools"], payload["attempts"], payload["rmux_binaries"]))
        return errors
    except (KeyError, TypeError, ValueError, QualificationError) as error:
        return [f"invalid accounting artifact: {error}"]
