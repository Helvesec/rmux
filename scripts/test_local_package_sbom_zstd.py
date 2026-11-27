"""Real zstd bytes through inventory, local SBOM and reconstruction.

Requires Python compression.zstd: a missing runtime is an environment error,
never an omitted positive variant. Cargo/source identities remain explicit
fixtures from test_local_package_sbom, not real resolution or RC evidence.
"""
import copy
import gzip
import hashlib
import sys
import unittest
from unittest.mock import patch

from package_fixture_helpers import ar_bytes, cpio_bytes, rpm_bytes, tar_bytes
import test_local_package_sbom as fixtures

local = fixtures.local
reader = local.package_payloads
sys.path.insert(0, str(fixtures.ROOT / "scripts/bench"))
import cutoff_assets as assets


class LocalZstdPackageSbom(fixtures.LocalPackageSbom):
    """Also rerun inherited package() consumers with actual zstd payloads."""

    CONTROL = [("control", b"Package: fixture\nVersion: 1\nArchitecture: all\n")]
    VARIANTS = (("deb", "zstd", "gzip"), ("deb", "gzip", "zstd"),
                ("deb", "zstd", "zstd"), ("rpm", "none", "zstd"))

    @classmethod
    def setUpClass(cls):
        # Deliberately no skip or codec fallback: this module needs zstd proof.
        from compression import zstd
        cls.zstd = zstd

    def members(self, files=None, metadata=None):
        return [*(self.files if files is None else files),
                ("usr/share/rmux/artifact-metadata.json", local.sbom.canonical(
                    self.metadata if metadata is None else metadata))]

    def encode(self, raw, codec):
        if codec == "gzip":
            return gzip.compress(raw, mtime=0)
        self.assertEqual(codec, "zstd")
        frame = self.zstd.compress(raw, options={self.zstd.CompressionParameter.checksum_flag: 1})
        self.assertTrue(frame.startswith(b"\x28\xb5\x2f\xfd"))
        self.assertTrue(frame[4] & 4, "fixture must carry a real zstd checksum")
        self.assertNotEqual(frame, raw)
        self.assertEqual(self.zstd.decompress(frame), raw)
        return frame

    def encoded_package(self, kind, members, control_codec="zstd", data_codec="zstd",
                        damage=None):
        decoded = ({"control": tar_bytes(self.CONTROL), "data": tar_bytes(members)}
                   if kind == "deb" else {"data": cpio_bytes(members)})
        codecs = {"control": control_codec, "data": data_codec}
        streams = {section: self.encode(raw, codecs[section]) for section, raw in decoded.items()}
        if damage is not None:
            section, change = damage
            streams[section] = change(streams[section])
        if kind == "rpm":
            return rpm_bytes([], data_codec, streams["data"])
        suffixes = {"gzip": ".gz", "zstd": ".zst"}
        return ar_bytes([("debian-binary", b"2.0\n"), *[
            (section + ".tar" + suffixes[codecs[section]], streams[section])
            for section in ("control", "data")]])

    def write_package(self, kind, raw):
        subject = self.assets / ("rmux." + kind)
        subject.write_bytes(raw)
        return subject

    def package(self, kind, files=None, metadata=None):
        return self.write_package(kind, self.encoded_package(kind, self.members(files, metadata)))

    def verify(self, documents):
        with patch.object(local, "source_identity", return_value=self.identity), \
             patch.object(local, "cargo_resolution", return_value=(self.resolution, self.lock, self.lock_sha)):
            return local.verify_local_sboms(self.assets, documents, self.root,
                                            self.root / "cargo-not-executed", self.logs)

    def test_zstd_control_data_and_cpio_inventory_sbom_roundtrip(self):
        documents = {}
        for kind, control_codec, data_codec in self.VARIANTS:
            with self.subTest(kind=kind, control=control_codec, data=data_codec):
                members = self.members()
                raw = self.encoded_package(kind, members, control_codec, data_codec)
                subject = self.write_package(kind, raw)
                expected = {name: dict(path=name, bytes=len(data),
                                      sha256=hashlib.sha256(data).hexdigest(), mode=0o644)
                            for name, data in members}
                if kind == "deb":
                    for name, data in self.CONTROL:
                        path = "@control/" + name
                        expected[path] = dict(path=path, bytes=len(data),
                                              sha256=hashlib.sha256(data).hexdigest(), mode=0o644)
                inventory, metadata = local.local_archive(subject)
                self.assertEqual(inventory, expected)
                self.assertEqual(metadata, self.metadata)
                cutoff = assets.scan(subject)
                self.assertEqual(cutoff["status"], "PASS")
                self.assertEqual(cutoff["sha256"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(cutoff["bytes"], len(raw))
                leaves = {row["name"]: (row["bytes"], row["sha256"])
                          for row in cutoff["members"] if not row["name"].startswith("metadata!")}
                self.assertEqual(leaves, {
                    ("control!" + path.removeprefix("@control/") if path.startswith("@control/")
                     else "data!" + path): (row["bytes"], row["sha256"])
                    for path, row in expected.items()})
                document = self.generate(subject)
                self.assertEqual(document["files"], sorted(expected.values(), key=lambda row: row["path"]))
                self.assertEqual(document["subject"], dict(name=subject.name, bytes=len(raw),
                                                          sha256=hashlib.sha256(raw).hexdigest()))
                self.assertEqual(document["signature_status"], "unsigned")
                self.assertEqual(document["build_provenance"], "local-logs-not-attested")
                self.assertEqual({root["role"] for root in document["binary_roots"]}, set(local.LOCAL_ROLES))
                path = self.root / (kind + ".sbom.json")
                path.write_bytes(local.sbom.canonical(document))
                documents[subject.name] = path
                self.assertEqual(self.verify(documents), len(documents))

    def test_zstd_sbom_tampering_is_rejected_by_reconstruction(self):
        documents = {}
        for kind in ("deb", "rpm"):
            subject = self.package(kind)
            original = self.generate(subject)
            path = self.root / (kind + ".sbom.json")
            documents[subject.name] = path
            path.write_bytes(local.sbom.canonical(original))
            self.assertEqual(self.verify(documents), len(documents))
            for field, value in (("sha256", "0" * 64), ("bytes", 0),
                                 ("path", "usr/bin/foreign"), ("mode", 0o777)):
                with self.subTest(kind=kind, field=field):
                    changed = copy.deepcopy(original)
                    changed["files"][0][field] = value
                    path.write_bytes(local.sbom.canonical(changed))
                    with self.assertRaisesRegex(ValueError, "differs from reconstructed"):
                        self.verify(documents)
            path.write_bytes(local.sbom.canonical(original))

    def test_zstd_corruption_truncation_trailing_and_second_frame_fail_closed(self):
        damages = {
            "checksum": (lambda frame: frame[:-1] + bytes([frame[-1] ^ 1]), "corrupt zstd"),
            "magic": (lambda frame: b"\0" + frame[1:], "corrupt zstd"),
            "truncated": (lambda frame: frame[:-1], "truncated or trailing"),
            "trailing": (lambda frame: frame + b"trailing", "truncated or trailing"),
            "second-frame": (lambda frame: frame + frame, "truncated or trailing"),
        }
        documents = {}
        for kind, section in (("deb", "control"), ("deb", "data"), ("rpm", "data")):
            subject = self.package(kind)
            path = self.root / (kind + ".sbom.json")
            path.write_bytes(local.sbom.canonical(self.generate(subject)))
            documents[subject.name] = path
            self.assertEqual(self.verify(documents), len(documents))
            for name, (change, message) in damages.items():
                with self.subTest(kind=kind, section=section, damage=name):
                    subject.write_bytes(self.encoded_package(kind, self.members(), damage=(section, change)))
                    for consume in (local.local_archive, self.generate, assets.scan):
                        with self.assertRaisesRegex(ValueError, message):
                            consume(subject)
                    with self.assertRaisesRegex(ValueError, message):
                        self.verify(documents)
            self.package(kind)

    def test_zstd_decoded_bytes_and_member_limits_include_deb_control(self):
        for kind in ("deb", "rpm"):
            with self.subTest(kind=kind):
                members = self.members()
                subject = self.package(kind)
                raw = subject.read_bytes()
                expanded = (len(tar_bytes(self.CONTROL)) + len(tar_bytes(members))
                            if kind == "deb" else len(cpio_bytes(members)))
                count = len(members) + (len(self.CONTROL) if kind == "deb" else 0)
                self.assertLess(len(raw), expanded)
                entries = list(reader.entries(raw, kind, max_bytes=expanded, max_members=count))
                self.assertEqual(len([entry for entry in entries if entry.section != "metadata"]), count)
                for limits, message in ((dict(max_bytes=expanded - 1), "package expansion limit"),
                                        (dict(max_bytes=len(raw) - 1), "package input bound"),
                                        (dict(max_members=count - 1), "member limit")):
                    with self.assertRaisesRegex(ValueError, message):
                        list(reader.entries(raw, kind, **limits))
                # Cutoff also charges decoded archive metadata and each leaf.
                cutoff_bytes = sum(len(entry.data) for entry in entries)
                with patch.object(assets, "MAX_BYTES", cutoff_bytes):
                    self.assertEqual(assets.scan(subject)["status"], "PASS")
                with patch.object(assets, "MAX_BYTES", cutoff_bytes - 1):
                    with self.assertRaisesRegex(ValueError, "asset expansion limit"):
                        assets.scan(subject)

    def test_zstd_local_sbom_member_limit_at_real_boundary(self):
        documents = {}
        for kind in ("deb", "rpm"):
            count = 1024 - len(self.members()) - (len(self.CONTROL) if kind == "deb" else 0)
            files = [*self.files, *[("usr/share/fixture/" + str(i), b"safe") for i in range(count)]]
            subject = self.package(kind, files=files)
            with self.subTest(kind=kind):
                document = self.generate(subject)
                self.assertEqual(len(document["files"]), 1024)
                path = self.root / (kind + ".sbom.json")
                path.write_bytes(local.sbom.canonical(document))
                documents[subject.name] = path
                self.assertEqual(self.verify(documents), len(documents))
                subject = self.package(kind, files=[*files, ("usr/share/fixture/overflow", b"safe")])
                with self.assertRaisesRegex(ValueError, "member limit"):
                    self.generate(subject)
                with self.assertRaisesRegex(ValueError, "member limit"):
                    self.verify(documents)
                self.package(kind, files=files)

    def test_zstd_local_metadata_size_limit_at_real_boundary(self):
        encoded = local.sbom.canonical(self.metadata)
        for kind in ("deb", "rpm"):
            for extra in (0, 1):
                with self.subTest(kind=kind, excess_bytes=extra):
                    metadata = encoded + b" " * (1024 * 1024 + extra - len(encoded))
                    members = [*self.files, ("usr/share/rmux/artifact-metadata.json", metadata)]
                    subject = self.write_package(kind, self.encoded_package(kind, members))
                    if extra:
                        with self.assertRaisesRegex(ValueError, "metadata bound exceeded"):
                            self.generate(subject)
                    else:
                        document = self.generate(subject)
                        row = next(row for row in document["files"] if row["path"].endswith("artifact-metadata.json"))
                        self.assertEqual(row["bytes"], 1024 * 1024)
                        self.assertEqual(row["sha256"], hashlib.sha256(metadata).hexdigest())

    def test_zstd_unsafe_duplicate_missing_or_ambiguous_metadata_is_rejected(self):
        for kind in ("deb", "rpm"):
            for label, members, message in (
                ("traversal", self.members([*self.files, ("../escape", b"safe")]), "unsafe package member"),
                ("duplicate", self.members([*self.files, self.files[0]]), "duplicate package member"),
                ("missing", self.files, "exactly one artifact metadata"),
                ("ambiguous", [*self.members(), ("extra/artifact-metadata.json", b"{}")], "exactly one artifact metadata"),
            ):
                with self.subTest(kind=kind, case=label):
                    subject = self.write_package(kind, self.encoded_package(kind, members))
                    with self.assertRaisesRegex(ValueError, message):
                        self.generate(subject)

    def test_zstd_cutoff_rejects_markers_in_decoded_package_bytes(self):
        for kind, control_codec, data_codec in self.VARIANTS:
            for marker in assets.MARKERS:
                for encoding in ("utf-8", "utf-16-le", "utf-16-be"):
                    with self.subTest(kind=kind, control=control_codec, data=data_codec,
                                      marker=marker, encoding=encoding):
                        content = marker.encode(encoding)
                        members = self.members([*self.files, ("usr/share/fixture/marker", content)])
                        subject = self.write_package(kind, self.encoded_package(
                            kind, members, control_codec, data_codec))
                        result = assets.scan(subject)
                        self.assertEqual(result["status"], "FAIL")
                        row = next(row for row in result["members"]
                                   if row["name"] == "data!usr/share/fixture/marker")
                        self.assertEqual(row["sha256"], hashlib.sha256(content).hexdigest())
                        self.assertTrue(any(hit["marker"] == marker for hit in row["findings"]))


if __name__ == "__main__":
    unittest.main()
