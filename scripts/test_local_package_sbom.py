"""Local SBOM byte-boundary tests; Cargo results are explicit Python doubles."""
import copy
import hashlib
import json
import io
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile
from unittest.mock import patch

from package_fixture_helpers import deb_bytes, rpm_bytes, tar_bytes

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/release"))
import release_sbom as local


class LocalPackageSbom(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".rmux-audit/s4207"
        audit.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.target = "x86_64-unknown-linux-gnu"
        self.identity = dict(commit="1" * 40, tree="2" * 40)
        (self.root / "Cargo.toml").write_text('[package]\nname="rmux"\nversion="1.0.0"\n')
        (self.root / "Cargo.lock").write_text("fixture: resolution supplied by test double\n")
        self.logs, self.files = {}, []
        self.metadata = dict(target=self.target, git_commit=self.identity["commit"],
                             git_dirty=False, payload_provenance=None)
        for role, (prefix, binary) in local.LOCAL_ROLES.items():
            name = "usr/libexec/rmux/rmux" if role == "helper" else "usr/bin/" + binary
            data = ("ELF fixture " + role).encode()
            self.files.append((name, data))
            self.metadata.update({prefix + "_path": "/" + name,
                                  prefix + "_bytes": len(data),
                                  prefix + "_sha256": hashlib.sha256(data).hexdigest()})
            log = self.root / (role + ".jsonl")
            artifact = dict(reason="compiler-artifact", target=dict(name=binary, kind=["bin"]),
                            executable=str(self.root / self.target / binary),
                            manifest_path=str(self.root / "Cargo.toml"), profile=dict(test=False), features=[])
            log.write_bytes(local.sbom.canonical(artifact) + local.sbom.canonical(dict(reason="build-finished", success=True)))
            self.logs[role] = log
        self.resolution = dict(packages=[
            dict(id="root", name="rmux", version="1.0.0", source=None, license="MIT",
                 manifest_path=str(self.root / "Cargo.toml")),
            dict(id="leaf", name="leaf", version="1.0.0", source="registry:test", license="MIT")],
            resolve=dict(root="root", nodes=[
                dict(id="root", features=[], deps=[dict(name="leaf_alias", pkg="leaf", dep_kinds=[dict(kind=None)])]),
                dict(id="leaf", features=[], deps=[])]))
        self.lock = dict(package=[dict(name="rmux", version="1.0.0"),
                                 dict(name="leaf", version="1.0.0", source="registry:test", checksum="3" * 64)])
        self.lock_sha = local.file_hash(self.root / "Cargo.lock")

    def package(self, kind, files=None, metadata=None):
        metadata = self.metadata if metadata is None else metadata
        files = self.files if files is None else files
        raw = {"deb": deb_bytes, "rpm": rpm_bytes}[kind]([
            *files, ("usr/share/rmux/artifact-metadata.json", local.sbom.canonical(metadata))])
        subject = self.assets / ("rmux." + kind)
        subject.write_bytes(raw)
        return subject

    def generate(self, subject):
        with patch.object(local, "source_identity", return_value=self.identity), \
             patch.object(local, "cargo_resolution", return_value=(self.resolution, self.lock, self.lock_sha)):
            return local.local_document(self.root, subject, self.root / "cargo-not-executed", self.logs)

    def test_deb_rpm_inventory_resolution_unsigned_and_verify(self):
        documents = {}
        for kind in ("deb", "rpm"):
            subject = self.package(kind)
            result = self.generate(subject)
            self.assertEqual(result["profile"], local.LOCAL_PROFILE)
            self.assertEqual(result["signature_status"], "unsigned")
            self.assertEqual(result["build_provenance"], "local-logs-not-attested")
            self.assertEqual(result["subject"]["sha256"], hashlib.sha256(subject.read_bytes()).hexdigest())
            self.assertEqual({p["name"] for p in result["packages"]}, {"rmux", "leaf"})
            self.assertEqual({r["name"] for r in result["relationships"]}, {"leaf_alias"})
            self.assertEqual({r["path"] for r in result["binary_roots"]}, {n for n, _ in self.files})
            rows = {r["path"]: r for r in result["files"]}
            for name, data in self.files:
                self.assertEqual(rows[name]["sha256"], hashlib.sha256(data).hexdigest())
            self.assertEqual("@control/control" in rows, kind == "deb")
            document = self.root / (kind + ".sbom.json")
            document.write_bytes(local.sbom.canonical(result))
            documents[subject.name] = document
        with patch.object(local, "source_identity", return_value=self.identity), \
             patch.object(local, "cargo_resolution", return_value=(self.resolution, self.lock, self.lock_sha)):
            self.assertEqual(local.verify_local_sboms(self.assets, documents, self.root,
                                                     self.root / "cargo-not-executed", self.logs), 2)
            changed = json.loads(documents["rmux.deb"].read_bytes())
            changed["packages"].pop()
            documents["rmux.deb"].write_bytes(local.sbom.canonical(changed))
            with self.assertRaisesRegex(ValueError, "differs from reconstructed"):
                local.verify_local_sboms(self.assets, documents, self.root, self.root / "cargo-not-executed", self.logs)

    def test_same_names_changed_payload_rejected_against_metadata(self):
        files = [(name, b"changed same path") for name, _ in self.files]
        for kind in ("deb", "rpm"):
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, "binary differs"):
                self.generate(self.package(kind, files=files))

    def test_source_build_provenance_and_target_checks_remain(self):
        for field, value in (("git_commit", "4" * 40), ("git_dirty", True),
                             ("payload_provenance", {}), ("target", "unsupported")):
            changed = dict(self.metadata, **{field: value})
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "source/target"):
                self.generate(self.package("deb", metadata=changed))
        self.logs["cli"].write_text('{"reason":"build-finished","success":false}\n')
        with self.assertRaisesRegex(ValueError, "successful completion"):
            self.generate(self.package("rpm"))

    def test_duplicate_or_missing_metadata_and_traversal_fail(self):
        for build in (deb_bytes, rpm_bytes):
            path = self.root / "payload.bin"
            for files in (self.files, [*self.files, ("../artifact-metadata.json", b"{}")],
                          [*self.files, ("a/artifact-metadata.json", b"{}"),
                           ("b/artifact-metadata.json", b"{}")]):
                path.write_bytes(build(files))
                with self.assertRaises(ValueError):
                    local.local_archive(path)

    def test_snapshot_inventory_does_not_reopen_subject(self):
        subject = self.package("rpm")
        original = subject.read_bytes()
        subject.write_bytes(b"different bytes after snapshot")
        files, metadata = local.local_archive(subject, original)
        self.assertEqual(metadata, self.metadata)
        self.assertEqual(files[self.files[0][0]]["sha256"], hashlib.sha256(self.files[0][1]).hexdigest())
        with self.assertRaises(ValueError):
            local.local_archive(subject)

    def test_package_mutated_during_resolution_is_rejected(self):
        subject = self.package("deb")
        def change(*args):
            subject.write_bytes(b"replacement")
            return self.resolution, self.lock, self.lock_sha
        with patch.object(local, "source_identity", return_value=self.identity), \
             patch.object(local, "cargo_resolution", side_effect=change):
            with self.assertRaisesRegex(ValueError, "changed during resolution"):
                local.local_document(self.root, subject, self.root / "cargo-not-executed", self.logs)

    def test_existing_tar_zip_local_profile_and_metadata_remain_unchanged(self):
        import gzip
        metadata = dict(self.metadata)
        for prefix, _ in local.LOCAL_ROLES.values():
            metadata[prefix + "_path"] = metadata[prefix + "_path"].removeprefix("/")
        files = [("package/" + n, b) for n, b in [*self.files,
                 ("artifact-metadata.json", local.sbom.canonical(metadata))]]
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data in files:
                archive.writestr(name, data)
        for suffix, raw in ((".tar.gz", gzip.compress(tar_bytes(files))), (".zip", output.getvalue())):
            subject = self.assets / ("rmux" + suffix)
            subject.write_bytes(raw)
            result = self.generate(subject)
            self.assertEqual(result["signature_status"], "unsigned")
            self.assertEqual(result["subject"]["sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual({row["path"] for row in result["files"]},
                             {n for n, _ in self.files} | {"artifact-metadata.json"})

    def test_cargo_production_graph_keeps_only_the_supplied_nondev_edges(self):
        metadata = copy.deepcopy(self.resolution)
        metadata["packages"].append(dict(id="dev", name="dev-only", version="1.0.0", source="registry:test"))
        metadata["resolve"]["nodes"][0]["deps"].append(dict(name="dev_only", pkg="dev", dep_kinds=[dict(kind="dev")]))
        metadata["resolve"]["nodes"].append(dict(id="dev", features=[], deps=[]))
        graph = local.production_graph(metadata, "0rmux v1.0.0|\n1leaf v1.0.0|\n")
        self.assertEqual({node["id"] for node in graph["nodes"]}, {"root", "leaf"})
        self.assertEqual(graph["nodes"][0]["deps"][0]["name"], "leaf_alias")
        for tree in ("0rmux v1.0.0|\n1dev-only v1.0.0|\n", "0rmux v1.0.0|\n2leaf v1.0.0|\n"):
            with self.assertRaises(ValueError):
                local.production_graph(metadata, tree)


if __name__ == "__main__":
    unittest.main()
