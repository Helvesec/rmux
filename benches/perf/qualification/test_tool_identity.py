#!/usr/bin/env python3
"""Pure R6 controls through the real sampler; no executable or version process runs."""
from contextlib import ExitStack
import copy
import hashlib
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import bench_unix as b
import qualification as q
import tool_identity as t

EVIDENCE = None


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def pin(path):
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "bytes": path.stat().st_size}


class Fixture:
    def __init__(self, directory, tools=None):
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=False)
        self.out = directory / "artifact.json"
        self.tools = tools or {"rmux", "tmux"}
        self.paths = {}
        for role in ("rmux", "rmux_daemon", "tmux"):
            path = directory / role
            path.write_text("synthetic bytes for " + role + " version A\n")
            path.chmod(0o700)
            self.paths[role] = path
        self.calls = []
        self.probes = []
        self.bound_executables = {}
        self.on_call = None
        self.provenance = directory / "provenance.json"
        self.receipt = directory / "build-receipt.json"
        self.freeze_receipt()

    def freeze_receipt(self):
        receipt = {"schema": 1, "synthetic": True, "status": "success", "returncode": 0,
                   "argv": ["SYNTHETIC-NOT-A-BUILD"], "kind": "source-build", "build_id": "synthetic-build",
                   "source": {"commit": "1" * 40, "tree": "2" * 40, "clean": True},
                   "artifacts": {role: {"role": role, **pin(path)} for role, path in self.paths.items()}}
        save(self.receipt, receipt)
        save(self.provenance, {"schema": 1, "tools": {role: {"receipt": pin(self.receipt), "artifact": role}
                                                       for role in self.paths}})

    def versions(self, executable, *args, **kwargs):
        self.probes.append([executable, *args])
        return "SYNTHETIC ONLY 1.0"

    def factory(self, adapter, iterations):
        self.bound_executables[adapter.tool] = adapter.executable
        def make(label):
            def operation():
                saved = json.loads(self.out.read_text())
                state = saved["tool_identity"]
                assert state["manifest"] is not None
                assert pin(Path(state["manifest"]["path"])) == state["manifest"]
                assert saved["attempts"][-1]["status"] == "running"
                self.calls.append((adapter.tool, label))
                if self.on_call:
                    self.on_call(self)
                return 1.25
            return operation
        return {label: make(label) for label in q.population()["tools"][adapter.tool]}

    def collect(self, required=True, version=None, daemon=True):
        original_which = b.shutil.which
        def which(name):
            return str(self.paths[name]) if name in self.paths else original_which(name)
        with ExitStack() as stack:
            stack.enter_context(patch.object(b, "tmux_like_operations", side_effect=self.factory))
            stack.enter_context(patch.object(b.shutil, "which", side_effect=which))
            stack.enter_context(patch.object(b, "version", side_effect=version or self.versions))
            stack.enter_context(patch.object(b, "git", return_value="SYNTHETIC ONLY"))
            b.collect(self.out, 1, self.paths["rmux"], rmux_layout="synthetic-only",
                      rmux_public_binary=None, rmux_helper_binary=None,
                      rmux_daemon_binary=self.paths["rmux_daemon"] if daemon else None,
                      progress_enabled=False, sample_progress=False,
                      only_operations={"new_session_warm_sh"}, selected_tools=self.tools,
                      tool_provenance=self.provenance if required else None)

    def artifact(self):
        return json.loads(self.out.read_text())


class IdentityControls(unittest.TestCase):
    def fixture(self, **kwargs):
        return Fixture(EVIDENCE / self._testMethodName, **kwargs)

    def rejected(self, fixture, **kwargs):
        with self.assertRaises(Exception):
            fixture.collect(**kwargs)
        artifact = fixture.artifact()
        self.assertFalse(artifact["complete"])
        self.assertTrue(q.validate_artifact(artifact))
        self.assertNotEqual(artifact["tool_identity"]["status"], "verified_declared_tools")
        return artifact

    def test_stable_manifest_precedes_calls_and_final_gate(self):
        f = self.fixture(); f.collect(); a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(q.validate_artifact(a), [])
        self.assertEqual(len(f.calls), 2)
        self.assertEqual(a["tool_identity"]["status"], "verified_declared_tools")
        m = a["tool_identity"]["manifest_data"]
        self.assertEqual(m["run_id"], a["sampling_plan"]["run_id"])
        self.assertEqual(m["transient_helpers"], "unknown; not observed")
        self.assertEqual(len(f.probes), 2)
        self.assertNotIn(str(f.paths["rmux_daemon"]), [row[0] for row in f.probes])

    def test_same_path_bytes_changed_before_sampling(self):
        f = self.fixture(); original = t.write_once
        def write(path, manifest):
            result = original(path, manifest)
            f.paths["rmux"].write_text(f.paths["rmux"].read_text().replace("version A", "version B"))
            return result
        with patch.object(t, "write_once", side_effect=write): self.rejected(f)
        self.assertEqual(f.calls, [])

    def test_relative_declared_client_is_bound_to_absolute_executable(self):
        f = self.fixture(); absolute = f.paths["rmux"]
        f.paths["rmux"] = Path(os.path.relpath(absolute, Path.cwd()))
        f.freeze_receipt(); f.collect(); a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(f.bound_executables["rmux"], str(absolute))
        identity = a["tool_identity"]["manifest_data"]["tools"]["rmux"]["identity"]
        self.assertEqual(identity["declared"], str(f.paths["rmux"]))
        self.assertEqual(identity["selected"], str(absolute))

    def test_symlink_retargeted_before_sampling(self):
        f = self.fixture(); old = f.paths["rmux"]
        second = f.directory / "second-target"; second.write_bytes(old.read_bytes()); second.chmod(0o700)
        link = f.directory / "client-link"; link.symlink_to(old)
        f.paths["rmux"] = link; f.freeze_receipt(); original = t.write_once
        def write(path, manifest):
            result = original(path, manifest); link.unlink(); link.symlink_to(second); return result
        with patch.object(t, "write_once", side_effect=write): self.rejected(f)
        self.assertEqual(f.calls, [])

    def test_daemon_from_other_source_receipt(self):
        f = self.fixture(); other = f.directory / "foreign-build.json"
        receipt = json.loads(f.receipt.read_text()); receipt["source"]["commit"] = "3" * 40
        receipt["build_id"] = "foreign-build"; save(other, receipt)
        supplied = json.loads(f.provenance.read_text()); supplied["tools"]["rmux_daemon"]["receipt"] = pin(other)
        save(f.provenance, supplied); a = self.rejected(f)
        self.assertEqual(f.calls, [])
        self.assertIn("same build receipt", " ".join(a["tool_identity"]["preflight_errors"]))

    def test_missing_daemon_is_unknown_not_neighbor_inference(self):
        f = self.fixture(); self.rejected(f, daemon=False)
        self.assertEqual(f.calls, []); self.assertEqual(f.probes, [])

    def test_missing_provenance_prevents_even_version_probe(self):
        f = self.fixture(); supplied = json.loads(f.provenance.read_text())
        del supplied["tools"]["tmux"]; save(f.provenance, supplied)
        self.rejected(f); self.assertEqual(f.calls, []); self.assertEqual(f.probes, [])

    def test_incomplete_success_receipt_not_attestation(self):
        f = self.fixture(); receipt = json.loads(f.receipt.read_text()); del receipt["argv"]
        save(f.receipt, receipt)
        supplied = json.loads(f.provenance.read_text())
        for row in supplied["tools"].values(): row["receipt"] = pin(f.receipt)
        save(f.provenance, supplied); self.rejected(f); self.assertEqual(f.calls, [])

    def test_manifest_write_refusal_prevents_launch(self):
        f = self.fixture()
        with patch.object(t, "write_once", side_effect=PermissionError("fixture manifest write refused")):
            a = self.rejected(f)
        self.assertEqual(f.calls, [])
        self.assertIn("write refused", str(a["sampler_errors"]))

    def test_manifest_exclusive_creation_refuses_overwrite(self):
        f = self.fixture(); path = f.directory / "existing.json"; path.write_text("preserved")
        with self.assertRaises(FileExistsError): t.write_once(path, {"new": 1})
        self.assertEqual(path.read_text(), "preserved")

    def test_required_version_failure_is_unknown(self):
        f = self.fixture(); a = self.rejected(f, version=lambda *a, **k: "available")
        self.assertEqual(f.calls, [])
        self.assertIn("version unknown", str(a["tool_identity"]["preflight_errors"]))

    def test_end_change_retains_all_raw_samples(self):
        f = self.fixture()
        def mutate(f):
            if len(f.calls) == 2: f.paths["rmux_daemon"].write_text("changed after last sample\n")
        f.on_call = mutate; a = self.rejected(f)
        self.assertEqual(len(a["attempts"]), 2)
        self.assertTrue(all(row["status"] == "pass" for row in a["attempts"]))
        self.assertEqual(sum(len(row["metrics"]) for row in a["operations"]), 2)
        self.assertTrue(a["tool_identity"]["final_errors"])

    def test_between_samples_change_blocks_next_call(self):
        f = self.fixture()
        f.on_call = lambda f: f.paths["tmux"].write_text("changed between samples\n")
        a = self.rejected(f); self.assertEqual(len(f.calls), 1)
        self.assertEqual(len(a["unattempted"]), 1)

    def test_receipt_change_invalidates_qualification(self):
        f = self.fixture(); original = t.write_once
        def write(path, manifest):
            result = original(path, manifest); f.receipt.write_text("changed receipt"); return result
        with patch.object(t, "write_once", side_effect=write): self.rejected(f)
        self.assertEqual(f.calls, [])

    def test_incomplete_saved_manifest_rejected(self):
        f = self.fixture(); f.collect(); a = f.artifact()
        del a["tool_identity"]["manifest_data"]["tools"]["rmux_daemon"]
        errors = q.validate_artifact(a); self.assertTrue(errors)
        save(f.directory / "mutated-manifest.json", {"synthetic_mutation": True, "artifact": a, "rejections": errors})

    def test_saved_required_mode_cannot_silently_downgrade(self):
        f = self.fixture(); f.collect(); a = f.artifact()
        a["tool_identity"]["mode"] = "historical"
        self.assertTrue(q.validate_artifact(a))
        a["tool_identity"]["status"] = "unqualified_historical"
        errors = q.validate_artifact(a); self.assertTrue(errors)
        save(f.directory / "mutated-mode.json", {"synthetic_mutation": True, "artifact": a, "rejections": errors})

    def test_saved_missing_identity_checks_rejected(self):
        f = self.fixture(); f.collect(); a = f.artifact()
        a["tool_identity"]["checks"] = a["tool_identity"]["checks"][-1:]
        errors = q.validate_artifact(a); self.assertTrue(errors)
        save(f.directory / "mutated-checks.json", {"synthetic_mutation": True, "artifact": a, "rejections": errors})

    def test_receipt_artifact_role_cannot_be_substituted(self):
        f = self.fixture(); receipt = json.loads(f.receipt.read_text())
        receipt["artifacts"]["rmux_daemon"]["role"] = "rmux"
        save(f.receipt, receipt); supplied = json.loads(f.provenance.read_text())
        for row in supplied["tools"].values(): row["receipt"] = pin(f.receipt)
        save(f.provenance, supplied); self.rejected(f); self.assertEqual(f.calls, [])

    def test_historical_unknowns_do_not_grant_r6_pass(self):
        f = self.fixture(); f.collect(required=False, daemon=False); a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(a["tool_identity"]["status"], "unqualified_historical")
        self.assertTrue(a["tool_identity"]["preflight_errors"])
        self.assertEqual(a["tool_identity"]["manifest_data"]["tools"]["rmux_daemon"]["identity"]["status"], "unknown")

    def test_third_party_package_has_separate_provenance(self):
        f = self.fixture(); package = f.directory / "package-receipt.json"
        receipt = {"schema": 1, "synthetic": True, "status": "success", "returncode": 0,
                   "argv": ["SYNTHETIC-INSTALL-VALIDATION"], "kind": "installed-package",
                   "package": {"name": "synthetic-tmux", "version": "1", "installation": "private-fixture"},
                   "artifacts": {"tmux": {"role": "tmux", **pin(f.paths["tmux"])}}}
        save(package, receipt); supplied = json.loads(f.provenance.read_text())
        supplied["tools"]["tmux"]["receipt"] = pin(package); save(f.provenance, supplied)
        f.collect(); self.assertTrue(f.artifact()["complete"])


if __name__ == "__main__":
    EVIDENCE = Path(sys.argv[1]).absolute(); EVIDENCE.mkdir(parents=True, exist_ok=False)
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(IdentityControls))
    save(EVIDENCE / "summary.json", {"tests": result.testsRun, "pass": result.wasSuccessful(),
         "failures": len(result.failures), "errors": len(result.errors), "product_runs": 0,
         "child_processes": 0, "performance_evidence": False, "receipts_are_synthetic": True})
    raise SystemExit(not result.wasSuccessful())
