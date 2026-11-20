"""Deterministic local release neighbors; no release-signature proof is claimed."""
import copy
import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/release"))
import release_publish_security as security
import release_sbom

spec = importlib.util.spec_from_file_location("verify_release", ROOT / "scripts/release/verify-release.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class AttestationNeighbors(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.subject = self.root / "output.tar.gz"
        self.subject.write_bytes(b"release bytes")
        self.bundle = self.root / "bundle.json"
        self.bundle.write_text("{}")
        self.gh = self.root / "gh"
        self.gh.write_text("unit test double, never executed")
        self.predicate = {"assets": [{"name": self.subject.name, "role": "archive",
            "size": self.subject.stat().st_size, "sha256": security.file_hash(self.subject)}]}
        self.statement = {"subject": [{"name": self.subject.name, "digest": {
            "sha256": security.file_hash(self.subject)}}], "predicate": self.predicate,
            "predicateType": security.PREDICATE_TYPE}

    def invoke(self, result):
        with patch.object(security.subprocess, "run", return_value=result) as run:
            security.verify_promotion_attestation(
                gh=self.gh, assets={self.subject.name: self.subject}, bundle=self.bundle,
                predicate=self.predicate, source_git_sha="1" * 40, release_ref="v0.11.0-rc.1")
            return run.call_args.args[0]

    def result(self, statement=None, rc=0):
        return subprocess.CompletedProcess([], rc, json.dumps([{
            "verificationResult": {"statement": statement or self.statement}}]).encode(), b"")

    def test_exact_bytes_and_statement_retain_signer_constraints(self):
        argv = self.invoke(self.result())
        for flag in ("--signer-workflow", "--signer-digest", "--source-digest",
                     "--source-ref", "--predicate-type", "--deny-self-hosted-runners"):
            self.assertIn(flag, argv)

    def test_signature_failure_and_altered_statement_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "verification failed closed"):
            self.invoke(self.result(rc=1))
        altered = copy.deepcopy(self.statement)
        altered["subject"][0]["digest"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "statement changed"):
            self.invoke(self.result(altered))

    def test_payload_changed_during_verification_is_rejected(self):
        def alter(*args, **kwargs):
            self.subject.write_bytes(b"changed bytes")
            return self.result()
        with patch.object(security.subprocess, "run", side_effect=alter):
            with self.assertRaisesRegex(ValueError, "asset bytes differ"):
                security.verify_promotion_attestation(
                    gh=self.gh, assets={self.subject.name: self.subject}, bundle=self.bundle,
                    predicate=self.predicate, source_git_sha="1" * 40, release_ref="v0.11.0-rc.1")

    def test_duplicate_subject_and_nonobject_verification_are_rejected(self):
        self.predicate["assets"].append(copy.deepcopy(self.predicate["assets"][0]))
        with self.assertRaisesRegex(ValueError, "duplicated"):
            self.invoke(self.result())
        self.predicate["assets"].pop()
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.invoke(subprocess.CompletedProcess([], 0, b"[null]", b""))

    def test_local_asset_set_hash_and_symlink_are_checked(self):
        records = self.predicate["assets"]
        assets = self.root / "assets"
        assets.mkdir()
        target = assets / self.subject.name
        target.write_bytes(self.subject.read_bytes())
        gate.verify_files(assets, records)
        target.write_bytes(b"altered bytes")
        with self.assertRaisesRegex(ValueError, "asset bytes differ"):
            gate.verify_files(assets, records)
        target.unlink()
        target.symlink_to(self.subject)
        with self.assertRaisesRegex(ValueError, "regular file"):
            gate.verify_files(assets, records)
        target.unlink()
        target.write_bytes(self.subject.read_bytes())
        (assets / "extra").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "names differ"):
            gate.verify_files(assets, records)


class SbomResolutionNeighbors(unittest.TestCase):
    def test_unsupported_or_foreign_sbom_cannot_reach_reconstruction(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "output.sbom.json"
            record = {"name": "output.tar.gz", "role": "archive", "size": 1,
                      "sha256": "3" * 64, "platform_key": "linux-x86_64"}
            mapping = {record["name"]: path}
            def invoke():
                release_sbom.verify_sboms([record], root, mapping, mapping, mapping,
                                         root, "1" * 40, "2" * 40, Path(sys.executable))
            path.write_bytes(release_sbom.sbom.canonical({"profile": "unproven"}))
            with self.assertRaisesRegex(ValueError, "unsupported SBOM"):
                invoke()
            document = {"profile": release_sbom.sbom.PROFILE, "subject": {
                "name": record["name"], "bytes": 1, "sha256": "0" * 64}}
            path.write_bytes(release_sbom.sbom.canonical(document))
            with self.assertRaisesRegex(ValueError, "subject differs"):
                invoke()
            document["subject"]["sha256"] = record["sha256"]
            document["payload_source"] = {"commit": "0" * 40, "tree": "2" * 40}
            path.write_bytes(release_sbom.sbom.canonical(document))
            with self.assertRaisesRegex(ValueError, "source differs"):
                invoke()

    def test_every_output_needs_one_sbom_without_metadata_exemptions(self):
        record = {"name": "output.tar.gz", "role": "archive"}
        with self.assertRaisesRegex(ValueError, "every package output"):
            release_sbom.verify_sboms([record], ROOT, {}, {}, {}, ROOT, "1" * 40,
                                     "2" * 40, Path(sys.executable))
        for values in (["output=a", "output=b"], ["../output=a"], ["output="]):
            with self.assertRaises(ValueError):
                gate.mapped_paths(values)

    def test_coherent_inputs_cannot_omit_serde_core_from_real_cargo_closure(self):
        cargo = Path(shutil.which("cargo"))
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            (source / "src").mkdir()
            (source / "src/main.rs").write_text("fn main() {}\n")
            (source / "Cargo.toml").write_text(
                '[package]\nname="closure-fixture"\nversion="0.1.0"\n'
                'edition="2021"\nlicense="MIT"\n[workspace]\n'
                '[dependencies]\nserde="=1.0.228"\n[features]\ndefault=[]\n')
            subprocess.run([str(cargo), "generate-lockfile", "--offline"],
                           cwd=source, check=True, capture_output=True)
            metadata, lock, lock_sha = release_sbom.cargo_resolution(
                source, "x86_64-unknown-linux-gnu", cargo)
            by_id = {p["id"]: p for p in metadata["packages"]}
            nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
            root = metadata["resolve"]["root"]
            pending, included = [root], set()
            while pending:
                current = pending.pop()
                if current not in included:
                    included.add(current)
                    pending.extend(d["pkg"] for d in nodes[current]["deps"]
                                   if any(k["kind"] != "dev" for k in d["dep_kinds"]))
            self.assertIn("serde_core", {by_id[i]["name"] for i in included})
            locked = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
            packages, units, relations = [], [], []
            for identity in included:
                p = by_id[identity]
                key = (p["name"], p["version"], p["source"])
                packages.append(dict(id=identity, name=p["name"], version=p["version"],
                    source=p["source"] or "workspace:" + "1" * 40, license=p["license"],
                    checksum=locked[key].get("checksum"), manifest_sha256=(
                        security.file_hash(Path(p["manifest_path"])) if p["source"] is None else None)))
                units.append(dict(id="unit:" + identity, package=identity,
                    features=nodes[identity]["features"], kind="bin-closure-fixture" if identity == root else "lib",
                    context="target"))
                relations.extend(dict(kind="depends_on", source="unit:" + identity,
                    target="unit:" + d["pkg"], name=d["name"]) for d in nodes[identity]["deps"]
                    if d["pkg"] in included and any(k["kind"] != "dev" for k in d["dep_kinds"]))
            document = dict(payload_source=dict(commit="1" * 40, tree="2" * 40),
                resolution_contexts=[dict(role="cli", target="x86_64-unknown-linux-gnu",
                    features=["default"], lock_sha256=lock_sha)], packages=packages, build_units=units,
                binary_roots=[dict(role="cli", unit="unit:" + root, reachable_units=len(units))],
                relationships=relations)
            # Recompute the supplied projection and its digest after removing the node
            # and every incident edge; no dangling reference exposes the omission.
            omitted = next(p["id"] for p in packages if p["name"] == "serde_core")
            document["packages"] = [p for p in packages if p["id"] != omitted]
            document["build_units"] = [u for u in units if u["package"] != omitted]
            document["relationships"] = [r for r in relations
                if "unit:" + omitted not in (r["source"], r["target"])]
            document["binary_roots"][0]["reachable_units"] = len(document["build_units"])
            supplied = source / "inputs.json"
            supplied.write_bytes(release_sbom.sbom.canonical(document))
            digest = security.file_hash(supplied)
            self.assertEqual(digest, hashlib.sha256(release_sbom.sbom.canonical(document)).hexdigest())
            with self.assertRaisesRegex(ValueError, "Cargo closure"):
                release_sbom.validate_resolution(json.loads(supplied.read_bytes()),
                                                 metadata, lock, lock_sha, source)

    def test_real_locked_metadata_is_complete_deterministic_and_unchanged(self):
        cargo = Path(shutil.which("cargo"))
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            (source / "src").mkdir()
            (source / "src/lib.rs").write_text("pub fn value() -> u8 { 1 }\n")
            (source / "Cargo.toml").write_text(
                '[package]\nname="release-resolution-fixture"\nversion="0.1.0"\n'
                'edition="2021"\nlicense="MIT"\n[workspace]\n')
            subprocess.run([str(cargo), "generate-lockfile", "--offline"],
                           cwd=source, check=True, capture_output=True)
            before = (source / "Cargo.lock").read_bytes()
            first = release_sbom.cargo_resolution(source, "x86_64-unknown-linux-gnu", cargo)
            second = release_sbom.cargo_resolution(source, "x86_64-unknown-linux-gnu", cargo)
            self.assertEqual(first, second)
            metadata, lock, lock_sha = first
            self.assertEqual(metadata["packages"][0]["name"], "release-resolution-fixture")
            self.assertEqual(len(metadata["resolve"]["nodes"]), 1)
            self.assertEqual(before, (source / "Cargo.lock").read_bytes())
            self.assertEqual(lock_sha, hashlib.sha256(before).hexdigest())
            projection = {"resolution_contexts": [{"lock_sha256": "0" * 64}]}
            with self.assertRaisesRegex(ValueError, "Cargo.lock binding"):
                release_sbom.validate_resolution(projection, metadata, lock, lock_sha, source)
            projection["resolution_contexts"][0]["lock_sha256"] = lock_sha
            projection.update(packages=[], build_units=[], binary_roots=[])
            with self.assertRaisesRegex(ValueError, "incomplete"):
                release_sbom.validate_resolution(projection, metadata, lock, lock_sha, source)
            (source / "Cargo.toml").write_text((source / "Cargo.toml").read_text().replace('0.1.0', '0.2.0'))
            with self.assertRaisesRegex(ValueError, "no complete resolution"):
                release_sbom.cargo_resolution(source, "x86_64-unknown-linux-gnu", cargo)


class CargoClosureNeighbors(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.source = Path(self.directory.name)
        self.cargo = Path(shutil.which("cargo"))
        for name in ("closure-fixture", "platform-only", "optional-only"):
            root = self.source if name == "closure-fixture" else self.source / name
            (root / "src").mkdir(parents=True)
            binary = name == "closure-fixture"
            (root / "src" / ("main.rs" if binary else "lib.rs")).write_text(
                "fn main() {}\n" if binary else "pub fn value() {}\n")
            (root / "Cargo.toml").write_text(
                '[package]\nname="' + name + '"\nversion="0.1.0"\nlicense="MIT"\nedition="2021"\n')
        with (self.source / "Cargo.toml").open("a") as stream:
            stream.write('[workspace]\nresolver="2"\n[dependencies]\nserde="=1.0.228"\n'
                'optional-only={path="optional-only",optional=true}\n'
                "[target.'cfg(windows)'.dependencies]\nplatform-only={path=\"platform-only\"}\n"
                '[features]\ndefault=[]\nextra=["dep:optional-only"]\n')
        subprocess.run([str(self.cargo), "generate-lockfile", "--offline"],
                       cwd=self.source, check=True, capture_output=True)

    def projection(self, target="x86_64-unknown-linux-gnu", features=("default",)):
        resolution = release_sbom.cargo_resolution(self.source, target, self.cargo, features)
        metadata, lock, lock_sha = resolution
        nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
        root = metadata["resolve"]["root"]
        pending, included, edges = [root], set(), []
        while pending:
            identity = pending.pop()
            if identity in included:
                continue
            included.add(identity)
            for d in nodes[identity]["deps"]:
                if any(k["kind"] != "dev" for k in d["dep_kinds"]):
                    pending.append(d["pkg"])
                    edges.append(dict(kind="depends_on", source="unit:" + identity,
                                      target="unit:" + d["pkg"], name=d["name"]))
        locked = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
        packages, units = [], []
        for p in metadata["packages"]:
            if p["id"] not in included:
                continue
            key = p["name"], p["version"], p["source"]
            packages.append(dict(id=p["id"], name=p["name"], version=p["version"],
                source=p["source"] or "workspace:" + "1" * 40, license=p["license"],
                checksum=locked[key].get("checksum"), manifest_sha256=(
                    security.file_hash(Path(p["manifest_path"])) if p["source"] is None else None)))
            units.append(dict(id="unit:" + p["id"], package=p["id"],
                features=nodes[p["id"]]["features"], context="target",
                kind="bin-closure-fixture" if p["id"] == root else "lib"))
        return dict(payload_source=dict(commit="1" * 40, tree="2" * 40),
            resolution_contexts=[dict(role="cli", target=target, features=list(features), lock_sha256=lock_sha)],
            packages=packages, build_units=units, relationships=edges,
            binary_roots=[dict(role="cli", unit="unit:" + root, reachable_units=len(units))]), resolution

    def validate(self, document, resolution):
        release_sbom.validate_resolution(document, *resolution, self.source)

    def test_dev_feature_does_not_add_log_to_production_closure(self):
        manifest = self.source / "Cargo.toml"
        manifest.write_text(
            '[package]\nname="closure-fixture"\nversion="0.1.0"\n'
            'edition="2021"\nlicense="MIT"\n[workspace]\nresolver="2"\n'
            '[dependencies]\ntokio={version="=1.52.3",default-features=false,features=["net"]}\n'
            '[dev-dependencies]\nmio="=1.2.1"\n[features]\ndefault=[]\n')
        subprocess.run([str(self.cargo), "generate-lockfile", "--offline"],
                       cwd=self.source, check=True, capture_output=True)
        raw = json.loads(subprocess.check_output([str(self.cargo), "metadata", "--locked",
            "--offline", "--format-version", "1", "--filter-platform", "x86_64-unknown-linux-gnu"],
            cwd=self.source))
        raw_ids = {p["name"]: p["id"] for p in raw["packages"]}
        mio = next(n for n in raw["resolve"]["nodes"] if n["id"] == raw_ids["mio"])
        self.assertIn("log", mio["features"])
        self.assertTrue(any(d["pkg"] == raw_ids["log"] for d in mio["deps"]))
        oracle = subprocess.check_output([str(self.cargo), "tree", "--locked", "--offline",
            "--target", "x86_64-unknown-linux-gnu", "--edges", "normal,build", "--prefix", "none",
            "--format", "{p}"], cwd=self.source, text=True)
        self.assertIn("tokio v1.52.3", oracle)
        self.assertIn("mio v1.2.1", oracle)
        self.assertNotIn("log v", oracle)
        document, resolution = self.projection()
        # This expected production closure is fixed independently of the verifier:
        # Tokio disables Mio defaults; only the dev edge activates Mio's log.
        excluded = {p["id"] for p in document["packages"] if p["name"] == "log"}
        document["packages"] = [p for p in document["packages"] if p["id"] not in excluded]
        document["build_units"] = [u for u in document["build_units"] if u["package"] not in excluded]
        for unit in document["build_units"]:
            if unit["package"] == raw_ids["mio"]:
                unit["features"] = [f for f in unit["features"] if f not in ("default", "log")]
        document["relationships"] = [r for r in document["relationships"]
            if r["source"] not in {"unit:" + i for i in excluded}
            and r["target"] not in {"unit:" + i for i in excluded}
            and not (r["source"] == "unit:" + raw["resolve"]["root"]
                     and r["target"] == "unit:" + raw_ids["mio"])]
        document["binary_roots"][0]["reachable_units"] = len(document["build_units"])
        self.validate(document, resolution)

    def test_positive_closures_follow_target_and_optional_features(self):
        for target in ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"):
            for features in (("default",), ("default", "extra")):
                with self.subTest(target=target, features=features):
                    document, resolution = self.projection(target, features)
                    self.validate(document, resolution)
                    names = {p["name"] for p in document["packages"]}
                    self.assertEqual("platform-only" in names, "windows" in target)
                    self.assertEqual("optional-only" in names, "extra" in features)

    def test_build_features_and_renames_survive_without_dev_activation(self):
        for name in ("build-leaf", "dev-leaf"):
            path = self.source / name
            (path / "src").mkdir(parents=True)
            (path / "src/lib.rs").write_text("")
            (path / "Cargo.toml").write_text(
                '[package]\nname="' + name + '"\nversion="0.1.0"\nlicense="MIT"\n')
        with (self.source / "optional-only/Cargo.toml").open("a") as stream:
            stream.write('[dependencies]\n'
                'build-leaf={path="../build-leaf",optional=true}\n'
                'dev-leaf={path="../dev-leaf",optional=true}\n'
                '[features]\nhost=["dep:build-leaf"]\ndev=["dep:dev-leaf"]\n')
        (self.source / "Cargo.toml").write_text(
            '[package]\nname="closure-fixture"\nversion="0.1.0"\nlicense="MIT"\n'
            '[workspace]\nresolver="2"\n[features]\ndefault=[]\n'
            '[dependencies]\nrenamed={package="optional-only",path="optional-only"}\n'
            '[build-dependencies]\nrenamed={package="optional-only",path="optional-only",features=["host"]}\n'
            '[dev-dependencies]\nrenamed={package="optional-only",path="optional-only",features=["dev"]}\n')
        (self.source / "build.rs").write_text("fn main() {}\n")
        subprocess.run([str(self.cargo), "generate-lockfile", "--offline"],
                       cwd=self.source, check=True, capture_output=True)
        document, resolution = self.projection()
        self.assertEqual({p["name"] for p in document["packages"]},
                         {"closure-fixture", "optional-only", "build-leaf"})
        self.assertIn("renamed", {r["name"] for r in document["relationships"]})
        self.assertFalse(any("dev" in u["features"] for u in document["build_units"]))
        self.validate(document, resolution)
        omitted = next(p["id"] for p in document["packages"] if p["name"] == "build-leaf")
        document["packages"] = [p for p in document["packages"] if p["id"] != omitted]
        document["build_units"] = [u for u in document["build_units"] if u["package"] != omitted]
        document["relationships"] = [r for r in document["relationships"] if r["target"] != "unit:" + omitted]
        document["binary_roots"][0]["reachable_units"] = len(document["build_units"])
        with self.assertRaisesRegex(ValueError, "Cargo closure"):
            self.validate(document, resolution)

    def test_production_tree_join_fails_closed_on_ambiguous_or_incomplete_output(self):
        _, (metadata, _, _) = self.projection()
        for tree in ("", "0unknown v0.1.0|", "1closure-fixture v0.1.0|default",
                     "0closure-fixture v0.1.0|default\n2serde v1.0.228|",
                     "0closure-fixture v0.1.0|default\n0closure-fixture v0.1.0|default",
                     "0closure-fixture v0.1.0|default (*)", "not a Cargo tree"):
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                release_sbom.production_graph(metadata, tree)
        duplicate = copy.deepcopy(metadata)
        duplicate["packages"].append(dict(next(p for p in metadata["packages"]
            if p["name"] == "closure-fixture"), id="another-source"))
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            release_sbom.production_graph(duplicate, "0closure-fixture v0.1.0|default")
        with patch.object(release_sbom.subprocess, "run", side_effect=[
            subprocess.CompletedProcess([], 0, b"host: x86_64-unknown-linux-gnu\n"),
            subprocess.CompletedProcess([], 0, json.dumps(metadata).encode()),
            subprocess.CompletedProcess([], 1, b"", b"tree failed")]):
            with self.assertRaisesRegex(ValueError, "no complete production resolution"):
                release_sbom.cargo_resolution(self.source, "x86_64-unknown-linux-gnu", self.cargo)

    def test_edges_roots_units_and_package_identity_are_checked(self):
        document, resolution = self.projection()
        mutations = {
            "edge-omitted": lambda d: d["relationships"].pop(),
            "edge-renamed": lambda d: d["relationships"][0].update(name="foreign"),
            "root-features": lambda d: d["resolution_contexts"][0].update(features=[]),
            "root-swapped": lambda d: d["binary_roots"][0].update(unit=d["build_units"][-1]["id"]),
            "root-count": lambda d: d["binary_roots"][0].update(reachable_units=1),
            "unit-feature": lambda d: d["build_units"][-1].update(features=["foreign"]),
            "unit-duplicate": lambda d: d["build_units"].append(copy.deepcopy(d["build_units"][0])),
            "unit-unreachable": lambda d: d["build_units"].append(dict(d["build_units"][-1], id="unreachable")),
            "checksum": lambda d: d["packages"][-1].update(checksum="0" * 64),
            "license": lambda d: d["packages"][-1].update(license="foreign"),
        }
        for name, edit in mutations.items():
            with self.subTest(name=name):
                changed = copy.deepcopy(document)
                edit(changed)
                with self.assertRaises(ValueError):
                    self.validate(changed, resolution)

    def test_verify_sboms_resolves_each_feature_context_and_reuses_exact_contexts(self):
        document, _ = self.projection()
        extra, _ = self.projection(features=("default", "extra"))
        # Separate root units preserve Cargo's feature-dependent binary identities.
        extra_root = extra["binary_roots"][0]["unit"]
        root_unit = next(u for u in extra["build_units"] if u["id"] == extra_root)
        root_unit["id"] += ":extra"
        for edge in extra["relationships"]:
            if edge["source"] == extra_root:
                edge["source"] = root_unit["id"]
        extra["binary_roots"][0].update(role="helper", unit=root_unit["id"])
        extra["resolution_contexts"][0]["role"] = "helper"
        for field, identity in (("packages", "id"), ("build_units", "id")):
            document[field] = list({v[identity]: v for v in document[field] + extra[field]}.values())
        document["relationships"] = list({json.dumps(e, sort_keys=True): e
            for e in document["relationships"] + extra["relationships"]}.values())
        document["binary_roots"] += extra["binary_roots"]
        document["resolution_contexts"] += extra["resolution_contexts"]
        document["profile"] = release_sbom.sbom.PROFILE
        records, documents, inputs, descriptors = [], {}, {}, {}
        for name in ("first.tar.gz", "second.tar.gz"):
            (self.source / name).write_bytes(b"output")
            record = dict(name=name, role="archive", size=6, sha256=security.file_hash(self.source / name),
                          platform_key="linux-x86_64")
            current = dict(document, subject=dict(name=name, bytes=6, sha256=record["sha256"]))
            path = self.source / (name + ".json")
            path.write_bytes(release_sbom.sbom.canonical(current))
            documents[name] = inputs[name] = descriptors[name] = path
            records.append(record)
        # Pinned package reconstruction is covered by its own historical suites;
        # this boundary test supplies that result and runs real Cargo resolution.
        def reconstruct(descriptor, input_path, digest, subject):
            self.assertEqual(digest, security.file_hash(input_path))
            return json.loads(input_path.read_bytes()), {}
        with patch.object(release_sbom.sbom, "reconstruct", side_effect=reconstruct), \
             patch.object(release_sbom.sbom, "validate_document") as pinned, \
             patch.object(release_sbom, "cargo_resolution", wraps=release_sbom.cargo_resolution) as resolve:
            self.assertEqual(release_sbom.verify_sboms(records, self.source, documents, inputs,
                descriptors, self.source, "1" * 40, "2" * 40, self.cargo), 2)
            self.assertEqual(pinned.call_count, 2)
            self.assertEqual(resolve.call_count, 2)
            self.assertEqual({call.args[3] for call in resolve.call_args_list},
                             {("default",), ("default", "extra")})


if __name__ == "__main__":
    unittest.main()
