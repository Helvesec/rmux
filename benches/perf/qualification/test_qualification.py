#!/usr/bin/env python3
"""Pure controls of the same gate used by native qualification artifacts."""
import copy
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/bench"))
import qualification as q


def specimen():
    operations = []
    for regime in ("cold", "warm"):
        operations.append({"id": "new_session_" + regime + "_sh", "regime": regime,
            "setup": "fresh endpoint or observed seeded daemon", "reset": "owned exit",
            "window": "posix_spawn through waitpid", "postcondition": "fresh FIFO from pane",
            "timed_argv": {t: ["/synthetic/" + t, "-S", "{socket}", "new-session"]
                           for t in ("rmux", "tmux")}})
    m = {"schema": "darwin-harness-qualification-v1", "kind": "functional_qualification",
         "tool": "tmux", "run_id": "explicitly-synthetic-control", "performance_claim": False,
         "samples_per_cell": 4, "timeout_seconds": 5, "cleanup_seconds": 45,
         "operations": operations, "statistics": {"seed": 731, "resamples": 200},
         "identity_observation": "bounded_text_vnode_snapshots",
         "code_directory_hashes": {"tmux": "a" * 40}}
    rows = []
    for expected in q.qualification_plan(m):
        number = expected["sequence"]
        daemon = {"pid": 5000 + number, "ppid": 1, "birth": [number, 1]}
        rows.append({"identity": expected, "status": "pass", "timeout": False,
            "elapsed_ms": 2.0, "returncode": 0, "client_pid": number + 100,
            "environment_prepared_ns": 1, "path_context": {"socket": "/synthetic/s"},
            "timed_argv": ["/synthetic/tmux", "-S", "/synthetic/s", "new-session"],
            "driver": {"driver": "darwin-posix-spawn-kqueue-waitpid", "polling": False,
                "raw_wait_status": 0, "reaped_pid": number + 100,
                "prepared_ns": 2, "start_ns": 100, "end_ns": 2000100},
            "state_before": {"endpoint_present": expected["regime"] == "warm",
                "daemon": daemon if expected["regime"] == "warm" else None},
            "state_after": {"endpoint_present": True, "daemon": daemon},
            "work": {"challenge": format(number, "064x"), "acknowledged": True,
                "readiness": "fresh_fifo_kqueue", "session_count": 1, "pane_count": 1,
                "size": [80, 24], "daemon_pid": daemon["pid"], "pane_pid": number + 7000,
                "observed_pane": {"pid": number + 7000, "status": "observed_text_vnode",
                                  "before": {"pid": number + 7000, "birth": [1, number]},
                                  "after": {"pid": number + 7000, "birth": [1, number]}}},
            "cleanup": {"closed": True, "rescue": False, "seconds": .01},
            "image_identity": {"disk_unchanged": True, "all_measured_images_bound": False,
                "exec_fork_history_complete": False, "unexpected_images": []},
            "thermal": [{"state": 0}, {"state": 0}]})
    result = {"manifest": m, "attempts": rows, "sampling_finished": True,
              "cleanup_finished": True, "errors": [], "performance_claim": False,
              "floor": {"diagnostic_only": True, "raw_ms": [.1, .2]},
              "statistics": {}, "installed_package_bound": False}
    for op in operations:
        result["statistics"][op["id"]] = q.sample_statistics([2.0] * 4, seed=731, resamples=200)
    return result


def mutations():
    return {
        "omitted_cell": lambda a: a["attempts"].__delitem__(slice(4, None)),
        "reduced_population": lambda a: a["attempts"].pop(),
        "duplicate_attempt": lambda a: a["attempts"].append(copy.deepcopy(a["attempts"][0])),
        "retry": lambda a: a["attempts"][0]["identity"].update(attempt_number=2),
        "reorder": lambda a: a["attempts"].reverse(),
        "relabel": lambda a: a["attempts"][0]["identity"].update(operation="new_session_warm_sh"),
        "ignore_timeout": lambda a: a["attempts"][0].update(timeout=True),
        "nonzero": lambda a: a["attempts"][0].update(returncode=1),
        "polling": lambda a: a["attempts"][0]["driver"].update(polling=True),
        "unreaped": lambda a: a["attempts"][0]["driver"].update(reaped_pid=99999),
        "late_environment": lambda a: a["attempts"][0].update(
            environment_prepared_ns=a["attempts"][0]["driver"]["start_ns"] + 1),
        "false_timer": lambda a: a["attempts"][0].update(elapsed_ms=.01),
        "changed_argv": lambda a: a["attempts"][0]["timed_argv"].append("fake-work"),
        "cold_used_warm": lambda a: a["attempts"][0]["state_before"].update(endpoint_present=True),
        "warm_used_cold": lambda a: a["attempts"][4]["state_before"].update(daemon=None),
        "warm_restarted": lambda a: a["attempts"][4]["state_after"].update(daemon={"pid": 12}),
        "omitted_work": lambda a: a["attempts"][0]["work"].update(acknowledged=False),
        "cached_work": lambda a: a["attempts"][1]["work"].update(challenge=a["attempts"][0]["work"]["challenge"]),
        "wrong_artifact": lambda a: a["attempts"][0]["image_identity"].update(disk_unchanged=False),
        "wrong_state": lambda a: a["attempts"][0]["work"].update(pane_count=0),
        "thermal_unknown": lambda a: a["attempts"][0]["thermal"][0].update(state=None),
        "thermal_omitted": lambda a: a["attempts"][0].update(thermal=[]),
        "thermal_boolean": lambda a: a["attempts"][0]["thermal"][0].update(state=False),
        "pane_identity_omitted": lambda a: a["attempts"][0]["work"]["observed_pane"].update(before=None),
        "nonce_invalid": lambda a: a["attempts"][0]["work"].update(challenge="z" * 64),
        "post_endpoint_absent": lambda a: a["attempts"][0]["state_after"].update(endpoint_present=False),
        "cleanup_unknown": lambda a: a["attempts"][0]["cleanup"].update(closed=False),
        "rescued": lambda a: a["attempts"][0]["cleanup"].update(rescue=True),
        "statistics_tampered": lambda a: a["statistics"]["new_session_cold_sh"].update(p99_ms=0),
        "floor_primary": lambda a: a["floor"].update(diagnostic_only=False),
        "false_performance_claim": lambda a: a.update(performance_claim=True),
        "interrupted_after_samples": lambda a: a.update(sampling_finished=False),
        "removed_manifest_cell": lambda a: a["manifest"]["operations"].pop(),
        "reduced_manifest_count": lambda a: a["manifest"].update(samples_per_cell=3),
    }


class GateTests(unittest.TestCase):
    def test_functional_specimen_only(self):
        value = specimen()
        self.assertEqual(q.qualification_errors(value, q.digest(value["manifest"]), require_r6=False), [])
        self.assertTrue(q.qualification_errors(value, q.digest(value["manifest"])))

    def test_every_canary_uses_native_gate(self):
        good = specimen()
        frozen = q.digest(good["manifest"])
        for label, mutate in mutations().items():
            with self.subTest(label=label):
                bad = copy.deepcopy(good)
                mutate(bad)
                self.assertTrue(q.qualification_errors(bad, frozen, require_r6=False), label)

    def test_flags_cannot_promote_bounded_observation(self):
        value = specimen()
        for row in value["attempts"]:
            row["image_identity"].update(all_measured_images_bound=True, exec_fork_history_complete=True)
        value.update(installed_package_bound=True, thermal_stabilization_confirmed=True)
        errors = q.qualification_errors(value, q.digest(value["manifest"]))
        self.assertIn("R6 frozen observer cannot attest complete transient execution history", errors)

    def test_statistics_reference(self):
        value = q.sample_statistics([1, 2, 3, 4, 5], seed=123, resamples=200)
        self.assertEqual(value["p50_ms"], 3)
        self.assertAlmostEqual(value["p95_ms"], 4.8)
        self.assertAlmostEqual(value["p99_ms"], 4.96)
        self.assertEqual(value, q.sample_statistics([1, 2, 3, 4, 5], seed=123, resamples=200))
        self.assertLessEqual(value["median_ci95_ms"][0], 3)
        self.assertGreaterEqual(value["median_ci95_ms"][1], 3)

    def test_invalid_samples(self):
        for values in ([], [float("nan")], [float("inf")], [-1], [True]):
            with self.subTest(values=values), self.assertRaises(q.QualificationError):
                q.sample_statistics(values, seed=1)


def quiet_specimen():
    """Synthetic counters, explicitly not host observations."""
    start = 1_000_000_000
    rows = [{"ns": start + i * 2_000_000_000,
             "cpu_ticks": [i * 10, 0, i * 90, 0], "thermal_state": 0}
            for i in range(31)]
    manifest = {"quiet_policy": copy.deepcopy(q.QUIET_POLICY), "cohort_id": "synthetic-pair"}
    window = {"policy": copy.deepcopy(q.QUIET_POLICY), "cohort_id": "synthetic-pair",
              "source": "host_statistics.HOST_CPU_LOAD_INFO+NSProcessInfo.thermalState",
              "attempt_number": 1, "errors": [], "own_builds": 0,
              "reservation": {"explicitly_synthetic": True}, "products": ["tmux", "rmux"],
              "samples": rows, "start_ns": rows[0]["ns"], "end_ns": rows[-1]["ns"]}
    attempt = {"driver": {"start_ns": rows[-1]["ns"] + 1},
               "quiet_window_id": "synthetic-pair", "thermal": [{"state": 0}, {"state": 0}]}
    return window, manifest, [attempt]


class QuietAndRoleTests(unittest.TestCase):
    def test_complete_raw_window(self):
        self.assertEqual(q.quiet_window_errors(*quiet_specimen()), [])

    def test_window_canaries(self):
        changes = {
            "missing_samples": lambda w, m, a: w.update(samples=[]),
            "snapshot_only": lambda w, m, a: w.update(samples=w["samples"][:1]),
            "short_window": lambda w, m, a: w["samples"].pop(),
            "observation_gap": lambda w, m, a: w["samples"].__delitem__(slice(3, 6)),
            "pressure": lambda w, m, a: w["samples"][10].update(thermal_state=1),
            "boolean_state": lambda w, m, a: w["samples"][10].update(thermal_state=False),
            "unknown_counter": lambda w, m, a: w["samples"][10].update(cpu_ticks=None),
            "relaxed_policy": lambda w, m, a: m["quiet_policy"].update(mean_cpu_percent_max=100),
            "different_product_window": lambda w, m, a: m.update(cohort_id="another-window"),
            "hidden_retry": lambda w, m, a: w.update(attempt_number=2),
            "own_build": lambda w, m, a: w.update(own_builds=1),
            "no_reservation": lambda w, m, a: w.update(reservation=None),
            "stale_clock_endpoint": lambda w, m, a: w.update(end_ns=w["end_ns"] - 1),
            "sampling_before_window": lambda w, m, a: a[0]["driver"].update(start_ns=0),
            "unbound_sample": lambda w, m, a: a[0].update(quiet_window_id="another-window"),
            "sample_pressure": lambda w, m, a: a[0]["thermal"][1].update(state=1),
            "producer_error": lambda w, m, a: w["errors"].append("lost observation"),
        }
        for label, mutate in changes.items():
            with self.subTest(label=label):
                w, m, a = quiet_specimen(); mutate(w, m, a)
                self.assertTrue(q.quiet_window_errors(w, m, a), label)

    def test_mean_and_peak_separately_rejected(self):
        w, m, a = quiet_specimen()
        for i, row in enumerate(w["samples"]):
            row["cpu_ticks"] = [i * 21, 0, i * 79, 0]
        errors = q.quiet_window_errors(w, m, a)
        self.assertTrue(any("mean aggregate" in e for e in errors))
        self.assertFalse(any("sample exceeds" in e for e in errors))
        w, m, a = quiet_specimen()
        for row in w["samples"][1:]:
            row["cpu_ticks"][0] += 31
        self.assertTrue(any("sample exceeds" in e for e in q.quiet_window_errors(w, m, a)))

    def test_booleans_and_complete_label_cannot_invent_evidence(self):
        value = specimen()
        value["manifest"]["identity_observation"] = "complete_exec_fork_history"
        value["thermal_stabilization_confirmed"] = True
        for row in value["attempts"]:
            row["image_identity"].update(all_measured_images_bound=True, exec_fork_history_complete=True)
        errors = q.qualification_errors(value, q.digest(value["manifest"]))
        self.assertIn("R6 frozen observer cannot attest complete transient execution history", errors)
        self.assertIn("R5 symmetric thermal stabilization evidence missing", errors)

    def test_role_graph_rejects_arbitrary_helper_and_runtime_overrides(self):
        m = specimen()["manifest"]
        m.update(package_source="synthetic-tmux3.7b", binaries={"tmux": {}})
        m["source_role_graph"] = {"schema": 1, "tool": "tmux", "public_role": "tmux",
            "source_commit": m["package_source"], "source_pins": [{"synthetic": True}],
            "route_reason": "synthetic tmux self-daemon", "binary_overrides": False,
            "forced_standalone": False, "web_required": False,
            "client_roles": ["tmux"], "daemon_role": "tmux"}
        self.assertEqual(q.role_graph_errors(m), [])
        for field, value in [("daemon_role", "unobserved-helper"), ("client_roles", ["tmux", "fallback"]),
                             ("binary_overrides", True), ("forced_standalone", True),
                             ("source_commit", "relabeled-source")]:
            bad = copy.deepcopy(m); bad["source_role_graph"][field] = value
            self.assertTrue(q.role_graph_errors(bad))


if __name__ == "__main__":
    unittest.main()
