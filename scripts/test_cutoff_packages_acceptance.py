"""Frozen S4207 acceptance: inspect clean deb/rpm and reject hidden sinks."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

from package_fixture_helpers import deb_bytes, rpm_bytes

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("cutoff_packages_acceptance", ROOT / "scripts/bench/cutoff_assets.py")
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class PackageAcceptance(unittest.TestCase):
    def test_clean_deb_and_rpm_payloads_are_inspected(self):
        payload = b"\x7fELF clean fixture bytes, never executed"
        audit = ROOT / ".rmux-audit/s4207"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            for kind, build in (("deb", deb_bytes), ("rpm", rpm_bytes)):
                with self.subTest(kind=kind):
                    path = Path(directory) / ("clean." + kind)
                    raw = build([("usr/bin/rmux", payload)])
                    path.write_bytes(raw)
                    result = assets.scan(path)
                    self.assertEqual(result["status"], "PASS")
                    self.assertEqual(result["sha256"], hashlib.sha256(raw).hexdigest())
                    self.assertTrue(any(row["name"].endswith("usr/bin/rmux")
                                        and row["sha256"] == hashlib.sha256(payload).hexdigest()
                                        for row in result["members"]))

    def test_marker_in_compressed_deb_and_rpm_payload_is_rejected(self):
        audit = ROOT / ".rmux-audit/s4207"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            for kind, build in (("deb", deb_bytes), ("rpm", rpm_bytes)):
                with self.subTest(kind=kind):
                    path = Path(directory) / ("sink." + kind)
                    path.write_bytes(build([("usr/bin/rmux", b"\x7fELF RMUX_PERF_TRACE")]))
                    try:
                        result = assets.scan(path)
                    except ValueError:
                        continue
                    self.assertEqual(result["status"], "FAIL")
                    self.assertTrue(any(row["findings"] for row in result["members"]))


if __name__ == "__main__":
    unittest.main()
