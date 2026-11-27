"""D155 controls use synthetic R2 evidence; they cannot approve real amendments."""
import copy
import hashlib
import json
from pathlib import Path, PureWindowsPath
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "scripts/bench"))
import pin_revisions as revisions
import provenance_archive as archive
import qualification as q
import pin_revision_sources as sources
from pin_activation_controls import PinActivationControls


class PinRevisionTests(PinActivationControls, unittest.TestCase):
    def setUp(self):
        directory = ROOT / "target/pin-revision-tests"
        directory.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=directory)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Sidney Sissaoui")
        self.git("config", "user.email", "shideneyu@gmail.com")
        self.git("config", "commit.gpgsign", "false")
        self.projection = json.loads((ROOT / "benches/perf/qualification/population.json").read_bytes())
        # Always start at the historical epoch, even after real revisions exist.
        for name, (_, old, _) in revisions.ALLOWED.items():
            self.projection["preserved_definitions"][name]["ast_sha256"] = old
        self.original = subprocess.check_output([
            "git", "-C", str(ROOT), "cat-file", "blob", self.projection["historical_source"]["git_blob"]])
        self.git("hash-object", "-w", "--stdin", input=self.original)
        self.document = {"schema": revisions.SCHEMA, "archive_sha256": revisions.ARCHIVE_SHA256,
                         "entries": []}
        self.write_chain()
        self.commit()
        initial = self.git("rev-parse", "HEAD").decode().strip()
        self.pin_commits = {}
        for name in revisions.ALLOWED:
            self.git("checkout", "-q", "--detach", initial)
            (self.root / (name + ".txt")).write_text("synthetic pin commit\n")
            self.commit()
            self.pin_commits[name] = self.git("rev-parse", "HEAD").decode().strip()
        self.git("checkout", "-q", "--detach", initial)
        self.proof_path = self.root / revisions.PROOFS / revisions.ATTRIBUTION / "PROOF.json"
        self.proof_path.parent.mkdir(parents=True)
        self.evidence = {"mandate": revisions.ATTRIBUTION, "verdict": "PASS",
                         "reviewer_role": "R2 independent scope proof",
                         "created_utc": "2026-10-17T02:28:40+00:00",
                         "method": "SYNTHETIC fixture, not independent real evidence",
                         "conditions": {"D147": True, "D152": True}, "revisions": []}
        for name, (decision, old, new) in revisions.ALLOWED.items():
            conditions = {key: True for key in revisions.REQUIRED_CONDITIONS}
            if name == "screen_cleanup":
                conditions["single_original_5s_budget"] = True
            self.evidence["revisions"].append({"function": name, "decision": decision,
                "old_ast_sha256": old, "new_ast_sha256": new, "conditions": conditions,
                "pin_commit": self.pin_commits[name]})
        self.proof_path.write_text(json.dumps(self.evidence))

    def git(self, *args, input=None):
        return subprocess.check_output(["git", "-C", str(self.root), *args], input=input,
                                       stderr=subprocess.PIPE)

    def commit(self):
        self.git("add", ".")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "synthetic D155 epoch")

    def write_chain(self):
        path = self.root / revisions.CHAIN
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.document))

    def append(self, name, *, activate=True):
        if activate:
            self.activate(name)
        rows = self.document["entries"]
        decision, old, new = revisions.ALLOWED[name]
        row = {"id": decision + "-" + name, "pin": name, "old_ast_sha256": old,
               "new_ast_sha256": new, "decision": decision,
               "proof": {"path": self.proof_path.relative_to(self.root).as_posix(),
                         "sha256": hashlib.sha256(self.proof_path.read_bytes()).hexdigest()},
               "recorded_at": "2026-10-17T02:28:4%dZ" % (len(rows) + 1),
               "attribution": revisions.ATTRIBUTION,
               "previous_sha256": rows[-1]["sha256"] if rows else revisions.ARCHIVE_SHA256}
        row["sha256"] = revisions.digest(row)
        rows.append(row)
        self.write_chain()

    def seal(self):
        previous = revisions.ARCHIVE_SHA256
        for row in self.document["entries"]:
            row["previous_sha256"] = previous
            row["sha256"] = revisions.digest({k: v for k, v in row.items() if k != "sha256"})
            previous = row["sha256"]
        self.write_chain()

    def revised(self, *names):
        value = copy.deepcopy(self.projection)
        for name in names:
            value["preserved_definitions"][name]["ast_sha256"] = revisions.ALLOWED[name][2]
        return value

    def decode(self, value, **options):
        return archive.decode(json.dumps(value).encode(), self.root, **options)

    def merge_hidden_revisions(self, *, rewrite=False, extra_versions=0):
        self.commit()  # Keep the synthetic proof available on both parents.
        base = self.git("rev-parse", "HEAD").decode().strip()
        self.git("checkout", "-qb", "revisions")
        self.append("relative_or_string")
        self.append("screen_cleanup")
        self.commit()  # A: the two declared revisions.
        self.decode(self.revised(*revisions.ALLOWED), current=True)
        for index in range(extra_versions):
            (self.root / revisions.CHAIN).write_text(json.dumps(self.document) + "\n" * (index + 1))
            self.commit()
        added = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-qb", "empty", base)
        if rewrite:
            for index, row in enumerate(self.document["entries"]):
                row["recorded_at"] = "2026-10-17T02:28:4%dZ" % (index + 3)
            self.seal()
        else:
            self.document["entries"].clear()
        (self.root / "sibling.txt").write_text("B keeps its chosen chain\n")
        self.commit()
        self.assertEqual(revisions.entries(self.root), self.document["entries"])
        self.git("merge", "--no-ff", "--no-edit", "-s", "ours", "revisions")
        # M is TREESAME to B: default path simplification hides parent A.
        self.assertNotIn(added, self.git("log", "--format=%H", "HEAD", "--", str(revisions.CHAIN)).splitlines())
        self.assertIn(added, self.git("log", "--full-history", "--format=%H", "HEAD", "--", str(revisions.CHAIN)).splitlines())

    def assert_history_rejected(self, projection, message="history rewritten or removed"):
        manifest = self.root / "population.json"
        manifest.write_text(json.dumps(projection))
        readers = {
            "entries": lambda: revisions.entries(self.root),
            "provenance historical projection": lambda: self.decode(projection),
            "provenance current projection": lambda: self.decode(projection, current=True),
            "provenance historical read": lambda: archive.read(manifest, self.root),
            "provenance current read": lambda: archive.read(manifest, self.root, current=True),
            "provenance current raw": lambda: archive.decode(self.original, self.root, current=True),
            "provenance current locator": lambda: self.decode(self.projection["historical_source"], current=True),
        }
        for name, read_population in readers.items():
            with self.subTest(reader=name), self.assertRaisesRegex(ValueError, message):
                read_population()
        read = archive.read
        with patch.object(q, "MANIFEST", manifest), patch.object(archive, "read",
                side_effect=lambda path, repository, **kw: read(path, self.root, **kw)):
            for read_population in (q.population, q.population_bytes):
                with self.subTest(reader=read_population.__name__), self.assertRaisesRegex(q.QualificationError, message):
                    read_population()
        # Original archive-only reads do not select an epoch of the revision chain.
        self.assertEqual(self.decode(self.projection["historical_source"]), self.original)
        self.assertEqual(archive.decode(self.original, self.root), self.original)

    def test_atomic_activation_tool_only_keeps_exact_historical_current_pin(self):
        # D157: sealed future revisions cannot activate before their pin commits.
        # Install the byte-exact real chain/proof in a history without pin commits.
        self.document = json.loads((ROOT / revisions.CHAIN).read_bytes())
        self.write_chain()
        self.proof_path.write_bytes(revisions.proof_bytes(ROOT))
        self.assertEqual(self.decode(self.projection, current=True), self.original)
        self.assertEqual(len(json.loads(self.original)["preserved_definitions"]), 105)

    def test_windows_chain_uses_git_paths_for_log_and_show(self):
        chain = PureWindowsPath(revisions.CHAIN.as_posix())
        with patch.object(revisions, "CHAIN", chain), patch.object(
                revisions, "_git", wraps=revisions._git) as git:
            revisions._append_only(self.root, [])
        self.assertEqual(git.call_args_list[0].args,
                         (self.root, "log", "--full-history", "--format=%H", "-65",
                          "HEAD", "--", chain.as_posix()))
        self.assertGreater(len(git.call_args_list), 1)
        for call in git.call_args_list[1:]:
            self.assertEqual(call.args[1], "show")
            self.assertEqual(call.args[2].split(":", 1)[1], chain.as_posix())

    def test_windows_chain_reads_real_git_object_after_history_discovery(self):
        chain = PureWindowsPath(revisions.CHAIN.as_posix())
        commits = self.git("log", "--full-history", "--format=%H", "-65",
                           "HEAD", "--", chain.as_posix())
        self.assertTrue(commits.strip())
        git = revisions._git

        def discovered_history(repository, *arguments):
            # Isolate object lookup from platform-specific pathspec handling.
            if arguments[0] == "log":
                return commits
            return git(repository, *arguments)

        with patch.object(revisions, "CHAIN", chain), patch.object(
                revisions, "_git", side_effect=discovered_history):
            revisions._append_only(self.root, [])

    def test_windows_chain_preserves_append_only_history(self):
        self.append("relative_or_string")
        self.commit()
        self.append("screen_cleanup")
        self.commit()
        rows = self.document["entries"]
        rewritten = copy.deepcopy(rows)
        rewritten[0]["recorded_at"] = "2026-10-17T02:28:43Z"
        chain = PureWindowsPath(revisions.CHAIN.as_posix())
        with patch.object(revisions, "CHAIN", chain):
            revisions._append_only(self.root, rows)
            for invalid in ([], rows[:-1], rewritten):
                with self.subTest(entries=invalid), self.assertRaisesRegex(
                        ValueError, "history rewritten or removed"):
                    revisions._append_only(self.root, invalid)

    def test_merge_cannot_hide_committed_revisions(self):
        self.merge_hidden_revisions()
        self.assert_history_rejected(self.projection)

    def test_merge_cannot_hide_resealed_rewrite(self):
        self.merge_hidden_revisions(rewrite=True)
        self.assert_history_rejected(self.revised(*revisions.ALLOWED))

    def test_hidden_merge_parent_counts_toward_history_bound(self):
        self.merge_hidden_revisions(extra_versions=63)
        self.assert_history_rejected(self.projection, "history exceeds bounded D155 scope")

    def test_history_bound_accepts_64_versions_and_rejects_65(self):
        chain = self.root / revisions.CHAIN
        for index in range(1, 64):
            chain.write_text(json.dumps(self.document) + "\n" * index)
            self.commit()
        self.assertEqual(revisions.entries(self.root), [])
        chain.write_text(json.dumps(self.document) + "\n" * 64)
        self.commit()
        with self.assertRaisesRegex(ValueError, "history exceeds bounded D155 scope"):
            revisions.entries(self.root)

    def test_append_only_merge_preserves_every_epoch_and_current_pins(self):
        self.commit()
        base = self.git("rev-parse", "HEAD").decode().strip()
        self.git("checkout", "-qb", "revisions")
        self.append("relative_or_string")
        self.commit()
        self.git("checkout", "-qb", "sibling", base)
        (self.root / "sibling.txt").write_text("unrelated change\n")
        self.commit()
        # Import a revision from the second parent, then extend it after the merge.
        self.git("merge", "--no-ff", "--no-edit", "revisions")
        self.append("screen_cleanup")
        self.commit()
        self.assertEqual(revisions.entries(self.root), self.document["entries"])
        for snapshot in (self.projection, self.revised("relative_or_string"),
                         self.revised(*revisions.ALLOWED)):
            self.assertEqual(self.decode(snapshot), self.original)
        manifest = self.root / "population.json"
        manifest.write_text(json.dumps(self.revised(*revisions.ALLOWED)))
        read = archive.read
        with patch.object(q, "MANIFEST", manifest), patch.object(archive, "read",
                side_effect=lambda path, repository, **kw: read(path, self.root, **kw)):
            self.assertEqual(q.population_bytes(), self.original)
            effective = q.population()["preserved_definitions"]
            for name, (_, _, new) in revisions.ALLOWED.items():
                self.assertEqual(effective[name]["ast_sha256"], new)
        with self.assertRaisesRegex(ValueError, "stale"):
            self.decode(self.projection, current=True)

    def test_each_pin_red_green_withdrawal_restored(self):
        for name in revisions.ALLOWED:
            with self.subTest(pin=name):
                value = self.revised(name)
                with self.assertRaisesRegex(ValueError, "undeclared"):
                    self.decode(value, current=True)
                self.append(name)
                self.assertEqual(json.loads(self.decode(value, current=True))["preserved_definitions"],
                                 value["preserved_definitions"])
                saved = self.document["entries"].pop()
                self.write_chain()
                with self.assertRaises(ValueError):
                    self.decode(value, current=True)
                self.document["entries"].append(saved)
                self.write_chain()
                self.decode(value, current=True)
                self.document["entries"].clear()
                self.write_chain()

    def test_each_historical_epoch_remains_byte_exact_current_requires_latest(self):
        snapshots = [self.projection]
        self.append("relative_or_string")
        snapshots.append(self.revised("relative_or_string"))
        self.commit()
        self.append("screen_cleanup")
        snapshots.append(self.revised(*revisions.ALLOWED))
        for index, snapshot in enumerate(snapshots):
            with self.subTest(epoch=index):
                self.assertEqual(self.decode(snapshot), self.original)
                if index < 2:
                    with self.assertRaisesRegex(ValueError, "stale"):
                        self.decode(snapshot, current=True)
        effective = json.loads(self.decode(snapshots[-1], current=True))
        expected = json.loads(self.original)
        for name in revisions.ALLOWED:
            expected["preserved_definitions"][name]["ast_sha256"] = revisions.ALLOWED[name][2]
        self.assertEqual(effective, expected)
        # A state that never existed at an epoch is not historical evidence.
        with self.assertRaises(ValueError):
            self.decode(self.revised("screen_cleanup"))

    def test_declared_revisions_do_not_rewrite_original_bytes(self):
        self.append("relative_or_string")
        self.append("screen_cleanup")
        self.assertEqual(self.decode(self.revised(*revisions.ALLOWED)), self.original)

    def test_unknown_revision_decision_and_attribution_are_rejected_even_resealed(self):
        self.append("relative_or_string")
        good = copy.deepcopy(self.document)
        for field, value in (("pin", "sanitize"), ("id", "unknown-revision"),
                             ("decision", "D152"), ("attribution", "S1-self-approval"),
                             ("old_ast_sha256", "0" * 64), ("new_ast_sha256", "1" * 64)):
            with self.subTest(field=field):
                self.document = copy.deepcopy(good)
                self.document["entries"][0][field] = value
                self.seal()
                with self.assertRaises(ValueError):
                    self.decode(self.revised("relative_or_string"))

    def test_proof_unreadable_changed_or_outside_attribution_is_rejected(self):
        self.append("relative_or_string")
        raw = self.proof_path.read_bytes()
        for content in (b"tampered", None):
            with self.subTest(content=content):
                if content is None:
                    self.proof_path.unlink()
                else:
                    self.proof_path.write_bytes(content)
                with self.assertRaisesRegex(ValueError, "R2 proof"):
                    self.decode(self.revised("relative_or_string"))
        self.proof_path.write_bytes(raw)
        self.document["entries"][0]["proof"]["path"] = "../PROOF.json"
        self.seal()
        with self.assertRaisesRegex(ValueError, "attribution"):
            self.decode(self.revised("relative_or_string"))

    def test_resealed_proof_must_cover_exact_pin_and_conditions(self):
        self.append("relative_or_string")
        for field, value in (("verdict", "FAIL"), ("mandate", "self-authored"),
                             ("conditions", {"D147": True, "D152": False}),
                             ("revisions", [])):
            with self.subTest(field=field):
                bad = copy.deepcopy(self.evidence)
                bad[field] = value
                self.proof_path.write_text(json.dumps(bad))
                self.document["entries"][0]["proof"]["sha256"] = hashlib.sha256(self.proof_path.read_bytes()).hexdigest()
                self.seal()
                with self.assertRaises(ValueError):
                    self.decode(self.revised("relative_or_string"))

    def test_committed_tail_cannot_be_removed_or_rewritten_even_resealed(self):
        self.append("relative_or_string")
        self.commit()
        good = copy.deepcopy(self.document)
        self.document["entries"] = []
        self.write_chain()
        with self.assertRaisesRegex(ValueError, "rewritten or removed"):
            self.decode(self.projection)
        self.document = copy.deepcopy(good)
        self.document["entries"][0]["recorded_at"] = "2026-10-17T02:28:43Z"
        self.seal()
        with self.assertRaisesRegex(ValueError, "rewritten or removed"):
            self.decode(self.revised("relative_or_string"))
        # Committing the rewrite does not launder its history.
        self.commit()
        with self.assertRaisesRegex(ValueError, "rewritten or removed"):
            self.decode(self.revised("relative_or_string"))

    def test_missing_scope_budget_and_misattributed_proof_are_rejected(self):
        self.append("screen_cleanup")
        mutations = [lambda e: e.update(reviewer_role="S1"),
                     lambda e: e.update(created_utc="2099-01-01T00:00:00+00:00"),
                     lambda e: e["revisions"][1]["conditions"].pop("single_original_5s_budget"),
                     lambda e: e["revisions"][1]["conditions"].pop("recipe_and_population_fields_unchanged"),
                     lambda e: e["revisions"][1].update(new_ast_sha256="0" * 64)]
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index):
                bad = copy.deepcopy(self.evidence)
                mutate(bad)
                self.proof_path.write_text(json.dumps(bad))
                self.document["entries"][0]["proof"]["sha256"] = hashlib.sha256(self.proof_path.read_bytes()).hexdigest()
                self.seal()
                with self.assertRaises(ValueError):
                    self.decode(self.revised("screen_cleanup"))
    def test_missing_chain_and_symlink_to_external_evidence_are_rejected(self):
        self.append("relative_or_string")
        chain = self.root / revisions.CHAIN
        raw = chain.read_bytes()
        chain.unlink()
        with self.assertRaisesRegex(ValueError, "chain unreadable"):
            self.decode(self.projection)
        chain.write_bytes(raw)
        outside = self.root.parent / (self.root.name + "-outside")
        outside.write_bytes(self.proof_path.read_bytes())
        self.addCleanup(outside.unlink)
        self.proof_path.unlink()
        self.proof_path.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "escapes"):
            self.decode(self.revised("relative_or_string"))

    def test_duplicate_json_keys_schema_and_reversed_dates_are_rejected(self):
        self.append("relative_or_string")
        self.append("screen_cleanup")
        good = copy.deepcopy(self.document)
        self.document["entries"][1]["recorded_at"] = self.document["entries"][0]["recorded_at"]
        self.seal()
        with self.assertRaisesRegex(ValueError, "dates"):
            self.decode(self.revised(*revisions.ALLOWED))
        self.document = copy.deepcopy(good)
        self.document["schema"] = "unknown-revision-schema"
        self.write_chain()
        with self.assertRaises(ValueError):
            self.decode(self.projection)
        (self.root / revisions.CHAIN).write_text(json.dumps(good).replace(
            '"entries":', '"entries": [], "entries":', 1))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.decode(self.projection)

    def test_hash_link_dates_duplicate_entries_and_fields_fail_closed(self):
        self.append("relative_or_string")
        good = copy.deepcopy(self.document)
        for field, value in (("previous_sha256", "0" * 64), ("sha256", "0" * 64),
                             ("recorded_at", "unknown"), ("extra", True)):
            with self.subTest(field=field):
                self.document = copy.deepcopy(good)
                self.document["entries"][0][field] = value
                self.write_chain()
                with self.assertRaises(ValueError):
                    self.decode(self.revised("relative_or_string"))
        self.document = copy.deepcopy(good)
        self.document["entries"].append(copy.deepcopy(good["entries"][0]))
        self.seal()
        with self.assertRaisesRegex(ValueError, "repeated"):
            self.decode(self.revised("relative_or_string"))

    def test_every_other_pin_recipe_and_population_field_remains_frozen(self):
        self.append("relative_or_string")
        good = self.revised("relative_or_string")
        for name in good["preserved_definitions"]:
            if name in revisions.ALLOWED:
                continue
            with self.subTest(pin=name):
                changed = copy.deepcopy(good)
                changed["preserved_definitions"][name]["ast_sha256"] = "0" * 64
                with self.assertRaises(ValueError):
                    self.decode(changed, current=True)
        for field in ("operations", "public_operations", "tools", "recipe_limits"):
            with self.subTest(field=field):
                changed = copy.deepcopy(good)
                changed[field] = []
                with self.assertRaises(ValueError):
                    self.decode(changed, current=True)
        changed = copy.deepcopy(good)
        changed["preserved_definitions"]["relative_or_string"]["sha256"] = "0" * 64
        with self.assertRaises(ValueError):
            self.decode(changed, current=True)

    def test_current_raw_or_locator_cannot_bypass_latest_pin(self):
        self.append("relative_or_string")
        for raw in (self.original, json.dumps(self.projection["historical_source"]).encode(),
                    json.dumps(self.revised("relative_or_string")["preserved_definitions"]).encode()):
            with self.subTest(raw=raw[:40]), self.assertRaises(ValueError):
                archive.decode(raw, self.root, current=True)
        # The original-only locator remains a valid historical read.
        self.assertEqual(self.decode(self.projection["historical_source"]), self.original)

    def test_qualification_verifies_each_corrected_source_against_effective_pin(self):
        read = archive.read
        manifest = self.root / "population.json"
        for name in revisions.ALLOWED:
            with self.subTest(pin=name):
                source = sources.revised_source(name)
                bindings = [(row["id"], SimpleNamespace(__name__=row["function"]))
                            for row in self.projection["operations"]]
                manifest.write_text(json.dumps(self.revised(name)))
                with patch.object(q, "MANIFEST", manifest), patch.object(archive, "read",
                        side_effect=lambda path, repository, **kw: read(path, self.root, **kw)):
                    with self.assertRaises(q.QualificationError):
                        q.verify_source(source, bindings)
                    self.append(name)
                    q.verify_source(source, bindings)
                    with self.assertRaisesRegex(q.QualificationError, "frozen recipe/setup changed: " + name):
                        q.verify_source(source + "\ndef " + name + "(): pass\n", bindings)
                    self.assertEqual(q.population_bytes(), self.original)
                    self.document["entries"].clear()
                    self.write_chain()
                    with self.assertRaises(q.QualificationError):
                        q.verify_source(source, bindings)
                    self.append(name)
                    q.verify_source(source, bindings)
                self.document["entries"].clear()
                self.write_chain()

    def test_schedule_uses_frozen_population_current_source_still_requires_latest(self):
        self.append("relative_or_string")
        self.append("screen_cleanup")
        manifest = self.root / "population.json"
        manifest.write_text(json.dumps(self.projection))
        read = archive.read
        with patch.object(q, "MANIFEST", manifest), patch.object(archive, "read",
                side_effect=lambda path, repository, **kw: read(path, self.root, **kw)):
            with self.assertRaisesRegex(q.QualificationError, "stale"):
                q.population()
            planned = q.schedule("frozen", ["new_session_warm_sh"], ["rmux", "tmux"], 15)
            self.assertEqual(len(planned), 30)
            self.assertEqual(q.population_bytes(), self.original)
            self.commit()
            self.document["entries"].pop()
            self.write_chain()
            with self.assertRaisesRegex(q.QualificationError, "history rewritten or removed"):
                q.schedule("frozen", ["new_session_warm_sh"], ["rmux", "tmux"], 15)

    def test_real_chain_activates_only_both_authorized_pins_in_frozen_neighborhood(self):
        rows = revisions.entries(ROOT)
        self.assertEqual([row["pin"] for row in rows], list(revisions.ALLOWED))
        proof = json.loads(revisions.proof_bytes(ROOT))
        active = []
        for row in proof["revisions"]:
            # An independent Git ancestry query is the oracle for rev-list.
            result = subprocess.run(["git", "-C", str(ROOT), "merge-base", "--is-ancestor",
                                     row["pin_commit"], "HEAD"], capture_output=True)
            self.assertIn(result.returncode, (0, 1))
            if result.returncode == 0:
                active.append(row["function"])
        effective = self.revised(*active)
        for snapshot in (self.projection, effective):
            self.assertEqual(archive.decode(json.dumps(snapshot).encode(), ROOT), self.original)
        archive.decode(json.dumps(effective).encode(), ROOT, current=True)
        manifest = self.root / "population.json"
        manifest.write_text(json.dumps(effective))
        bindings = [(row["id"], SimpleNamespace(__name__=row["function"]))
                    for row in self.projection["operations"]]
        with patch.object(q, "MANIFEST", manifest):
            q.verify_source(sources.revised_source(*active), bindings)
            for name in revisions.ALLOWED:
                changed = set(active) ^ {name}
                with self.subTest(pin=name), self.assertRaises(q.QualificationError):
                    q.verify_source(sources.revised_source(*changed), bindings)
            changed = sources.revised_source(*active) + "\nclass Adapter: pass\n"
            with self.assertRaisesRegex(q.QualificationError, "Adapter"):
                q.verify_source(changed, bindings)
        for names in ([], ["relative_or_string"], ["screen_cleanup"], list(revisions.ALLOWED)):
            if set(names) != set(active):
                with self.subTest(pins=names), self.assertRaisesRegex(ValueError, "stale"):
                    archive.decode(json.dumps(self.revised(*names)).encode(), ROOT, current=True)

    def test_all_105_historical_ast_pins_keep_their_original_digest(self):
        definitions = {node.name: node for node in q.ast.parse(sources.historical_source()).body
                       if isinstance(node, (q.ast.FunctionDef, q.ast.ClassDef))}
        pins = self.projection["preserved_definitions"]
        self.assertEqual(len(pins), 105)
        for name, pin in pins.items():
            with self.subTest(definition=name):
                actual = q.frozen_ast.dump(definitions[name])
                self.assertEqual(hashlib.sha256(actual.encode()).hexdigest(), pin["ast_sha256"])

    def test_schedule_rejects_unbound_raw_populations(self):
        manifest = self.root / "population.json"
        unbound = {key: value for key, value in self.projection.items() if key != "historical_source"}
        for value in (unbound, {**unbound, "recipe_limits": []}):
            with self.subTest(population=value["recipe_limits"]):
                manifest.write_text(json.dumps(value))
                with patch.object(q, "MANIFEST", manifest), self.assertRaises(q.QualificationError):
                    q.schedule("raw", ["new_session_warm_sh"], ["rmux", "tmux"], 15)

    def test_schedule_raw_archive_cannot_bypass_committed_revision_history(self):
        self.append("relative_or_string")
        self.commit()
        self.document["entries"].clear()
        self.write_chain()
        manifest = self.root / "population.json"
        manifest.write_bytes(self.original)
        entries = revisions.entries
        with patch.object(q, "MANIFEST", manifest), patch.object(revisions, "entries",
                side_effect=lambda repository: entries(self.root)):
            with self.assertRaisesRegex(q.QualificationError, "history rewritten or removed"):
                q.schedule("raw", ["new_session_warm_sh"], ["rmux", "tmux"], 15)


if __name__ == "__main__":
    unittest.main()
