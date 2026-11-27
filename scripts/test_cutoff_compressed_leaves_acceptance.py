"""Frozen S4208 acceptance: actual manpage gzip leaves in deb and RPM/cpio."""
import gzip
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

from package_fixture_helpers import deb_bytes, rpm_bytes

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "cutoff_compressed_leaves_acceptance", ROOT / "scripts/bench/cutoff_assets.py")
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class CompressedLeafAcceptance(unittest.TestCase):
    def test_real_gzip_manpage_in_clean_deb_and_rpm_is_inspected(self):
        manpage = (ROOT / "docs/man/rmux.1").read_bytes()
        self.assertTrue(manpage.startswith(b".TH RMUX"))
        compressed = gzip.compress(manpage, mtime=0)
        audit = ROOT / ".rmux-audit/s4208"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            for kind, build in (("deb", deb_bytes), ("rpm", rpm_bytes)):
                with self.subTest(kind=kind):
                    raw = build([("usr/bin/rmux", b"\x7fELF inert fixture"),
                                 ("usr/share/man/man1/rmux.1.gz", compressed)])
                    path = Path(directory) / ("clean." + kind)
                    path.write_bytes(raw)
                    result = assets.scan(path)
                    self.assertEqual(result["status"], "PASS")
                    self.assertEqual(result["sha256"], hashlib.sha256(raw).hexdigest())
                    self.assertEqual(result["bytes"], len(raw))
                    self.assertTrue(any(
                        "usr/share/man/man1/rmux.1.gz" in row["name"]
                        and row["sha256"] == hashlib.sha256(manpage).hexdigest()
                        and row["bytes"] == len(manpage)
                        for row in result["members"]))

    def test_decoded_manpage_marker_in_deb_and_rpm_is_a_finding(self):
        manpage = (ROOT / "docs/man/rmux.1").read_bytes() + b"\nRMUX_PERF_TRACE\n"
        audit = ROOT / ".rmux-audit/s4208"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            for kind, build in (("deb", deb_bytes), ("rpm", rpm_bytes)):
                with self.subTest(kind=kind):
                    path = Path(directory) / ("marker." + kind)
                    path.write_bytes(build([("usr/share/man/man1/rmux.1.gz",
                                             gzip.compress(manpage, mtime=0))]))
                    result = assets.scan(path)
                    self.assertEqual(result["status"], "FAIL")
                    self.assertTrue(any(
                        row["sha256"] == hashlib.sha256(manpage).hexdigest()
                        and any(hit["marker"] == "RMUX_PERF_TRACE"
                                for hit in row["findings"])
                        for row in result["members"]))


if __name__ == "__main__":
    unittest.main()
