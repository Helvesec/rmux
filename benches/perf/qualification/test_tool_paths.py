#!/usr/bin/env python3
"""Pure F1 path-binding controls. All producer receipts and calls are synthetic."""
from contextlib import ExitStack, chdir
from copy import deepcopy
import itertools
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest
from unittest.mock import patch

from test_tool_identity import Fixture, b, q, t, save
import pin_proof_publication as publications
import pin_revisions as revisions

EVIDENCE = None
POPEN = subprocess.Popen
REPOSITORY = Path(q.__file__).resolve().parents[2]
POPULATION_BLOB = json.loads(q.MANIFEST.read_bytes())["historical_source"]["git_blob"]
GIT_PROCESSES = 0
HISTORY_PATHS = (revisions.CHAIN.as_posix(), publications.CHAIN.as_posix())
PROVENANCE_COMMANDS = {
    ("cat-file", mode, blob)
    for blob in (POPULATION_BLOB, *(source["git_blob"] for source in publications.SOURCES.values()))
    for mode in ("-s", "blob")
} | {
    ("log", "--full-history", "--format=%H", limit, "HEAD", "--", path)
    for limit, path in zip(("-65", "-17"), HISTORY_PATHS)
} | {
    ("rev-parse", publications.ORIGIN + ":" + (publications.DIRECTORY / name).as_posix())
    for name in publications.SOURCES
} | {("rev-list", "--full-history", "HEAD")}


def forbidden(*args, **kwargs):
    raise AssertionError("Pure path control attempted host lookup or process launch")


def provenance_popen(*args, **kwargs):
    # Keep real archive, append-only history and sealed-proof validation. Permit
    # only their bounded local reads, never a generic Git command or shell.
    if (len(args) != 1 or not isinstance(args[0], (list, tuple))
            or any(not isinstance(item, str) for item in args[0])
            or tuple(args[0][:3]) != ("git", "-C", str(REPOSITORY))
            or kwargs != {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE}):
        return forbidden(*args, **kwargs)
    command = tuple(args[0][3:])
    history_read = (len(command) == 2 and command[0] == "show"
                    and any(re.fullmatch(r"[0-9a-f]{40}:" + re.escape(path), command[1])
                            for path in HISTORY_PATHS))
    if command not in PROVENANCE_COMMANDS and not history_read:
        return forbidden(*args, **kwargs)
    process = POPEN(*args, **kwargs)
    global GIT_PROCESSES
    GIT_PROCESSES += 1
    return process


class PathFixture(Fixture):
    def __init__(self, directory, explicit=True, helper=True, relative_roles=("rmux",),
                 symlink_roles=("rmux",)):
        super().__init__(directory)
        self.explicit, self.helper = explicit, helper
        for role in ("rmux_public", "rmux_helper"):
            if (role == "rmux_public" and explicit) or (role == "rmux_helper" and helper):
                path = directory / role
                path.write_text("NONEXECUTED SYNTHETIC PATH FIXTURE " + role + "\n")
                path.chmod(0o700)
                self.paths[role] = path
        for role in symlink_roles:
            link = directory / (role + "-link")
            link.symlink_to(self.paths[role].name)
            self.paths[role] = link
        for role in relative_roles:
            self.paths[role] = Path(os.path.relpath(self.paths[role], Path.cwd()))
        self.freeze_receipt()

    def collect(self):
        with ExitStack() as stack:
            stack.enter_context(patch.object(subprocess, "Popen", side_effect=provenance_popen))
            stack.enter_context(patch.object(b, "tmux_like_operations", side_effect=self.factory))
            stack.enter_context(patch.object(b.shutil, "which", side_effect=lambda name: str(self.paths["tmux"]) if name == "tmux" else None))
            stack.enter_context(patch.object(b, "version", side_effect=self.versions))
            stack.enter_context(patch.object(b, "git", return_value="SYNTHETIC ONLY"))
            b.collect(self.out, 1, self.paths["rmux"], rmux_layout="synthetic-only",
                      rmux_public_binary=self.paths.get("rmux_public"),
                      rmux_helper_binary=self.paths.get("rmux_helper"),
                      rmux_daemon_binary=self.paths["rmux_daemon"],
                      progress_enabled=False, sample_progress=False,
                      only_operations={"new_session_warm_sh"}, selected_tools=self.tools,
                      tool_provenance=self.provenance)


class PathControls(unittest.TestCase):
    def fixture(self, suffix="", **kwargs):
        f = PathFixture(EVIDENCE / (self._testMethodName + suffix), **kwargs)
        f.collect()
        a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(q.validate_artifact(a), [])
        self.assertEqual(a["tool_identity"]["status"], "verified_declared_tools")
        return f, a

    def reject(self, f, a, name, repin=False):
        if repin:
            raw = t.encoded(a["tool_identity"]["manifest_data"])
            a["tool_identity"]["manifest"].update(sha256=t.digest(raw), bytes=len(raw))
        errors = q.validate_artifact(a)
        save(f.directory / (name + ".json"), {"synthetic_mutation": True,
             "synthetic_manifest_repin": repin, "artifact": a, "errors": errors})
        self.assertTrue(errors, name)
        if repin:
            self.assertNotIn("identity manifest digest differs", errors)
        return errors

    def test_C1_relative_symlink_all_roles_and_C4_public_contradiction(self):
        f, a = self.fixture()
        m = a["tool_identity"]["manifest_data"]
        self.assertEqual(set(m["tools"]), set(f.paths))
        self.assertEqual(m["tools"]["rmux"]["identity"]["declared_type"], "symlink")
        self.assertEqual(m["tools"]["rmux"]["identity"]["selected"], f.bound_executables["rmux"])
        self.assertEqual(a["rmux_binaries"]["client"], str(f.paths["rmux"]))
        self.assertEqual(len(f.probes), 2)
        mutant = deepcopy(a)
        mutant["rmux_binaries"]["public"] = str(f.paths["tmux"])
        self.assertEqual(mutant["tool_identity"], a["tool_identity"])
        errors = self.reject(f, mutant, "C4-public-contradiction")
        self.assertIn("RMUX public path differs from frozen rmux_public identity", errors)

    def test_each_path_changed_and_each_pair_swapped(self):
        f, a = self.fixture()
        fields = tuple(a["rmux_binaries"])
        for field in fields:
            with self.subTest(changed=field):
                mutant = deepcopy(a)
                mutant["rmux_binaries"][field] = str(f.paths["tmux"])
                self.reject(f, mutant, "changed-" + field)
        for first, second in itertools.combinations(fields, 2):
            with self.subTest(swapped=(first, second)):
                mutant = deepcopy(a)
                mapping = mutant["rmux_binaries"]
                mapping[first], mapping[second] = mapping[second], mapping[first]
                self.reject(f, mutant, "swapped-" + first + "-" + second)

    def test_absolute_relative_spellings_and_all_symlink_roles(self):
        roles = ("rmux", "rmux_public", "rmux_helper", "rmux_daemon")
        f, a = self.fixture(relative_roles=roles, symlink_roles=roles)
        m = a["tool_identity"]["manifest_data"]
        for field, role in m["path_context"]["rmux_bindings"].items():
            self.assertFalse(Path(a["rmux_binaries"][field]).is_absolute())
            identity = m["tools"][role]["identity"]
            self.assertEqual(identity["declared_type"], "symlink")
            self.assertNotEqual(identity["resolved"], identity["selected"])
            a["rmux_binaries"][field] = identity["selected"]
        self.assertEqual(q.validate_artifact(a), [])
        save(f.directory / "equivalent-absolute.json", a)
        for field, role in m["path_context"]["rmux_bindings"].items():
            mutant = deepcopy(a)
            mutant["rmux_binaries"][field] = m["tools"][role]["identity"]["resolved"]
            self.reject(f, mutant, "resolved-target-is-not-selected-" + field)

    def test_implicit_public_client_fallback_and_absent_helper(self):
        for relative in (True, False):
            f, a = self.fixture(str(relative), explicit=False, helper=False,
                                relative_roles=("rmux",) if relative else ())
            self.assertEqual(a["rmux_binaries"]["client"], a["rmux_binaries"]["public"])
            self.assertIsNone(a["rmux_binaries"]["helper"])
            bindings = a["tool_identity"]["manifest_data"]["path_context"]["rmux_bindings"]
            self.assertEqual(bindings["public"], "rmux")
            self.assertIsNone(bindings["helper"])
            for field in ("public", "client", "helper"):
                mutant = deepcopy(a)
                mutant["rmux_binaries"][field] = str(f.paths["tmux"])
                self.reject(f, mutant, "fallback-changed-" + field)

    def test_explicit_public_can_equal_client_and_helper_can_be_absent(self):
        # Distinct roles may legitimately have identical file paths/bytes.
        f = PathFixture(EVIDENCE / self._testMethodName, helper=False, relative_roles=())
        f.paths["rmux_public"] = f.paths["rmux"]
        f.freeze_receipt(); f.collect(); a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(q.validate_artifact(a), [])

    def test_missing_or_invalid_mapping_fields_rejected(self):
        f, a = self.fixture()
        for field in a["rmux_binaries"]:
            for value in (None, "", 3, "<temp>/invented", "bad\0path"):
                with self.subTest(field=field, value=value):
                    mutant = deepcopy(a); mutant["rmux_binaries"][field] = value
                    self.reject(f, mutant, "bad-" + field + "-" + str((None, "", 3, "<temp>/invented", "bad\0path").index(value)))
            mutant = deepcopy(a); del mutant["rmux_binaries"][field]
            self.reject(f, mutant, "missing-" + field)

    def test_missing_contradictory_context_and_declarations_rejected_after_repin(self):
        f, a = self.fixture()
        for key in ("cwd", "schema", "flavor", "rmux_bindings"):
            mutant = deepcopy(a); del mutant["tool_identity"]["manifest_data"]["path_context"][key]
            self.reject(f, mutant, "missing-context-" + key, repin=True)
        for index, value in enumerate((None, "relative", "/different-cwd", "/bad\0cwd")):
            mutant = deepcopy(a); mutant["tool_identity"]["manifest_data"]["path_context"]["cwd"] = value
            self.reject(f, mutant, "bad-cwd-" + str(index), repin=True)
        mutant = deepcopy(a)
        mutant["tool_identity"]["manifest_data"]["path_context"]["rmux_bindings"]["public"] = "rmux"
        self.reject(f, mutant, "contradictory-public-role", repin=True)
        for role in ("rmux", "rmux_public", "rmux_helper", "rmux_daemon"):
            for key in ("declared", "selected", "lookup"):
                mutant = deepcopy(a)
                mutant["tool_identity"]["manifest_data"]["tools"][role]["declaration"][key] = str(f.paths["tmux"])
                self.reject(f, mutant, "bad-declaration-" + role + "-" + key, repin=True)

    def test_frozen_roles_swapped_rejected_after_repin(self):
        f, a = self.fixture()
        for first, second in itertools.combinations(("rmux", "rmux_public", "rmux_helper", "rmux_daemon"), 2):
            mutant = deepcopy(a); records = mutant["tool_identity"]["manifest_data"]["tools"]
            records[first], records[second] = records[second], records[first]
            self.reject(f, mutant, "swapped-roles-" + first + "-" + second, repin=True)

    def test_offline_validation_ignores_review_cwd_and_tool_files(self):
        f, a = self.fixture()
        elsewhere = f.directory / "review-cwd"; elsewhere.mkdir()
        with chdir(elsewhere):
            self.assertEqual(q.validate_artifact(a), [])
            with ExitStack() as stack:
                stack.enter_context(patch.object(subprocess, "Popen", side_effect=forbidden))
                for method in ("resolve", "stat", "lstat", "read_bytes", "read_text"):
                    stack.enter_context(patch.object(Path, method, side_effect=forbidden))
                stack.enter_context(patch.object(os, "getcwd", side_effect=forbidden))
                stack.enter_context(patch.object(t.shutil, "which", side_effect=forbidden))
                self.assertEqual(t.saved_errors(a["tool_identity"], a["sampling_plan"]["run_id"],
                                 a["requested_tools"], a["attempts"], a["rmux_binaries"]), [])

    def test_old_required_artifact_has_no_invented_path_proof(self):
        f, a = self.fixture()
        del a["tool_identity"]["manifest_data"]["path_context"]
        del a["rmux_binaries"]["client"]
        self.reject(f, a, "old-required-missing-proof", repin=True)
        a["tool_identity"]["mode"] = "historical"
        a["tool_identity"]["manifest_data"]["mode"] = "historical"
        self.reject(f, a, "historical-cannot-keep-verified-status", repin=True)
        a["tool_identity"]["status"] = "unqualified_historical"
        self.assertEqual(q.validate_artifact(a), [])  # Only R5 accounting, no R6 status.


if __name__ == "__main__":
    EVIDENCE = Path(sys.argv[1]).absolute(); EVIDENCE.mkdir(parents=True, exist_ok=False)
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(PathControls))
    save(EVIDENCE / "summary.json", {"tests": result.testsRun, "pass": result.wasSuccessful(),
         "failures": len(result.failures), "errors": len(result.errors), "product_runs": 0,
         "child_processes": None, "guarded_provenance_git_processes": GIT_PROCESSES,
         "performance_evidence": False, "receipts_are_synthetic": True})
    raise SystemExit(not result.wasSuccessful())
