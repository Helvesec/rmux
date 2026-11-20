"""Synthetic falsifiers of phase-0 contracts; no product process or network (local Git fixtures only)."""
import base64
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/validation"))
import contracts
import poison
import reference_applicability
import web_profile


def load(name):
    return json.loads((ROOT / "docs/validation" / name).read_text())


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


class ReferenceApplicability(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads(contracts.read_reference(ROOT, reference_applicability.POLICY))

    def test_archived_receipts_bind_exact_current_bytes(self):
        self.assertEqual(reference_applicability.validate_policy(self.policy), self.policy["binding"])
        raw = contracts.read_reference(ROOT, reference_applicability.POLICY)
        self.assertEqual(json.loads(raw), self.policy)

    def test_archived_receipt_bytes_cannot_be_rewritten(self):
        self.policy["evidence"]["review"]["text"] += " "
        with self.assertRaisesRegex(ValueError, "receipt bytes differ"):
            reference_applicability.validate_policy(self.policy)

    def test_different_review_authority_cannot_authorize_same_hash(self):
        self.policy["binding"]["review"]["job_id"] = "R2-999"
        with self.assertRaisesRegex(ValueError, "authority identity differs"):
            reference_applicability.validate_policy(self.policy)

    def test_date_must_equal_original_receipt(self):
        self.policy["binding"]["review"]["completed_at"] = "2026-09-28T10:11:34+00:00"
        with self.assertRaisesRegex(ValueError, "unbound authority date"):
            reference_applicability.validate_policy(self.policy)

    def test_integration_source_cannot_be_reattributed(self):
        self.policy["binding"]["current"]["commit"] = "0" * 40
        with self.assertRaisesRegex(ValueError, "unbound integrated source"):
            reference_applicability.validate_policy(self.policy)

    def test_same_path_does_not_authorize_new_current_bytes(self):
        self.policy["binding"]["current"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "unbound current source"):
            reference_applicability.validate_policy(self.policy)

    def test_historical_source_digest_remains_bound(self):
        self.policy["binding"]["historical"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "unbound source proof"):
            reference_applicability.validate_policy(self.policy)

    def test_chronology_cannot_postdate_consumer(self):
        self.policy["source_proof"]["integrated"]["commit_date"] = "2026-09-28T12:25:08+02:00"
        with self.assertRaisesRegex(ValueError, "authority chronology"):
            reference_applicability.validate_policy(self.policy)

    def test_unzoned_authority_date_is_rejected(self):
        self.policy["source_proof"]["integrated"]["commit_date"] = "2026-09-27T10:25:08"
        with self.assertRaisesRegex(ValueError, "undated authority"):
            reference_applicability.validate_policy(self.policy)

    def test_source_proof_cannot_claim_a_different_composed_blob(self):
        self.policy["source_proof"]["qualified_base"]["blob"] = "0" * 40
        with self.assertRaisesRegex(ValueError, "unbound source proof"):
            reference_applicability.validate_policy(self.policy)


class ReferencePopulation(unittest.TestCase):
    def test_actual_reference_contract_is_complete_without_product_credit(self):
        result = contracts.validate()
        self.assertEqual(result["families"], 12)
        self.assertEqual(result["legacy_scenarios"], 2865)
        self.assertEqual(result["product_conformance"], "NOT_EVALUATED")
        self.assertEqual(result["web_indistinguishability"], "NOT_EVALUATED")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "tests").mkdir()
        text = '#[test]\nfn first() {}\n#[tokio::test]\nasync fn second() {}\n'
        (self.root / "tests/old.rs").write_text(text)
        self.expected = [{"id": "old-rust", "reader": "rust-tests",
                          "source": {"path": "tests/old.rs", "sha256": sha(text)},
                          "legacy_ids_sha256": contracts.digest(["first#1", "second#1"]), "cardinality": 2}]
        self.manifest = {"schema": 1, "evidence_status": "SOURCE_IDENTITIES_NOT_EXECUTION_PROOF",
                         "cardinality": 2, "collections": [{"id": "old-rust", "reader": "rust-tests",
                         "source": {"path": "tests/old.rs", "sha256": sha(text)}, "family": "runtime",
                         "legacy_ids": ["first#1", "second#1"], "mapping": "collection-id/legacy-id",
                         "contract_fields": {key: "frozen test contract" for key in
                             ("target", "features", "prerequisites", "observable", "risk", "neighbors", "platform", "oracle")}}]}

    def check(self):
        return contracts.inventory(self.root, self.manifest, self.expected)

    def test_legacy_mapping_is_exact_and_not_a_new_execution(self):
        mapped = self.check()
        self.assertEqual(set(mapped), {"old-rust/first#1", "old-rust/second#1"})
        self.assertTrue(all(row["status"] == "NOT_REQUALIFIED" for row in mapped.values()))

    def test_empty_population_fails(self):
        self.manifest["collections"] = []
        with self.assertRaisesRegex(ValueError, "empty"):
            self.check()

    def test_omitted_old_scenario_fails(self):
        self.manifest["collections"][0]["legacy_ids"].pop()
        self.manifest["cardinality"] = 1
        with self.assertRaisesRegex(ValueError, "omission"):
            self.check()

    def test_renamed_old_scenario_cannot_keep_old_proof(self):
        (self.root / "tests/old.rs").write_text('#[test]\nfn moved() {}\n')
        with self.assertRaisesRegex(ValueError, "stale reference"):
            self.check()

    def test_repin_does_not_silently_change_mapping(self):
        text = '#[test]\nfn moved() {}\n'
        (self.root / "tests/old.rs").write_text(text)
        self.manifest["collections"][0]["source"]["sha256"] = sha(text)
        with self.assertRaisesRegex(ValueError, "omission"):
            self.check()

    def frozen_history(self):
        """Commit the fixture as the frozen base of a private repository, as in the real checkout."""
        def git(*args):
            return subprocess.run(["git", "-C", str(self.root), "-c", "user.name=fixture",
                                   "-c", "user.email=fixture@invalid", *args],
                                  check=True, capture_output=True, text=True).stdout.strip()
        git("init", "-q")
        git("add", "tests/old.rs")
        git("commit", "-q", "-m", "frozen base")
        base = git("rev-parse", "HEAD")
        return mock.patch.object(contracts, "reference_population",
                                 return_value={"historical_source": {"commit": base}})

    def test_evolved_tree_reads_frozen_bytes_and_keeps_legacy_identities(self):
        with self.frozen_history():
            (self.root / "tests/old.rs").write_text(
                '#[test]\nfn first() { changed(); }\n#[test]\nfn added() {}\n'
                '#[tokio::test]\nasync fn second() {}\n')
            mapped = self.check()
        # The addition is not a legacy identity and gains no credit.
        self.assertEqual(set(mapped), {"old-rust/first#1", "old-rust/second#1"})

    def test_evolved_tree_cannot_drop_rename_or_reorder_a_legacy_identity(self):
        texts = ('#[test]\nfn first() {}\n',
                 '#[test]\nfn first() {}\n#[tokio::test]\nasync fn renamed() {}\n',
                 '#[tokio::test]\nasync fn second() {}\n#[test]\nfn first() {}\n')
        with self.frozen_history():
            for text in texts:
                (self.root / "tests/old.rs").write_text(text)
                with self.subTest(text=text), self.assertRaisesRegex(ValueError, "legacy identity lost or reordered"):
                    self.check()

    def test_removed_legacy_collection_fails_without_a_checker_owned_transition(self):
        with self.frozen_history():
            (self.root / "tests/old.rs").unlink()
            with self.assertRaisesRegex(ValueError, "legacy collection absent from the tree"):
                self.check()

    def test_transition_is_bound_to_its_collection_path(self):
        for record in (self.manifest["collections"][0], self.expected[0]):
            record["id"] = "bench_screen_adapter_spec.rs"
        with self.frozen_history():
            (self.root / "tests/old.rs").unlink()
            with self.assertRaisesRegex(ValueError, "identity transition path"):
                self.check()

    def test_evolved_identities_are_not_read_through_an_escaping_symlink(self):
        outside = tempfile.TemporaryDirectory()
        self.addCleanup(outside.cleanup)
        target = Path(outside.name) / "old.rs"
        target.write_text('#[test]\nfn first() {}\n#[tokio::test]\nasync fn second() {}\n#[test]\nfn x() {}\n')
        with self.frozen_history():
            (self.root / "tests/old.rs").unlink()
            (self.root / "tests/old.rs").symlink_to(target)
            with self.assertRaisesRegex(ValueError, "escapes root"):
                self.check()

    def test_frozen_object_must_match_the_cited_bytes(self):
        with self.frozen_history():
            self.manifest["collections"][0]["source"]["sha256"] = sha("other bytes")
            self.expected[0]["source"]["sha256"] = sha("other bytes")
            with self.assertRaisesRegex(ValueError, "stale reference"):
                self.check()
            with self.assertRaisesRegex(ValueError, "frozen source bytes differ"):
                contracts.frozen_source_bytes(self.root, {"path": "tests/old.rs", "sha256": sha("other bytes")})

    def test_only_the_tree_own_history_is_consulted(self):
        with self.frozen_history():
            nested = self.root / "nested"
            (nested / "tests").mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, "own Git history"):
                contracts.frozen_source_bytes(nested, self.manifest["collections"][0]["source"])

    def test_checker_owned_transitions_are_exact_and_load_bearing(self):
        self.assertEqual(set(contracts.IDENTITY_TRANSITIONS),
                         {"issue8_interactive_exit.rs", "bench_screen_adapter_spec.rs",
                          "window_pane_flag_semantics.rs"})
        for collection, count in (("issue8_interactive_exit.rs", 6),
                                  ("window_pane_flag_semantics.rs", 34)):
            with self.subTest(collection=collection):
                manifest = load("legacy-scenarios.json")
                row = next(r for r in manifest["collections"] if r["id"] == collection)
                manifest.update(collections=[row], cardinality=len(row["legacy_ids"]))
                expected = [r for r in contracts.reference_population()["collections"]
                            if r["id"] == row["id"]]
                self.assertEqual(len(contracts.inventory(ROOT, manifest, expected)), count)
                with mock.patch.dict(contracts.IDENTITY_TRANSITIONS, clear=True), \
                        self.assertRaisesRegex(ValueError, "legacy identity lost or reordered"):
                    contracts.inventory(ROOT, manifest, expected)

    def test_unreviewed_harness_is_not_read_without_frozen_reader(self):
        contract = load("reference-contracts.json")
        policy = json.loads(contracts.read_reference(ROOT, reference_applicability.POLICY))
        harness = policy["binding"]["path"]
        (self.root / Path(harness).parent).mkdir(parents=True)
        (self.root / harness).write_text("# an unreviewed later harness\n")
        with self.assertRaisesRegex(ValueError, "unauthorized current harness"):
            reference_applicability.resolve(self.root, contract, contracts.read_reference)
        historical = policy["binding"]["historical"]["sha256"]
        seen = []
        effective, result = reference_applicability.resolve(
            self.root, contract, contracts.read_reference,
            read_frozen=lambda tree, ref: seen.append(ref))
        self.assertEqual(seen, [{"path": harness, "sha256": historical}])
        self.assertEqual(effective, seen[0])
        self.assertEqual((result["mode"], result["accepted_source"], result["native_credit"]),
                         ("FROZEN_HISTORICAL", policy["binding"]["historical"]["commit"], False))
        self.assertEqual(result["observed_sha256"], sha("# an unreviewed later harness\n"))

    def test_copy_under_another_collection_id_fails(self):
        alias = copy.deepcopy(self.manifest["collections"][0])
        alias["id"] = "alias"
        self.manifest["collections"].append(alias)
        self.manifest["cardinality"] = 4
        with self.assertRaisesRegex(ValueError, "source mapped more than once"):
            self.check()

    def test_missing_feature_or_prerequisite_contract_fails(self):
        del self.manifest["collections"][0]["contract_fields"]["features"]
        with self.assertRaisesRegex(ValueError, "contract field"):
            self.check()

    def test_source_reference_cannot_escape_checkout(self):
        self.manifest["collections"][0]["source"]["path"] = "../secret"
        with self.assertRaisesRegex(ValueError, "escapes"):
            self.check()

    def test_duplicate_canonical_vector_id_fails(self):
        text = '{"id":"A"}\n{"id":"A"}\n'
        (self.root / "tests/old.rs").write_text(text)
        self.manifest["collections"][0].update(reader="jsonl", legacy_ids=["A", "A"])
        self.manifest["collections"][0]["source"]["sha256"] = sha(text)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.check()

    def test_parser_does_not_import_legacy_python(self):
        text = 'raise RuntimeError("must never execute")\nclass Cases:\n def test_kept(self): pass\n'
        self.assertEqual(contracts.source_ids("python-tests", text), ["Cases.test_kept"])

    def test_historical_oracle_mismatch_remains_explicit(self):
        finding = load("reference-contracts.json")["known_oracle_mismatch"]
        self.assertNotEqual(finding["historical_source_sha"], finding["current_reference_source_sha"])
        self.assertEqual(finding["disposition"], "PRESERVED_STALE_ATTRIBUTION_NO_CURRENT_PARITY_CREDIT")

    def test_family_identities_are_bound_beyond_the_count(self):
        population = contracts.reference_population()
        families = load("reference-contracts.json")["families"]
        self.assertEqual(set(population["family_ids"]), {"conformance", "configuration", "runtime", "windows",
                         "terminal", "sdk", "wire", "packaging", "web", "poison", "benchmark", "kitty"})
        contracts.family_roster(list(reversed(families)), population)
        for mutation in ("remove", "rename"):
            changed = copy.deepcopy(families)
            index = next(i for i, family in enumerate(changed) if family["id"] == "poison")
            if mutation == "remove": changed.pop(index)
            else: changed[index]["id"] = "invented-family-replacing-poison"
            with self.subTest(mutation=mutation), self.assertRaisesRegex(ValueError, "family roster"):
                contracts.family_roster(changed, population)

    def test_whole_collection_loss_or_rename_cannot_redefine_the_reference(self):
        original = load("legacy-scenarios.json")
        for mutation in ("remove", "rename"):
            changed = copy.deepcopy(original)
            row = next(row for row in changed["collections"] if row["reader"] == "divergences")
            self.assertEqual(len(row["legacy_ids"]), 59)
            if mutation == "remove":
                changed["collections"].remove(row)
                changed["cardinality"] -= 59
            else: row["id"] = "renamed-divergence-collection"
            with self.subTest(mutation=mutation), self.assertRaisesRegex(ValueError, "historical collection"):
                contracts.inventory(ROOT, changed)

    def test_oracle_identity_values_and_disposition_come_from_exact_yaml(self):
        population = contracts.reference_population()
        finding = load("reference-contracts.json")["known_oracle_mismatch"]
        result = contracts.oracle_attribution(ROOT, finding, population)
        expected = {"historical_source_sha": "31d77e29b6c9fbb07d032018da78db3a8a38d979",
                    "historical_binary_sha256": "525149cdac8d41b7e60ad68c4bab0670c8b769c646bab780e2a7b66239ad83a0",
                    "current_reference_source_sha": "e802909de06012a4df6209d55e86487c56223163",
                    "current_reference_binary_sha256": "eb05f981dfc0ed55f29b7dc8e13ed838827ebca2764a9fc559ae817d9cf1acd0"}
        self.assertEqual({key: result[key] for key in expected}, expected)
        for key in (*expected, "disposition"):
            changed = dict(finding)
            changed[key] = "0" * len(changed[key])
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "oracle attribution"):
                contracts.oracle_attribution(ROOT, changed, population)

    def test_expected_population_cannot_be_replaced_by_submitted_bytes(self):
        directory = self.root / "docs/validation"
        directory.mkdir(parents=True)
        (directory / "reference-population.json").write_text('{"family_ids": ["invented"]}')
        with self.assertRaisesRegex(ValueError, "stale reference"):
            contracts.reference_population(self.root)

    def test_yaml_identity_rejects_duplicate_and_wrong_section_fields(self):
        scalar = '  source_sha: "' + "a" * 40 + '"\n'
        self.assertEqual(contracts.yaml_identity("oracle:\n" + scalar, "oracle", "source_sha", 40), "a" * 40)
        for text in ("oracle:\n" + scalar * 2, "oracle:\n  nested:\n  " + scalar, "other:\n" + scalar):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, "oracle YAML"):
                contracts.yaml_identity(text, "oracle", "source_sha", 40)


class ConformanceClassification(unittest.TestCase):
    def setUp(self):
        self.record = {"classification": "parity", "owner": "L6", "invariant": "useful observable behavior",
                       "observable": "captured stdout/stderr/code/state", "routes": ["CLI", "by-id"],
                       "oracle": {"version": "tmux 3.7b", "source_sha": "a" * 40,
                                  "binary_sha256": "b" * 64, "capture_sha256": "c" * 64,
                                  "case_id": "independent-oracle-case", "symbol": "case"}}

    def test_pinned_parity_is_a_valid_reference(self):
        contracts.classification(self.record)

    def test_unpinned_oracle_fails(self):
        self.record["oracle"]["binary_sha256"] = "PATH:tmux"
        with self.assertRaisesRegex(ValueError, "unbound oracle"):
            contracts.classification(self.record)

    def corrective(self):
        self.record.update(classification="corrective-divergence", test="case_product_divergence",
                           differential_canary="faulty variant violates conservation",
                           independent_counterproof={"kind": "data-conservation", "sha256": "d" * 64,
                                                     "independent_of_oracle_case": True,
                                                     "violation": "independent expected bytes disappear"})

    def test_corrective_requires_independent_counterproof(self):
        self.corrective()
        contracts.classification(self.record)
        self.record["independent_counterproof"]["sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "independent proof"):
            contracts.classification(self.record)

    def test_product_preference_is_not_an_oracle_bug(self):
        self.corrective()
        self.record["independent_counterproof"]["kind"] = "product-preference"
        with self.assertRaisesRegex(ValueError, "preference"):
            contracts.classification(self.record)

    def test_corrective_cannot_be_called_matches_tmux(self):
        self.corrective()
        self.record["test"] = "case_matches_tmux"
        with self.assertRaisesRegex(ValueError, "must not claim parity"):
            contracts.classification(self.record)

    def test_extension_does_not_masquerade_as_parity(self):
        self.record["classification"] = "extension"
        del self.record["oracle"]
        contracts.classification(self.record)

    def test_indeterminate_cannot_be_pass(self):
        self.record.update(classification="indeterminate", product_pass=True)
        with self.assertRaisesRegex(ValueError, "not product PASS"):
            contracts.classification(self.record)


def campaign():
    rows = []
    for qualification in ("Q1", "Q2"):
        for os_name in ("linux", "macos"):
            rows.append({"qualification_id": qualification, "cell_id": os_name + "-default-CLI",
                         "scenario_id": "case-alpha", "risk": "state-conservation", "source_sha": "a" * 40,
                         "artifact_sha256": sha(os_name + "artifact"), "command_sha256": sha("literal command"),
                         "inputs_sha256": sha("literal input"), "observable_id": "marker-and-state",
                         "target": os_name + "-native", "native_os": os_name, "features_sha256": sha("default"),
                         "prerequisites_sha256": sha("ready"), "collector_sha256": sha("native collector")})
    plan = {"schema": 1, "source_sha": "a" * 40, "qualification_ids": ["Q1", "Q2"], "expected": rows}
    challenges, receipts = [], []
    for index, row in enumerate(rows):
        challenge = sha("issued challenge " + str(index))
        challenges.append({"challenge_id": challenge, "issued_ns": 100 + index,
                           "plan_sha256": contracts.digest(plan), "row_key": list(contracts.row_key(row))})
        result = {**row, "challenge_id": challenge, "run_id": row["qualification_id"] + "-run",
                  "job_id": "synthetic-job", "attempt": 1, "execution_id": "execution-" + str(index),
                  "started_ns": 1000 + index, "ended_ns": 1100 + index, "stdout_sha256": sha("ok"),
                  "stderr_sha256": sha(""), "status": "PASS", "executed": True, "skip": False, "synthetic": True}
        result["trace"] = {field: result[field] for field in contracts.TRACE_FIELDS}
        result["trace_sha256"] = contracts.digest(result["trace"])
        receipts.append(result)
    return plan, challenges, receipts


class ExecutionAccounting(unittest.TestCase):
    def setUp(self):
        self.plan, self.challenges, self.receipts = campaign()

    def check(self, previous=(), synthetic=True):
        return contracts.execution_contract({"case-alpha", "case-beta"}, self.plan, self.challenges,
                                            self.receipts, previous, synthetic=synthetic)

    def test_distinct_executed_q1_q2_account_for_every_cell(self):
        result = self.check()
        self.assertEqual(result["executions"], 4)
        self.assertFalse(result["native_authenticity_or_product_verdict"])

    def test_synthetic_cannot_become_native(self):
        with self.assertRaisesRegex(ValueError, "synthetic/native"):
            self.check(synthetic=False)

    def test_empty_selection_fails(self):
        self.plan["expected"] = []
        with self.assertRaisesRegex(ValueError, "empty expected"):
            self.check()

    def test_nonattempted_cell_fails(self):
        self.receipts.pop()
        with self.assertRaisesRegex(ValueError, "nonattempted"):
            self.check()

    def test_duplicate_case_fails(self):
        self.receipts[-1] = copy.deepcopy(self.receipts[0])
        with self.assertRaisesRegex(ValueError, "repeated scenario"):
            self.check()

    def test_wrong_cell_fails(self):
        self.receipts[0]["cell_id"] = "windows11-default-CLI"
        with self.assertRaisesRegex(ValueError, "wrong cell"):
            self.check()

    def test_skip_and_false_success_and_failed_status_fail(self):
        for key, value in (("skip", True), ("executed", False), ("status", "SKIP"), ("status", "FAIL")):
            with self.subTest(key=key, value=value):
                self.plan, self.challenges, self.receipts = campaign()
                self.receipts[0][key] = value
                with self.assertRaisesRegex(ValueError, "not PASS"):
                    self.check()

    def test_challenge_must_precede_execution(self):
        self.challenges[0]["issued_ns"] = self.receipts[0]["started_ns"]
        with self.assertRaisesRegex(ValueError, "not prior"):
            self.check()

    def test_challenge_for_another_plan_fails(self):
        self.plan["campaign_note"] = "changed after challenge issuance"
        with self.assertRaisesRegex(ValueError, "another plan"):
            self.check()

    def test_q2_cannot_relabel_q1_trace(self):
        self.receipts[2]["trace"] = copy.deepcopy(self.receipts[0]["trace"])
        self.receipts[2]["trace_sha256"] = self.receipts[0]["trace_sha256"]
        with self.assertRaisesRegex(ValueError, "trace relabelled"):
            self.check()

    def test_prior_execution_cannot_be_resubmitted(self):
        with self.assertRaisesRegex(ValueError, "replay"):
            self.check(previous=[self.receipts[0]])

    def test_unmapped_case_fails(self):
        self.plan["expected"][0]["scenario_id"] = "unmapped"
        with self.assertRaisesRegex(ValueError, "unmapped"):
            self.check()

    def test_q2_requires_identical_content_and_population(self):
        self.plan["expected"][2]["artifact_sha256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "content differs"):
            self.check()
        self.plan, self.challenges, self.receipts = campaign()
        self.plan["expected"].pop()
        with self.assertRaisesRegex(ValueError, "population differs"):
            self.check()

    def test_reordered_qualifications_do_not_hide_missing_q2_cell(self):
        self.plan["qualification_ids"] = ["Q2", "Q1"]
        self.plan["expected"].pop()
        with self.assertRaisesRegex(ValueError, "population differs"):
            self.check()

    def test_q2_cannot_weaken_the_observable(self):
        self.plan["expected"][2]["observable_id"] = "only-exit-code"
        with self.assertRaisesRegex(ValueError, "content differs"):
            self.check()

    def test_q2_cannot_reuse_q1_run_even_with_a_new_trace(self):
        result = self.receipts[2]
        result["run_id"] = self.receipts[0]["run_id"]
        result["trace"]["run_id"] = result["run_id"]
        result["trace_sha256"] = contracts.digest(result["trace"])
        with self.assertRaisesRegex(ValueError, "run reused"):
            self.check()

    def test_duplicate_challenge_cannot_cover_another_cell(self):
        self.challenges[1]["challenge_id"] = self.challenges[0]["challenge_id"]
        with self.assertRaisesRegex(ValueError, "challenge reused"):
            self.check()

    def test_copied_operation_cannot_gain_credit_as_another_scenario(self):
        rows = copy.deepcopy(self.plan["expected"])
        for row in rows:
            row["scenario_id"] = "case-beta"
        self.plan["expected"].extend(rows)
        for ticket in self.challenges:
            ticket["plan_sha256"] = contracts.digest(self.plan)
        for index, row in enumerate(rows):
            result = copy.deepcopy(self.receipts[index])
            result.update(row, challenge_id=sha("alias-ticket" + str(index)), execution_id="alias-" + str(index))
            result["trace"] = {field: result[field] for field in contracts.TRACE_FIELDS}
            result["trace_sha256"] = contracts.digest(result["trace"])
            self.receipts.append(result)
            self.challenges.append({"challenge_id": result["challenge_id"], "issued_ns": 200,
                                    "plan_sha256": contracts.digest(self.plan), "row_key": list(contracts.row_key(row))})
        with self.assertRaisesRegex(ValueError, "proof alias"):
            self.check()

    def test_declared_shared_scenario_cannot_also_be_counted_as_own_row(self):
        self.plan["expected"][0]["shared_scenario_ids"] = ["case-beta"]
        self.plan["expected"][2]["shared_scenario_ids"] = ["case-beta"]
        row = copy.deepcopy(self.plan["expected"][0])
        row.update(scenario_id="case-beta", shared_scenario_ids=[])
        self.plan["expected"].append(row)
        with self.assertRaisesRegex(ValueError, "counted twice"):
            self.check()

    def test_retry_does_not_replace_a_qualification(self):
        self.receipts[0]["attempt"] = 2
        with self.assertRaisesRegex(ValueError, "retry"):
            self.check()

    def test_declared_shared_case_counts_one_execution(self):
        for row, result in zip(self.plan["expected"], self.receipts):
            row["shared_scenario_ids"] = ["case-beta"]
            result["shared_scenario_ids"] = ["case-beta"]
        for ticket in self.challenges:
            ticket["plan_sha256"] = contracts.digest(self.plan)
        self.assertEqual(self.check()["executions"], 4)

    def test_undeclared_sharing_fails(self):
        self.receipts[0]["shared_scenario_ids"] = ["case-beta"]
        with self.assertRaisesRegex(ValueError, "undeclared sharing"):
            self.check()

    def test_output_digest_and_trace_identity_are_bound(self):
        self.receipts[0]["stdout_sha256"] = sha("changed output")
        with self.assertRaisesRegex(ValueError, "trace relabelled"):
            self.check()


class PoisonPolicy(unittest.TestCase):
    def test_known_source_witnesses_remain_open_not_approved(self):
        policy = load("poison-policy.json")
        self.assertEqual(poison.validate_policy(ROOT, policy), 46)
        self.assertEqual(policy["approved_exceptions"], [])
        self.assertEqual(policy["product_policy_conformance"], "OPEN_NOT_ESTABLISHED")

    def test_hidden_recovery_or_clear_poison_is_visible(self):
        text = 'let x = m.lock().unwrap_or_else(|e| e.into_inner());\nm.clear_poison();\n'
        self.assertEqual([x["kind"] for x in poison.witnesses(text)],
                         ["recovery-candidate", "poison-state-operation"])

    def test_new_or_silently_deleted_witness_fails(self):
        policy = load("poison-policy.json")
        policy["observed_sites"].pop()
        with self.assertRaisesRegex(ValueError, "inventory drift"):
            poison.validate_policy(ROOT, policy)

    def test_silent_recovery_cannot_be_policy(self):
        policy = load("poison-policy.json")
        policy["silent_recovery_allowed"] = True
        with self.assertRaisesRegex(ValueError, "silent recovery"):
            poison.validate_policy(ROOT, policy)

    def test_exception_requires_owned_reviewed_invariant_repair(self):
        policy = load("poison-policy.json")
        policy["approved_exceptions"] = [{"path": "some.rs", "reason": "ignore poison"}]
        with self.assertRaisesRegex(ValueError, "unreviewed"):
            poison.validate_policy(ROOT, policy)


def web_sample(seed, failure="token"):
    values = {"type": "challenge", "protocol_version": 1, "capabilities": ["e2ee-token-auth"]}
    for index, (field, size) in enumerate((("server_nonce", 16), ("server_public", 32), ("server_ml_kem_ct", 1088))):
        values[field] = base64.urlsafe_b64encode(bytes([seed + index]) * size).decode().rstrip("=")
    raw = json.dumps(values, separators=(",", ":")).encode()
    return {"failure_class": failure, "token_proof_verified": False, "pin_absent": False,
            "frames": [{"direction": "server-to-peer", "opcode": "text", "payload_hex": raw.hex(), "wire_bytes": len(raw)}],
            "close": {"code": 4000, "reason": "handshake_rejected"}}


class WebReference(unittest.TestCase):
    def setUp(self):
        self.profile = load("web-reference-profile.json")

    def test_profile_parameters_are_frozen_prospectively(self):
        web_profile.validate_profile(self.profile)
        self.assertEqual(self.profile["status"], "PROSPECTIVE_NOT_MEASURED")

    def test_missing_stage_population_or_weakened_floor_fails(self):
        for field in ("failure_classes", "backoff_states", "topologies", "stages"):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.profile)
                changed[field].pop()
                with self.assertRaises(ValueError):
                    web_profile.validate_profile(changed)
        self.profile["statistics"]["minimum_per_class_stratum"] = 10
        with self.assertRaisesRegex(ValueError, "floor"):
            web_profile.validate_profile(self.profile)

    def test_random_bytes_excluded_but_format_lengths_and_order_kept(self):
        seen = set()
        first = web_profile.transcript(self.profile, web_sample(10), seen)
        second = web_profile.transcript(self.profile, web_sample(20, "origin"), seen)
        self.assertEqual(first, second)
        self.assertEqual(len(seen), 6)

    def test_reason_leak_changes_canonical_projection(self):
        first, second = web_sample(10), web_sample(20, "origin")
        for sample, reason in ((first, "token-fail"), (second, "origin-bad")):
            frame = sample["frames"][0]
            fields = json.loads(bytes.fromhex(frame["payload_hex"]))
            fields["internal_reason"] = reason
            raw = json.dumps(fields, separators=(",", ":")).encode()
            frame.update(payload_hex=raw.hex(), wire_bytes=len(raw))
        self.assertNotEqual(web_profile.transcript(self.profile, first, set()),
                            web_profile.transcript(self.profile, second, set()))
        samples = [first, second, web_sample(30, "proof"), web_sample(40, "pin"), web_sample(50, "capability")]
        with self.assertRaisesRegex(ValueError, "deterministic transcript differs"):
            web_profile.transcript_family(self.profile, samples)

    def test_complete_five_class_synthetic_block_and_missing_class_canary(self):
        samples = [web_sample(10 * (i + 1), name) for i, name in enumerate(self.profile["failure_classes"])]
        self.assertTrue(web_profile.transcript_family(self.profile, samples)["canonical_equivalence"])
        with self.assertRaisesRegex(ValueError, "class absent"):
            web_profile.transcript_family(self.profile, samples[:-1])
        with self.assertRaisesRegex(ValueError, "class absent"):
            web_profile.transcript_family(self.profile, samples + [web_sample(60)])

    def test_encrypted_reason_is_not_erased_by_projection(self):
        samples = [web_sample(10 * (i + 1), name) for i, name in enumerate(self.profile["failure_classes"])]
        for sample in samples:
            plaintext = json.dumps({"reason": sample["failure_class"]}).encode()
            sample["frames"].append({"direction": "server-to-peer", "opcode": "binary",
                                     "payload_hex": "00" * 64, "wire_bytes": 64,
                                     "authenticated": True, "plaintext_hex": plaintext.hex()})
        with self.assertRaisesRegex(ValueError, "deterministic transcript differs"):
            web_profile.transcript_family(self.profile, samples)

    def test_reused_nonce_key_or_ciphertext_fails(self):
        for field in ("server_nonce", "server_public", "server_ml_kem_ct"):
            with self.subTest(field=field):
                first, second = web_sample(10), web_sample(20)
                values = json.loads(bytes.fromhex(second["frames"][0]["payload_hex"]))
                values[field] = json.loads(bytes.fromhex(first["frames"][0]["payload_hex"]))[field]
                raw = json.dumps(values, separators=(",", ":")).encode()
                second["frames"][0].update(payload_hex=raw.hex(), wire_bytes=len(raw))
                seen = set()
                web_profile.transcript(self.profile, first, seen)
                with self.assertRaisesRegex(ValueError, "reused"):
                    web_profile.transcript(self.profile, second, seen)

    def test_code_divergence_fails(self):
        sample = web_sample(10)
        sample["close"]["code"] = 4009
        with self.assertRaisesRegex(ValueError, "close code"):
            web_profile.transcript(self.profile, sample, set())

    def test_only_authenticated_missing_pin_has_exception(self):
        sample = web_sample(10, "missing-pin")
        sample.update(token_proof_verified=True, pin_absent=True)
        sample["close"] = {"code": 4008, "reason": "pin_required"}
        web_profile.transcript(self.profile, sample, set())
        sample["token_proof_verified"] = False
        with self.assertRaisesRegex(ValueError, "unauthenticated"):
            web_profile.transcript(self.profile, sample, set())

    def test_synthetic_equal_distribution_satisfies_prospective_bound(self):
        result = web_profile.timing(self.profile, [1000000] * 50000, [1000000] * 50000)
        self.assertLess(result["cdf_upper_bound"], 0.05)

    def test_shifted_synthetic_distribution_fails(self):
        with self.assertRaisesRegex(ValueError, "not demonstrated"):
            web_profile.timing(self.profile, [1000000] * 50000, [1000001] * 50000)

    def test_insufficient_nonfinite_or_negative_samples_fail(self):
        with self.assertRaisesRegex(ValueError, "insufficient"):
            web_profile.timing(self.profile, [1] * 49999, [1] * 50000)
        for value in (float("nan"), float("inf"), -1, True):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "nonfinite"):
                web_profile.cdf_distance([value], [1])

    def test_cdf_observable_has_known_manual_answer(self):
        self.assertEqual(web_profile.cdf_distance([0, 2], [1, 3]), 0.5)

    def test_crypto_distribution_bound_and_bias_canary(self):
        first = {"n": 100000, "ones": [50000] * 9088}
        second = copy.deepcopy(first)
        result = web_profile.random_distribution(self.profile, first, second)
        self.assertFalse(result["cryptographic_security_proven"])
        second["ones"][0] = 80000
        with self.assertRaisesRegex(ValueError, "distribution differs"):
            web_profile.random_distribution(self.profile, first, second)

    def test_random_summary_cannot_omit_bits_or_weaken_error_control(self):
        partial = {"n": 100000, "ones": [50000] * 16}
        with self.assertRaisesRegex(ValueError, "bit population"):
            web_profile.random_distribution(self.profile, partial, partial)
        self.profile["crypto_statistics"]["family_alpha"] = 0.99
        with self.assertRaisesRegex(ValueError, "crypto distribution profile"):
            web_profile.random_distribution(self.profile, partial, partial)

    def test_base64_shape_and_claimed_wire_size_are_checked(self):
        with self.assertRaisesRegex(ValueError, "base64url"):
            web_profile.decode("abcd=", 16)
        sample = web_sample(10)
        sample["frames"][0]["wire_bytes"] += 1
        with self.assertRaisesRegex(ValueError, "length falsified"):
            web_profile.transcript(self.profile, sample, set())


class UnreviewedBoundEdits(unittest.TestCase):
    """A real checkout that carries the frozen base: forged bound files must stay stale."""

    @classmethod
    def setUpClass(cls):
        # A shared clone borrows ROOT's objects (frozen base included) and is its
        # own Git top level, so read_cited consults the real history, not a mock.
        cls.directory = tempfile.TemporaryDirectory()
        cls.fixture = Path(cls.directory.name) / "checkout"
        head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], check=True,
                              capture_output=True, text=True).stdout.strip()
        for args in (["clone", "-q", "--shared", "--no-checkout", str(ROOT), str(cls.fixture)],
                     ["-C", str(cls.fixture), "checkout", "-q", "--detach", head]):
            subprocess.run(["git", *args], check=True, capture_output=True)
        cls.original = {path: (cls.fixture / path).read_bytes()
                        for path in (reference_applicability.DIVERGENCE_BINDING["path"],
                                     reference_applicability.PACKAGING_BINDING["path"])}

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def setUp(self):
        for path, raw in self.original.items():
            (self.fixture / path).write_bytes(raw)

    def forge(self, path, edit):
        target = self.fixture / path
        forged = edit(target.read_bytes())
        self.assertNotEqual(forged, self.original[path])
        target.write_bytes(forged)

    def forge_divergence(self):
        self.forge(reference_applicability.DIVERGENCE_BINDING["path"], lambda raw: raw + (
            b'\n[[entry]]\nid = "C-D94"\nsummary = "Forged without review"\ninventory = "bounded"\n'
            b'evidence = []\nlimitations = []\n'))

    def forge_layout_key(self):
        self.forge(reference_applicability.PACKAGING_BINDING["path"],
                   lambda raw: raw.replace(b'"schema": 1,', b'"schema": 1,\n  "forged": true,', 1))

    def test_reviewed_bound_files_pass_in_the_fixture(self):
        self.assertEqual(contracts.validate(self.fixture)["status"], "PASS_REFERENCE_CONTRACT_ONLY")

    def test_forged_divergence_is_stale_not_frozen(self):
        self.forge_divergence()
        with self.assertRaisesRegex(ValueError, "^stale reference: tests/reference/tmux_compat/divergences.toml$"):
            contracts.validate(self.fixture)

    def test_forged_layout_key_is_stale_not_frozen(self):
        self.forge_layout_key()
        with self.assertRaisesRegex(ValueError, "^stale reference: scripts/packaging/rmux-package-layout.json$"):
            contracts.validate(self.fixture)

    def test_forged_divergence_gains_no_legacy_inventory(self):
        self.forge_divergence()
        contract = load("reference-contracts.json")
        manifest = load("legacy-scenarios.json")
        path = reference_applicability.DIVERGENCE_BINDING["path"]
        row = next(r for r in manifest["collections"] if r["source"]["path"] == path)
        manifest.update(collections=[row], cardinality=len(row["legacy_ids"]))
        expected = [r for r in contracts.reference_population()["collections"] if r["source"]["path"] == path]
        with self.assertRaisesRegex(ValueError, "^stale reference: " + path + "$"):
            contracts.inventory(self.fixture, manifest, expected, contract=contract)


def frozen_history_missing():
    """Name the environment limit instead of cascading stale-reference failures."""
    base = load("reference-contracts.json")["source_base"]
    probe = subprocess.run(["git", "-C", str(ROOT), "cat-file", "-e", base + "^{commit}"],
                           capture_output=True, check=False)
    if probe.returncode != 0:
        return "frozen source base " + base + " is not in this repository's objects"
    try:
        contracts.read_reference(ROOT, reference_applicability.POLICY)
    except (ValueError, OSError) as error:
        return "archived applicability policy unreadable: " + str(error)
    return None


if __name__ == "__main__":
    missing = frozen_history_missing()
    if missing:
        print("ENVIRONMENT: " + missing + "; this checkout cannot verify the phase-0 contract",
              file=sys.stderr)
        raise SystemExit(2)
    unittest.main()
