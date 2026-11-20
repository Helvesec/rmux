"""Owned removal checks the modes the installer publishes, by entry role."""
import os
import unittest

from unix_installer_spec import InstallerCase
from local_install_cycle_spec import snapshot


@unittest.skipUnless(os.name == "posix", "Unix installed permission modes")
class InstalledModes(InstallerCase):
    def mode_archive(self, name, format_name):
        if format_name in ("v2", "implicit-v2"):
            metadata = (b'{"package_layout":"rmux-package-v2"}\n'
                        if format_name == "v2" else b'{"schema":1}\n')
            archive = self.archive(name, metadata=metadata,
                                   skip={"share/rmux/package-layout.tsv"})
        else:
            rows = []
            for line in self.manifest().splitlines():
                fields = line.split("\t")
                if fields[0] in ("libexec/rmux/rmux", "share/man/man1/rmux.1"):
                    fields[1] = "-"
                rows.append("\t".join(fields))
            metadata = ('{"package_layout":"rmux-package-' + format_name + '"}\n').encode()
            archive = self.archive(name, metadata=metadata,
                                   manifest="\n".join(rows) + "\n")
        (archive / "libexec/rmux/rmux").chmod(0o751)
        (archive / "share/man/man1/rmux.1").chmod(0o640)
        return archive

    def installed_non_source_modes(self, archive, prefix):
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        helper = prefix / "libexec/rmux/rmux"
        self.assertEqual(helper.stat().st_mode & 0o7777, 0o755)
        self.assertEqual(helper.read_bytes(), (archive / "libexec/rmux/rmux").read_bytes())
        asset = prefix / "share/man/man1/rmux.1"
        self.assertEqual(asset.stat().st_mode & 0o7777, 0o640)
        self.assertEqual(asset.read_bytes(), (archive / "share/man/man1/rmux.1").read_bytes())

    def test_uninstall_accepts_installed_modes_for_each_package_format(self):
        for format_name in ("v2", "implicit-v2", "v3", "v4"):
            with self.subTest(format=format_name):
                archive = self.mode_archive(format_name, format_name)
                prefix = self.root / (format_name + "-prefix")
                self.installed_non_source_modes(archive, prefix)
                removed = self.install(archive, prefix, "--uninstall")
                self.assertEqual(removed.returncode, 0, removed.stderr)
                self.assertEqual(snapshot(prefix), {})

    def test_rollback_accepts_previous_installed_modes_for_each_package_format(self):
        replacement = self.archive("replacement")
        (replacement / "libexec/rmux/rmux").write_bytes(b"replacement helper\n")
        for format_name in ("v2", "implicit-v2", "v3", "v4"):
            with self.subTest(format=format_name):
                archive = self.mode_archive(format_name, format_name)
                prefix = self.root / (format_name + "-prefix")
                self.installed_non_source_modes(archive, prefix)
                restored = self.install(replacement, prefix, "--previous-package-root", str(archive))
                self.assertEqual(restored.returncode, 0, restored.stderr)
                self.assertEqual((prefix / "libexec/rmux/rmux").read_bytes(), b"replacement helper\n")
                self.assertEqual((prefix / "libexec/rmux/rmux").stat().st_mode & 0o7777, 0o755)

    def test_removal_refuses_changed_executable_and_asset_modes_before_mutation(self):
        replacement = self.archive("replacement")
        for format_name in ("v2", "implicit-v2", "v3", "v4"):
            archive = self.mode_archive(format_name, format_name)
            for relative, changed_mode in (("libexec/rmux/rmux", 0o751),
                                           ("share/man/man1/rmux.1", 0o600)):
                with self.subTest(format=format_name, path=relative):
                    prefix = self.root / (format_name + "-" + str(changed_mode))
                    self.installed_non_source_modes(archive, prefix)
                    (prefix / relative).chmod(changed_mode)
                    before = snapshot(prefix)
                    for package, args in ((archive, ("--uninstall",)),
                                          (replacement, ("--previous-package-root", str(archive)))):
                        refused = self.install(package, prefix, *args)
                        self.assertNotEqual(refused.returncode, 0)
                        self.assertIn("installed package identity differs", refused.stderr)
                        self.assertEqual(snapshot(prefix), before)
                    self.assertFalse(any(p.name.startswith(".rmux-") for p in prefix.rglob("*")))

    def test_removal_failure_and_signal_restore_normalized_modes_and_source_mode_assets(self):
        replacement = self.archive("replacement")
        for format_name in ("v2", "implicit-v2", "v3", "v4"):
            archive = self.mode_archive(format_name, format_name)
            prefix = self.root / (format_name + "-prefix")
            self.installed_non_source_modes(archive, prefix)
            before = snapshot(prefix)
            for package, args, checkpoint in (
                    (archive, ("--uninstall",), "after-remove-owned"),
                    (replacement, ("--previous-package-root", str(archive)), "after-replace-helper")):
                for injection, status in (("FAIL", 1), ("SIGNAL", 143)):
                    with self.subTest(format=format_name, operation=args[0], injection=injection):
                        failed = self.install(package, prefix, *args, environment={
                            "RMUX_INSTALL_TEST_" + injection + "_AT": checkpoint})
                        self.assertEqual(failed.returncode, status, failed.stderr)
                        self.assertEqual(snapshot(prefix), before)
                        self.assertFalse(any(p.name.startswith(".rmux-") for p in prefix.rglob("*")))


if __name__ == "__main__":
    unittest.main()
