"""Run all installed migration cells with declared non-product process fixtures."""
import hashlib
import json
import os
from pathlib import Path
import sys
import tarfile
import unittest

from unix_installer_spec import InstallerCase, ROOT, write, fixture_root

sys.path.insert(0, str(ROOT / "scripts"))
from installed_wire_migration import Migration
from installed_wire_packages import InstalledPackage
import subprocess


@unittest.skipUnless(os.name == "posix", "Unix installed process fixtures")
class InstalledCells(InstallerCase):
    def setUp(self):
        self.root, cleanup = fixture_root("w" + hashlib.sha256(self.id().encode()).hexdigest()[:8])
        if cleanup is not None:
            self.addCleanup(cleanup)

    def package(self, version, wire):
        archive = self.archive("package-" + version)
        source = (ROOT / "tests/fixtures/installed-wire.py").read_text()
        script = ("#!" + sys.executable + "\n" + source.replace("@VERSION@", version).replace("@WIRE@", str(wire))).encode()
        for relative in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            write(archive / relative, script, "0755")
        metadata = {"package_layout": "rmux-package-v3", "rmux_version": version, "migration_fixture": True}
        write(archive / "share/rmux/artifact-metadata.json", json.dumps(metadata).encode(), "0644")
        sums = "".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(archive)}\n"
                       for p in sorted(archive.rglob("*")) if p.is_file() and p.name != "SHA256SUMS.txt")
        write(archive / "SHA256SUMS.txt", sums.encode(), "0644")
        packed = self.root / (version + ".tar.gz")
        with tarfile.open(packed, "w:gz") as output:
            output.add(archive, arcname="rmux")
        extracted = self.root / ("extracted-" + version)
        with tarfile.open(packed) as source:
            source.extractall(extracted)
        package = extracted / "rmux"
        prefix = self.root / ("installed-" + version)
        result = self.install(package, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        return package, prefix

    def pair(self):
        old = InstalledPackage(*self.package("0.10.0", 8), "0.10.0", 8, fixture=True)
        new = InstalledPackage(*self.package("0.11.0", 9), "0.11.0", 9, fixture=True)
        return old, new

    def test_four_pairs_three_states_preserve_then_explicitly_restart(self):
        old, new = self.pair()
        runner = Migration(old, new, self.root / "cells")
        receipt = runner.run()
        self.assertEqual(receipt["status"], "pass")
        self.assertEqual(receipt["evidence_kind"], "fixture")
        self.assertEqual(receipt["product_qualification"], "pending_signed_package_or_RC_and_native_campaign")
        self.assertEqual(len(receipt["cells"]), 12)
        self.assertEqual({(c["client_wire"], c["daemon_wire"]) for c in receipt["cells"]}, {(8, 8), (8, 9), (9, 8), (9, 9)})
        for cell in receipt["cells"]:
            self.assertEqual(cell["status"], "pass", cell)
            self.assertFalse(Path(cell["endpoint"]).exists())
            if "refusal" in cell:
                refusal = cell["refusal"]
                self.assertEqual(refusal["returncode"], 1)
                self.assertEqual(refusal["before"], refusal["after_refusal"])
                self.assertEqual(refusal["restart"]["client"]["wire"], cell["daemon_wire"])
                self.assertNotEqual(cell["before"]["pid"], cell["after_restart"]["pid"])
                self.assertEqual(cell["after_restart"]["sessions"], [])
            if cell["state"] == "absent":
                self.assertEqual(cell["effective_daemon_wire"], cell["client_wire"])

    def test_changed_or_redirected_installed_executable_is_refused_before_launch(self):
        old, _ = self.pair()
        old.cli.write_text("#!/bin/sh\nexit 0\n")
        with self.assertRaisesRegex(ValueError, "installed package differs"):
            old.call(["--version"], os.environ)
        old.cli.unlink()
        old.cli.symlink_to(old.root / "bin/rmux")
        with self.assertRaises(ValueError):
            old.verify()

    def test_fixture_cannot_be_admitted_as_signed_product_or_different_version(self):
        package, prefix = self.package("0.10.0", 8)
        with self.assertRaisesRegex(ValueError, "fixture"):
            InstalledPackage(package, prefix, "0.10.0", 8, fixture=False, commit="a" * 40)
        with self.assertRaisesRegex(ValueError, "version"):
            InstalledPackage(package, prefix, "0.11.0", 9, fixture=True)

    def test_missing_observed_diagnostic_fails_cell_and_cleans_owned_daemon(self):
        old, new = self.pair()
        call = old.call
        def missing_diagnostic(arguments, *args, **kwargs):
            record = call(arguments, *args, **kwargs)
            if arguments[-1] == "start-server" and record["returncode"] == 1:
                record["stderr"] = "unrelated failure"
            return record
        old.call = missing_diagnostic
        runner = Migration(old, new, self.root / "negative")
        with self.assertRaisesRegex(ValueError, "exact exit code, diagnostic"):
            runner.cell(0, old, new, "active")
        self.assertEqual(runner.receipt["cells"][0]["status"], "fail")
        self.assertFalse(Path(runner.current["endpoint"]).exists())

    def test_mutating_package_and_installed_copy_cannot_redefine_admitted_bytes(self):
        old, _ = self.pair()
        for path in (old.cli, old.root / "bin/rmux"):
            path.write_text("#!/bin/sh\nexit 0\n")
        with self.assertRaisesRegex(ValueError, "changed since migration admission"):
            old.verify()

    def test_cli_runs_installed_matrix_and_reports_fixture_limit(self):
        old, new = self.pair()
        output = self.root / "cli"
        result = subprocess.run([sys.executable, str(ROOT / "scripts/installed_wire_migration.py"),
                                 "--old-package", str(old.root), "--old-prefix", str(old.prefix),
                                 "--new-package", str(new.root), "--new-prefix", str(new.prefix),
                                 "--output", str(output), "--fixture"], capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((output / "receipt.json").read_text())
        self.assertEqual(receipt["evidence_kind"], "fixture")
        self.assertEqual(len(receipt["cells"]), 12)


if __name__ == "__main__":
    unittest.main()
