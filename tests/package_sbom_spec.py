#!/usr/bin/env python3
"""Native Linux tests for the frozen tar SBOM; never write historical inputs."""
import argparse
import copy
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "packaging"))
import sbom


class SbomContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.expected, cls.schema = sbom.reconstruct(ARGS.descriptor, ARGS.inputs,
                                                   ARGS.inputs_sha256, ARGS.subject)
        cls.manifest, cls.inputs = sbom.read_inputs(ARGS.inputs, ARGS.inputs_sha256)

    def refused(self, edit, message=None):
        value = copy.deepcopy(self.expected)
        edit(value)
        # Recompute the digest so each mutation reaches semantic validation.
        value["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(value)))
        with self.assertRaises(sbom.Error) as caught:
            sbom.validate_document(value, self.expected, self.schema)
        if message:
            self.assertIn(message, str(caught.exception))

    def test_positive_matches_the_delivered_sidecar(self):
        value = sbom.loads(Path(ARGS.sbom).read_bytes())
        sbom.validate_document(value, self.expected, self.schema)
        self.assertEqual(Path(ARGS.sbom).read_bytes(), sbom.canonical(value))
        self.assertEqual((len(value["files"]), len(value["binary_roots"]), len(value["packages"]),
                          len(value["build_units"])), (16, 3, 145, 198))
        self.assertTrue(all(r["reachable_units"] == 194 for r in value["binary_roots"]))

    def test_dependency_oracle_from_raw_numbers(self):
        # This oracle independently matches the historical integer references.
        aux = self.inputs["auxiliary"]["observations"]
        native_by_number = {int.from_bytes(bytes.fromhex(a["fingerprint"]["value"]), "little"):
                            a["fingerprint"]["value"] for a in aux}
        fp_by_path = {a["record"]["path"]: a["fingerprint"]["value"] for a in aux}
        oracle = {(fp_by_path[r["file"]["path"]], native_by_number[d[3]], d[1])
                  for r in self.inputs["units"]["fingerprint_records"] for d in r["record"]["deps"]}
        actual_units = {u["id"]: u["fingerprint"] for u in self.expected["build_units"]}
        actual = {(actual_units[r["source"]], actual_units[r["target"]], r["name"])
                  for r in self.expected["relationships"] if r["kind"] == "depends_on"}
        self.assertEqual(len(oracle), 379)
        self.assertEqual(actual, oracle)

    def test_build_script_features_and_target_context_from_raw_evidence(self):
        records = self.inputs["units"]["fingerprint_records"]
        observations = self.inputs["auxiliary"]["observations"]
        by_path = {r["file"]["path"]: r["record"] for r in records}
        by_number = {int.from_bytes(bytes.fromhex(a["fingerprint"]["value"]), "little"):
                     by_path[a["record"]["path"]] for a in observations}
        unit_by_sha = {u["historical_record_sha256"]: u for u in self.expected["build_units"]}
        empty = [r for r in records if r["record"]["features"] == ""]
        self.assertEqual(len(empty), 25)
        for item in empty:
            record = item["record"]
            dependencies = [d for d in record["deps"] if d[1] == "build_script_build"]
            self.assertEqual(len(dependencies), 1)
            features = json.loads(by_number[dependencies[0][3]]["features"])
            unit = unit_by_sha[item["file"]["sha256"]]
            self.assertEqual(unit["features"], sorted(features))
            expected_context = ("target" if "/target/x86_64-unknown-linux-gnu/" in item["file"]["path"]
                                else "host")
            self.assertEqual(unit["context"], expected_context)
        self.assertTrue(any(unit_by_sha[r["file"]["sha256"]]["features"] for r in empty))

    def test_actual_versions_not_workspace_union(self):
        actual = {(p["name"], p["version"]) for p in self.expected["packages"]}
        self.assertTrue({("getrandom", "0.2.17"), ("getrandom", "0.3.4"),
                         ("hashbrown", "0.15.5"), ("hashbrown", "0.17.1")} <= actual)
        for excluded in [("chacha20", "0.10.0"), ("cpufeatures", "0.3.0"),
                         ("getrandom", "0.4.3"), ("log", "0.4.32")]:
            self.assertNotIn(excluded, actual)

    def test_wrong_subject_digest_and_size(self):
        self.refused(lambda d: d["subject"].update(sha256="0" * 64))
        self.refused(lambda d: d["subject"].update(bytes=d["subject"]["bytes"] + 1))

    def test_wrong_historical_source(self):
        self.refused(lambda d: d["payload_source"].update(commit="501b610191f9a4c8a4643b0cf23f8c061201f2b4"))

    def test_missing_tiny_feature(self):
        def edit(d):
            next(c for c in d["resolution_contexts"] if c["role"] == "cli")["features"].remove("tiny-cli")
        self.refused(edit)

    def test_missing_extra_or_dangling_relation(self):
        self.refused(lambda d: d["relationships"].pop(next(i for i, r in enumerate(d["relationships"])
                                                       if r["kind"] == "depends_on")))
        self.refused(lambda d: d["relationships"].append(copy.deepcopy(d["relationships"][0])))
        self.refused(lambda d: d["relationships"][0].update(target="missing-unit"))

    def test_missing_unit_or_package(self):
        self.refused(lambda d: d["build_units"].pop())
        self.refused(lambda d: d["packages"].pop())

    def test_extra_conservative_package(self):
        def edit(d):
            p = copy.deepcopy(d["packages"][0])
            p.update(id="extra", name="getrandom", version="0.4.3")
            d["packages"].append(p)
        self.refused(edit)

    def test_host_target_and_multiversion_collapse(self):
        self.refused(lambda d: next(u for u in d["build_units"] if u["context"] == "host").update(context="target"))
        self.refused(lambda d: next(p for p in d["packages"] if p["name"] == "getrandom"
                                   and p["version"] == "0.2.17").update(version="0.3.4"))

    def test_lock_and_tool_pins(self):
        self.refused(lambda d: d["resolution_contexts"][0].update(lock_sha256="0" * 64))
        self.refused(lambda d: d["tool_pins"].update(generator="0" * 64))
        with self.assertRaisesRegex(sbom.Error, "input manifest digest mismatch"):
            sbom.read_inputs(ARGS.inputs, "0" * 64)

    def test_schema_is_closed_and_versioned(self):
        self.refused(lambda d: d.update(extra="not admitted"), "schema fields")
        self.refused(lambda d: d.update(version=2), "schema enum")
        self.refused(lambda d: d["build_units"][0].update(profile=True), "schema type")
        self.refused(lambda d: d.update(profile="unqualified"), "schema enum")
        with self.assertRaisesRegex(sbom.Error, "duplicate JSON key"):
            sbom.loads('{"version":1,"version":2}')

    def test_reuse_requires_equal_complete_closure(self):
        self.assertTrue(sbom.same_closure(self.expected, copy.deepcopy(self.expected)))
        for field in ("features", "target"):
            changed = copy.deepcopy(self.expected)
            changed["resolution_contexts"][0][field] = [] if field == "features" else "aarch64-unknown-linux-gnu"
            changed["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(changed)))
            self.assertFalse(sbom.same_closure(self.expected, changed))
        changed = copy.deepcopy(self.expected)
        changed["subject"]["sha256"] = "0" * 64
        self.assertTrue(sbom.same_closure(self.expected, changed))
        with self.assertRaises(sbom.Error):
            sbom.validate_document(changed, self.expected, self.schema)

    def test_changed_raw_input_is_refused_before_reconstruction(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["references"]["lock"]["sha256"] = "0" * 64
        content = sbom.canonical(manifest)
        with tempfile.NamedTemporaryFile(prefix="negative-lock-", suffix=".json",
                                         dir=Path(ARGS.inputs).parent, delete=False) as output:
            output.write(content)
            path = Path(output.name)
        with self.assertRaisesRegex(sbom.Error, "input pin mismatch"):
            sbom.read_inputs(path, hashlib.sha256(content).hexdigest())

    def test_cli_refuses_wrong_manifest_pin(self):
        command = [sys.executable, "-B", str(ROOT / "scripts/packaging/sbom.py"), "validate",
                   "--descriptor", ARGS.descriptor, "--inputs", ARGS.inputs,
                   "--inputs-sha256", "0" * 64, "--subject", ARGS.subject, "--sbom", ARGS.sbom]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("input manifest digest mismatch", result.stderr)


class RpmContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not ARGS.rpm_inputs:
            raise unittest.SkipTest("RPM inputs not supplied")
        cls.expected, cls.schema = sbom.reconstruct_rpm(ARGS.descriptor, ARGS.rpm_inputs,
                                                       ARGS.rpm_inputs_sha256, ARGS.rpm_subject)
        cls.proofs = Path(ARGS.rpm_proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def command(self, directory, *, subject=None, inputs=None, inputs_sha256=None, document=None):
        argv = [sys.executable, "-B", str(ROOT / "scripts/packaging/sbom.py"),
                "validate" if document else "generate", "--profile", sbom.RPM_PROFILE,
                "--descriptor", ARGS.descriptor, "--inputs", str(inputs or ARGS.rpm_inputs),
                "--inputs-sha256", inputs_sha256 or ARGS.rpm_inputs_sha256,
                "--subject", str(subject or ARGS.rpm_subject)]
        output = directory / "must-not-be-generated.json"
        argv += ["--sbom", str(document)] if document else ["--output", str(output)]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=180)
        (directory / "stdout.txt").write_text(result.stdout)
        (directory / "stderr.txt").write_text(result.stderr)
        (directory / "receipt.json").write_text(json.dumps(dict(argv=argv, expected_rc=2,
                                                               rc=result.returncode), indent=2) + "\n")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(output.exists())
        return result.stderr

    def refused(self, name, edit):
        directory = Path(tempfile.mkdtemp(prefix=name + "-", dir=self.proofs))
        document = copy.deepcopy(self.expected)
        edit(document)
        document["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(document)))
        path = directory / "mutated-sbom.json"
        path.write_bytes(sbom.canonical(document))
        self.command(directory, document=path)

    def test_rpm_positive_and_exact_cargo_reuse(self):
        document = sbom.loads(Path(ARGS.rpm_sbom).read_bytes())
        sbom.validate_document(document, self.expected, self.schema)
        self.assertEqual((len(document["files"]), len(document["packages"]),
                          len(document["build_units"])), (13, 145, 198))
        self.assertEqual(sum(r["kind"] == "depends_on" for r in document["relationships"]), 379)
        manifest = sbom.loads(Path(ARGS.rpm_inputs).read_bytes())
        parent = sbom.loads((Path(ARGS.rpm_inputs).parent / manifest["references"]["tar_sbom"]["path"]).read_bytes())
        self.assertEqual(sbom.cargo_projection(document), sbom.cargo_projection(parent))
        self.assertFalse(sbom.same_closure(document, parent))
        self.assertEqual(document["payload_source"]["commit"], "b592010d0f41f8a6deba30a526c84a668ff6c6e4")

    def test_rpm_physical_archive_byte_changed(self):
        directory = Path(tempfile.mkdtemp(prefix="archive-", dir=self.proofs))
        content = bytearray(Path(ARGS.rpm_subject).read_bytes())
        content[-10] ^= 1
        path = directory / Path(ARGS.rpm_subject).name
        path.write_bytes(content)
        self.assertIn("input pin mismatch", self.command(directory, subject=path))

    def test_rpm_binary_digest_changed(self):
        def edit(d):
            root = d["binary_roots"][0]
            root["sha256"] = "0" * 64
            next(f for f in d["files"] if f["path"] == root["path"])["sha256"] = "0" * 64
        self.refused("binary", edit)

    def test_rpm_unexpected_file(self):
        def edit(d):
            item = dict(path="usr/share/rmux/unexpected", bytes=0, mode=420,
                        sha256=hashlib.sha256(b"").hexdigest())
            d["files"].append(item)
            d["relationships"].append(dict(kind="contains", source="subject", target="file:" + item["path"], name=""))
        self.refused("extra-file", edit)

    def test_rpm_missing_file(self):
        def edit(d):
            item = d["files"].pop()
            d["relationships"] = [r for r in d["relationships"] if r["target"] != "file:" + item["path"]]
        self.refused("missing-file", edit)

    def test_rpm_unapproved_parent_closure(self):
        directory = Path(tempfile.mkdtemp(prefix="unproven-", dir=self.proofs))
        base = Path(ARGS.rpm_inputs).parent
        manifest = sbom.loads(Path(ARGS.rpm_inputs).read_bytes())
        ref = manifest["references"]["tar_sbom"]
        parent = sbom.loads((base / ref["path"]).read_bytes())
        parent["packages"].pop()
        parent["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(parent)))
        with tempfile.NamedTemporaryFile(prefix="unproven-parent-", suffix=".json", dir=base, delete=False) as stream:
            content = sbom.canonical(parent)
            stream.write(content)
            path = Path(stream.name)
        ref.update(path=path.name, bytes=len(content), sha256=sbom.digest(content))
        with tempfile.NamedTemporaryFile(prefix="unproven-inputs-", suffix=".json", dir=base, delete=False) as stream:
            content = sbom.canonical(manifest)
            stream.write(content)
            path = Path(stream.name)
        diagnostic = self.command(directory, inputs=path, inputs_sha256=sbom.digest(content))
        self.assertIn("UNPROVEN parent acceptance", diagnostic)

    def test_rpm_dependency_relation_removed(self):
        self.refused("missing-edge", lambda d: d["relationships"].pop(
            next(i for i, r in enumerate(d["relationships"]) if r["kind"] == "depends_on")))

    def test_rpm_features_changed(self):
        def edit(d):
            next(c for c in d["resolution_contexts"] if c["role"] == "cli")["features"].remove("tiny-cli")
        self.refused("features", edit)

    def test_rpm_main_header_binding_changed(self):
        self.refused("header", lambda d: next(s for s in d["rpm_binding"]["sections"]
                     if s["name"] == "main_header").update(sha256="0" * 64))


class DebContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not ARGS.deb_inputs:
            raise unittest.SkipTest("Debian inputs not supplied")
        cls.expected, cls.schema = sbom.reconstruct_deb(ARGS.descriptor, ARGS.deb_inputs,
                                                       ARGS.deb_inputs_sha256, ARGS.deb_subject)
        cls.proofs = Path(ARGS.deb_proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def command(self, directory, *, subject=None, inputs=None, inputs_sha256=None, document=None):
        argv = [sys.executable, "-B", str(ROOT / "scripts/packaging/sbom.py"),
                "validate" if document else "generate", "--profile", sbom.DEB_PROFILE,
                "--descriptor", ARGS.descriptor, "--inputs", str(inputs or ARGS.deb_inputs),
                "--inputs-sha256", inputs_sha256 or ARGS.deb_inputs_sha256,
                "--subject", str(subject or ARGS.deb_subject)]
        output = directory / "must-not-be-generated.json"
        argv += ["--sbom", str(document)] if document else ["--output", str(output)]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=180)
        (directory / "stdout.txt").write_text(result.stdout)
        (directory / "stderr.txt").write_text(result.stderr)
        (directory / "receipt.json").write_text(json.dumps(dict(argv=argv, expected_rc=2,
                                                               rc=result.returncode), indent=2) + "\n")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(output.exists())
        return result.stderr

    def refused(self, name, edit):
        directory = Path(tempfile.mkdtemp(prefix=name + "-", dir=self.proofs))
        document = copy.deepcopy(self.expected)
        edit(document)
        document["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(document)))
        path = directory / "mutated-sbom.json"
        path.write_bytes(sbom.canonical(document))
        self.command(directory, document=path)

    def test_deb_positive_and_exact_cargo_reuse(self):
        document = sbom.loads(Path(ARGS.deb_sbom).read_bytes())
        sbom.validate_document(document, self.expected, self.schema)
        self.assertEqual((len(document["files"]), len(document["packages"]),
                          len(document["build_units"])), (14, 145, 198))
        self.assertEqual(sum(r["kind"] == "depends_on" for r in document["relationships"]), 379)
        manifest = sbom.loads(Path(ARGS.deb_inputs).read_bytes())
        parent = sbom.loads((Path(ARGS.deb_inputs).parent / manifest["references"]["tar_sbom"]["path"]).read_bytes())
        self.assertEqual(sbom.cargo_projection(document), sbom.cargo_projection(parent))
        self.assertFalse(sbom.same_closure(document, parent))
        self.assertEqual(document["payload_source"]["commit"], "b592010d0f41f8a6deba30a526c84a668ff6c6e4")

    def test_deb_physical_ar_header_changed(self):
        directory = Path(tempfile.mkdtemp(prefix="archive-", dir=self.proofs))
        content = bytearray(Path(ARGS.deb_subject).read_bytes())
        content[0] ^= 1
        path = directory / Path(ARGS.deb_subject).name
        path.write_bytes(content)
        self.assertIn("input pin mismatch", self.command(directory, subject=path))

    def test_deb_binary_digest_changed(self):
        def edit(d):
            root = d["binary_roots"][0]
            root["sha256"] = "0" * 64
            next(f for f in d["files"] if f["path"] == root["path"])["sha256"] = "0" * 64
        self.refused("binary", edit)

    def test_deb_unexpected_file(self):
        def edit(d):
            item = dict(path="usr/share/rmux/unexpected", bytes=0, mode=420,
                        sha256=hashlib.sha256(b"").hexdigest())
            d["files"].append(item)
            d["relationships"].append(dict(kind="contains", source="subject", target="file:" + item["path"], name=""))
        self.refused("extra-file", edit)

    def test_deb_missing_file(self):
        def edit(d):
            item = d["files"].pop()
            d["relationships"] = [r for r in d["relationships"] if r["target"] != "file:" + item["path"]]
        self.refused("missing-file", edit)

    def test_deb_unapproved_parent_closure(self):
        directory = Path(tempfile.mkdtemp(prefix="unproven-", dir=self.proofs))
        base = Path(ARGS.deb_inputs).parent
        manifest = sbom.loads(Path(ARGS.deb_inputs).read_bytes())
        ref = manifest["references"]["tar_sbom"]
        parent = sbom.loads((base / ref["path"]).read_bytes())
        parent["packages"].pop()
        parent["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(parent)))
        with tempfile.NamedTemporaryFile(prefix="unproven-parent-", suffix=".json", dir=base, delete=False) as stream:
            content = sbom.canonical(parent)
            stream.write(content)
            path = Path(stream.name)
        ref.update(path=path.name, bytes=len(content), sha256=sbom.digest(content))
        with tempfile.NamedTemporaryFile(prefix="unproven-inputs-", suffix=".json", dir=base, delete=False) as stream:
            content = sbom.canonical(manifest)
            stream.write(content)
            path = Path(stream.name)
        diagnostic = self.command(directory, inputs=path, inputs_sha256=sbom.digest(content))
        self.assertIn("UNPROVEN parent acceptance", diagnostic)


    def physical_member(self, name):
        directory = Path(tempfile.mkdtemp(prefix=name + "-", dir=self.proofs))
        content = bytearray(Path(ARGS.deb_subject).read_bytes())
        _, members = sbom.deb_ar(content)
        member = next(m for m in members if m["name"] == name + ".tar.zst")
        content[member["offset"]] ^= 1
        path = directory / Path(ARGS.deb_subject).name
        path.write_bytes(content)
        self.assertIn("input pin mismatch", self.command(directory, subject=path))

    def test_deb_physical_control_member_changed(self):
        self.physical_member("control")

    def test_deb_physical_data_member_changed(self):
        self.physical_member("data")

    def test_deb_control_identity_changed(self):
        def edit(d):
            d["deb_binding"]["identity"]["package"] = "different-package"
            next(f for f in d["deb_binding"]["control_fields"] if f["field"] == "Package")["value"] = "different-package"
        self.refused("control-identity", edit)



class ReproContract(unittest.TestCase):
    """Counterexamples at the reader/closure boundary and the public CLI."""
    @classmethod
    def setUpClass(cls):
        if not ARGS.repro_inputs_dir:
            raise unittest.SkipTest("reproduction inputs not supplied")
        cls.cases = {}
        cls.proofs = Path(ARGS.repro_proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)
        for kind in ("tar", "deb", "rpm"):
            path = Path(ARGS.repro_inputs_dir) / (kind + ".json")
            manifest = sbom.loads(path.read_bytes())
            expected, schema = sbom.reconstruct_repro(ARGS.descriptor, path, sbom.file_hash(path),
                                                      manifest["archives"][0]["path"])
            cls.cases[kind] = (manifest, expected, schema)

    def refused_document(self, name, edit):
        for kind, (_, expected, schema) in self.cases.items():
            with self.subTest(kind=kind, mutation=name):
                value = copy.deepcopy(expected)
                edit(value)
                value["closure_sha256"] = sbom.digest(sbom.canonical(sbom.closure_fields(value)))
                (self.proofs / (kind + "-" + name + ".json")).write_bytes(sbom.canonical(value))
                with self.assertRaises(sbom.Error):
                    sbom.validate_document(value, expected, schema)

    def refused_manifest(self, kind, name, edit, diagnostic):
        manifest = copy.deepcopy(self.cases[kind][0])
        edit(manifest)
        path = self.proofs / (kind + "-" + name + "-inputs.json")
        path.write_bytes(sbom.canonical(manifest))
        output = self.proofs / (kind + "-" + name + "-must-not-exist.json")
        argv = [sys.executable, "-B", str(ROOT / "scripts/packaging/sbom.py"), "generate",
                "--profile", manifest["profile"], "--descriptor", ARGS.descriptor,
                "--inputs", str(path), "--inputs-sha256", sbom.file_hash(path),
                "--subject", self.cases[kind][0]["archives"][0]["path"], "--output", str(output)]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=30)
        (self.proofs / (kind + "-" + name + "-receipt.json")).write_text(json.dumps(
            dict(argv=argv, rc=result.returncode, expected_rc=2, stdout=result.stdout,
                 stderr=result.stderr, expected_diagnostic=diagnostic), indent=2) + "\n")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        self.assertFalse(output.exists())

    def test_repro_positive_complete_closure_and_unsigned_scope(self):
        for kind, (manifest, expected, schema) in self.cases.items():
            with self.subTest(kind=kind):
                document = sbom.loads((Path(ARGS.repro_output_dir) / (kind + ".sbom.json")).read_bytes())
                sbom.validate_document(document, expected, schema)
                parent = sbom.loads(Path(manifest["parent_sbom"]["path"]).read_bytes())
                self.assertEqual(sbom.cargo_projection(document), sbom.cargo_projection(parent))
                self.assertEqual((len(document["packages"]), len(document["build_units"])), (145, 198))
                self.assertEqual(document["files"], parent["files"])
                self.assertEqual(document["repro_binding"]["signature_status"], "unsigned")
                self.assertEqual(document["repro_binding"]["archives"], manifest["archives"])
                self.assertNotIn("rpm_signed", {r["name"] for r in document["recipe_evidence"]})
                self.assertNotEqual(document["subject"]["sha256"], parent["subject"]["sha256"])
        self.assertEqual(self.cases["rpm"][1]["repro_binding"]["review_scope"],
                         "author-proof-review-pending-at-mandate")

    def test_repro_subject_sha_and_size(self):
        self.refused_document("subject-sha", lambda d: d["subject"].update(sha256="0" * 64))
        self.refused_document("subject-size", lambda d: d["subject"].update(bytes=d["subject"]["bytes"] + 1))
        for kind in self.cases:
            self.refused_manifest(kind, "subject-size", lambda m: m["subject"].update(bytes=1),
                                  "reproduction subject differs")

    def test_repro_reader_file_set_and_binary_identity(self):
        descriptor = sbom.layout.load(ARGS.descriptor)
        for kind, (_, parent, _) in self.cases.items():
            variant = sbom.REPRO_PROFILES[parent["profile"]][1]
            original = {f["path"]: f for f in parent["files"]}
            for mutation in ("binary", "missing", "extra"):
                with self.subTest(kind=kind, mutation=mutation):
                    files = copy.deepcopy(original)
                    if mutation == "binary":
                        files[parent["binary_roots"][0]["path"]]["sha256"] = "0" * 64
                    elif mutation == "missing":
                        files.pop(next(iter(files)))
                    else:
                        files["unexpected"] = dict(path="unexpected", mode=420, bytes=0, sha256="0" * 64)
                    (self.proofs / (kind + "-reader-" + mutation + ".json")).write_bytes(sbom.canonical(files))
                    with self.assertRaisesRegex(sbom.Error, "reproduction (payload|file set) differs"):
                        sbom.repro_file_closure(files, parent, descriptor, variant)

    def test_repro_closure_target_features_source(self):
        self.refused_document("target", lambda d: d["resolution_contexts"][0].update(target="wrong-target"))
        self.refused_document("features", lambda d: d["resolution_contexts"][0].update(features=[]))
        self.refused_document("source", lambda d: d["payload_source"].update(commit="0" * 40))

    def test_repro_foreign_packaging_receipt(self):
        for kind in self.cases:
            other = "deb" if kind != "deb" else "rpm"
            self.refused_manifest(kind, "foreign-receipt", lambda m: m.update(
                packaging_receipt=self.cases[other][0]["packaging_receipt"]), "foreign packaging receipt")

    def test_repro_foreign_ab_binding(self):
        for kind in self.cases:
            self.refused_manifest(kind, "foreign-ab", lambda m: m["archives"][1].update(
                path=m["archives"][0]["path"]), "foreign A/B archive binding")

    def test_repro_false_signature_inheritance(self):
        self.refused_document("signed", lambda d: d["repro_binding"].update(signature_status="signed"))
        self.refused_document("signed-evidence", lambda d: d["recipe_evidence"].append(
            dict(name="rpm_signed", bytes=1, sha256="0" * 64)))

    def test_repro_packager_source_and_tools_binding(self):
        self.refused_document("packager-source", lambda d: d["repro_binding"]["packager_source"].update(commit="0" * 40))
        self.refused_document("assembly-tools", lambda d: d["repro_binding"]["historical_tools"][0].update(sha256="0" * 64))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", required=True)
    parser.add_argument("--inputs-sha256", required=True)
    parser.add_argument("--subject", required=True)
    parser.add_argument("--descriptor", default=str(ROOT / "scripts/packaging/rmux-package-layout.json"))
    parser.add_argument("--sbom", required=True)
    parser.add_argument("--rpm-inputs")
    parser.add_argument("--rpm-inputs-sha256")
    parser.add_argument("--rpm-subject")
    parser.add_argument("--rpm-sbom")
    parser.add_argument("--rpm-proofs", default=tempfile.gettempdir())
    parser.add_argument("--deb-inputs")
    parser.add_argument("--deb-inputs-sha256")
    parser.add_argument("--deb-subject")
    parser.add_argument("--deb-sbom")
    parser.add_argument("--deb-proofs", default=tempfile.gettempdir())
    parser.add_argument("--repro-inputs-dir")
    parser.add_argument("--repro-output-dir")
    parser.add_argument("--repro-proofs", default=tempfile.gettempdir())
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[sys.argv[0], *remaining], verbosity=2)
