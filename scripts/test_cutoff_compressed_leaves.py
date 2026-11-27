"""Compressed-leaf boundaries; inert byte fixtures, no installed package proof."""
import bz2
import gzip
import hashlib
import io
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from package_fixture_helpers import deb_bytes, rpm_bytes, tar_bytes

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import cutoff_assets as assets


def zipped(files):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in files:
            archive.writestr(name, data)
    return stream.getvalue()


def codecs():
    result = [(".gz", gzip.compress), (".bz2", bz2.compress),
              (".xz", lzma.compress),
              (".lzma", lambda raw: lzma.compress(raw, format=lzma.FORMAT_ALONE))]
    try:
        from compression import zstd
    except ImportError:
        pass
    else:
        result.append((".zst", zstd.compress))
    return result


WRAPPERS = (deb_bytes, rpm_bytes, tar_bytes, zipped)


class CompressedLeaves(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".rmux-audit/s4208"
        audit.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.manpage = (ROOT / "docs/man/rmux.1").read_bytes()

    def scan(self, raw, name="asset.bin"):
        path = self.root / name
        path.write_bytes(raw)
        return assets.scan(path)

    def reject(self, raw, name="asset.bin"):
        with self.assertRaises(ValueError):
            self.scan(raw, name)

    def assert_decoded(self, result, raw, status):
        self.assertEqual(result["status"], status)
        rows = [row for row in result["members"]
                if row["sha256"] == hashlib.sha256(raw).hexdigest()
                and row["bytes"] == len(raw)]
        self.assertTrue(rows)
        self.assertEqual(any(row["findings"] for row in rows), status == "FAIL")

    def test_real_manpage_clean_and_marked_in_all_wrappers_and_codecs(self):
        for suffix, compress in codecs():
            for build in WRAPPERS:
                for name in ("usr/share/man/man1/rmux.1" + suffix, "resource.bin"):
                    for marker, status in ((b"", "PASS"), (b"\nRMUX_PERF_TRACE\n", "FAIL")):
                        with self.subTest(codec=suffix, wrapper=build.__name__, name=name, status=status):
                            decoded = self.manpage + marker
                            raw = build([(name, compress(decoded))])
                            result = self.scan(raw)
                            self.assertEqual(result["sha256"], hashlib.sha256(raw).hexdigest())
                            self.assert_decoded(result, decoded, status)

    def test_top_level_compressed_leaves_and_all_marker_encodings(self):
        for suffix, compress in codecs():
            for name in ("rmux.1" + suffix, "renamed.bin"):
                self.assert_decoded(self.scan(compress(self.manpage), name), self.manpage, "PASS")
                for marker in assets.MARKERS:
                    for encoding in ("utf-8", "utf-16-le", "utf-16-be"):
                        with self.subTest(codec=suffix, name=name, marker=marker, encoding=encoding):
                            decoded = marker.encode(encoding)
                            self.assert_decoded(self.scan(compress(decoded), name), decoded, "FAIL")

    def test_truncated_trailing_and_concatenated_leaves_are_rejected(self):
        for suffix, compress in codecs():
            encoded = compress(self.manpage)
            for build in WRAPPERS:
                for name in ("rmux.1" + suffix, "renamed.bin"):
                    for broken in (encoded[:-1], encoded[:len(encoded) // 2],
                                   encoded + b"junk", encoded + encoded):
                        with self.subTest(codec=suffix, wrapper=build.__name__, name=name, size=len(broken)):
                            self.reject(build([(name, broken)]))
                # A recognizable compression suffix still requires a valid stream
                # when truncation/corruption has destroyed its magic bytes.
                for broken in (b"", encoded[:1], b"ordinary text"):
                    self.reject(build([("rmux.1" + suffix, broken)]))

    def test_gzip_checksum_corruption_is_rejected(self):
        encoded = bytearray(gzip.compress(self.manpage))
        encoded[-8] ^= 1
        for build in WRAPPERS:
            self.reject(build([("rmux.1.gz", bytes(encoded))]))

    def test_advertised_tar_cannot_become_a_clean_compressed_leaf(self):
        for suffix, compress in codecs():
            names = ["resource.tar" + suffix]
            if suffix == ".gz":
                names.extend(("resource.tgz", "resource.crate", "resource.tar"))
            for name in names:
                for build in WRAPPERS:
                    with self.subTest(codec=suffix, wrapper=build.__name__, name=name):
                        self.reject(build([(name, compress(self.manpage))]))
                        self.reject(build([(name, compress(zipped([("safe", b"clean")])))]))

    def test_decoded_opaque_or_corrupt_archives_still_fail(self):
        broken_tar = bytearray(tar_bytes([("safe", b"clean")]))
        broken_tar[0] ^= 1  # Recognizable ustar header, invalid checksum.
        for suffix, compress in codecs():
            for name, decoded in (("resource.bin", b"hsqsopaque"),
                                  ("resource.bin", bytes(broken_tar)),
                                  ("resource.bin", b"!<arch>\ntruncated"),
                                  ("resource.bin", b"\xed\xab\xee\xdbtruncated"),
                                  ("resource.bin", b"PK\x03\x04truncated"),
                                  ("resource.snap" + suffix, b"opaque"),
                                  ("resource.nupkg" + suffix, b"opaque"),
                                  ("resource.zip" + suffix, b"opaque"),
                                  ("resource.deb" + suffix, b"opaque"),
                                  ("resource.rpm" + suffix, b"opaque")):
                for build in WRAPPERS:
                    with self.subTest(codec=suffix, wrapper=build.__name__, name=name):
                        self.reject(build([(name, compress(decoded))]))

    def test_recursion_recognizes_compressed_containers_and_preserves_paths(self):
        for suffix, compress in codecs():
            for inner in WRAPPERS:
                for content, status in ((b"clean", "PASS"), (b"RMUX_PERF_TRACE", "FAIL")):
                    with self.subTest(codec=suffix, inner=inner.__name__, status=status):
                        raw = zipped([("resource.bin", compress(inner([("payload", content)])))])
                        result = self.scan(raw)
                        self.assert_decoded(result, content, status)
                        expected = "resource.bin!" + ("data!" if inner in (deb_bytes, rpm_bytes) else "") + "payload"
                        self.assertTrue(any(row["name"] == expected for row in result["members"]))

    def test_depth_counts_compression_without_losing_existing_tar_neighborhood(self):
        raw = b"clean"
        for _ in range(4):
            raw = gzip.compress(tar_bytes([("resource.bin", raw)]))
        self.assertEqual(self.scan(raw)["status"], "PASS")
        self.reject(gzip.compress(tar_bytes([("resource.bin", raw)])))
        raw = b"RMUX_PERF_TRACE"
        for _ in range(4):
            raw = gzip.compress(raw)
        self.assertEqual(self.scan(raw)["status"], "FAIL")
        self.reject(gzip.compress(raw))

    def test_decoded_bytes_members_and_repeated_leaves_have_cumulative_bounds(self):
        for suffix, compress in codecs():
            leaf = compress(b"a" * 8000)
            single = zipped([("first" + suffix, leaf)])
            pair = zipped([("first" + suffix, leaf), ("second" + suffix, leaf)])
            with patch.object(assets, "MAX_BYTES", 12000):
                self.assertEqual(self.scan(single)["status"], "PASS")
                self.reject(pair)
            with patch.object(assets, "MAX_BYTES", 4000):
                self.reject(single)
            with patch.object(assets, "MAX_MEMBERS", 3):
                self.assertEqual(self.scan(single)["status"], "PASS")
                self.reject(pair)

    def test_decoded_inventory_names_do_not_alias_real_members(self):
        raw = gzip.compress(tar_bytes([("@decoded", b"clean")]))
        result = self.scan(raw)
        self.assertEqual(result["status"], "PASS")
        self.assertTrue(any(row["name"] == "@decoded" for row in result["members"]))
        self.assertEqual(len(result["members"]), len({row["name"] for row in result["members"]}))

    def test_missing_zstd_fails_closed_at_the_leaf_entry_path(self):
        with patch.dict(sys.modules, {"compression": None}):
            for build in WRAPPERS:
                with self.assertRaisesRegex(ValueError, "requires Python"):
                    self.scan(build([("rmux.1.zst", b"\x28\xb5\x2f\xfd")]))

    def test_cli_reports_clean_leaf_marker_and_truncated_stream(self):
        for build in (deb_bytes, rpm_bytes):
            paths = []
            for name, encoded in (("clean", gzip.compress(self.manpage)),
                                  ("marker", gzip.compress(b"RMUX_PERF_TRACE")),
                                  ("truncated", gzip.compress(self.manpage)[:-1])):
                path = self.root / (build.__name__ + name + ".bin")
                path.write_bytes(build([("usr/share/man/man1/rmux.1.gz", encoded)]))
                paths.append(path)
            output = self.root / "report.json"
            run = subprocess.run([sys.executable, str(Path(assets.__file__)), *map(str, paths),
                                  "--output", str(output)], capture_output=True, text=True, timeout=10)
            self.assertEqual(run.returncode, 1, run.stderr)
            results = json.loads(output.read_text())
            self.assertEqual([row["status"] for row in results], ["PASS", "FAIL", "FAIL"])
            self.assertNotIn("error", results[1])
            self.assertIn("error", results[2])


if __name__ == "__main__":
    unittest.main()
