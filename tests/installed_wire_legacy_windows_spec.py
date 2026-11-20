"""Historical v8 Windows layout as local files; never Windows runtime proof.

The file set and installed destinations come from v0.10.0 (bb921bb5e970),
scripts/package-windows.ps1 and scripts/install-windows.ps1. That release
predates package-layout.tsv and the bundled ConPTY runtime.
"""
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import installed_wire_packages as packages


LEGACY_FILES = {
    "rmux.exe": "bin/rmux.exe",
    "rmux-daemon.exe": "bin/rmux-daemon.exe",
    "libexec/rmux/rmux.exe": "libexec/rmux/rmux.exe",
    "install.ps1": None,
    "README.md": "README.md",
    "LICENSE-APACHE": "LICENSE-APACHE",
    "LICENSE-MIT": "LICENSE-MIT",
    "rmux.1": "rmux.1",
    "SHA256SUMS.txt": "SHA256SUMS.txt",
    **{path: path for path in (
        "share/bash-completion/completions/rmux",
        "share/zsh/site-functions/_rmux",
        "share/fish/vendor_completions.d/rmux.fish",
        "share/powershell/Completions/_rmux.ps1",
        "share/elvish/lib/rmux.elv",
        "share/rmux/artifact-metadata.json",
    )},
}


class LegacyWindowsPackage(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        self.archive, self.prefix = root / "archive", root / "installed"
        for source, destination in LEGACY_FILES.items():
            self.write(self.archive / source, source.encode())
            if destination:
                self.write(self.prefix / destination, source.encode())
        self.metadata()
        # Select the Windows package policy without changing pathlib or
        # pretending this host can execute Windows binaries.
        self.platform = patch.object(packages, "os", SimpleNamespace(name="nt"))
        self.platform.start()
        self.addCleanup(self.platform.stop)

    @staticmethod
    def write(path, contents):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)

    def metadata(self, version="0.10.0", identity="rmux-windows-package-v2"):
        value = {"package_layout": identity, "rmux_version": version,
                 "migration_fixture": True}
        for root in (self.archive, self.prefix):
            (root / "share/rmux/artifact-metadata.json").write_text(
                json.dumps(value), encoding="utf-8-sig")

    def admit(self, version="0.10.0", wire=8):
        return packages.InstalledPackage(self.archive, self.prefix, version, wire, fixture=True)

    def test_released_windows_v8_installed_files_are_admitted(self):
        package = self.admit()
        record = package.verify()
        self.assertEqual(record["path"], str(self.prefix / "bin/rmux.exe"))
        self.assertEqual({image["path"] for image in record["images"]}, {
            str(self.prefix / name) for name in (
                "bin/rmux.exe", "bin/rmux-daemon.exe", "libexec/rmux/rmux.exe")})

    def test_historical_layout_selection_also_applies_outside_fixture_mode(self):
        # Exercise admission only: these are still inert files, not executables
        # or signed packages, and no native qualification is claimed.
        commit = "bb921bb5e970c67883f2b4172a2608256d66e7fb"
        for root in (self.archive, self.prefix):
            path = root / "share/rmux/artifact-metadata.json"
            value = json.loads(path.read_text(encoding="utf-8-sig"))
            value.pop("migration_fixture")
            value["git_commit"] = commit
            path.write_text(json.dumps(value), encoding="utf-8-sig")
        package = packages.InstalledPackage(
            self.archive, self.prefix, "0.10.0", 8, commit=commit)
        self.assertEqual(package.verify()["commit"], commit)
        with self.assertRaisesRegex(ValueError, "commit differs"):
            packages.InstalledPackage(
                self.archive, self.prefix, "0.10.0", 8, commit="0" * 40)

    def test_legacy_exception_does_not_apply_to_other_version_wire_or_layout(self):
        for version, wire, identity in (
            ("0.11.0", 9, "rmux-windows-package-v2"),
            ("0.10.0", 9, "rmux-windows-package-v2"),
            ("0.10.0", 8, "rmux-windows-package-v3"),
            ("0.10.0", 8, "rmux-package-v2"),
        ):
            with self.subTest(version=version, wire=wire, identity=identity):
                self.metadata(version, identity)
                with self.assertRaises(ValueError):
                    self.admit(version, wire)

    def test_current_windows_package_keeps_manifest_installer_and_conpty_requirements(self):
        for entry, placement in packages.layout.entries(packages.layout.load(), "windows-archive"):
            data = entry["id"].encode()
            self.write(self.archive / placement["path"], data)
            if placement.get("install"):
                self.write(self.prefix / placement["install"], data)
        self.metadata("0.11.0", "rmux-windows-package-v3")
        self.admit("0.11.0", 9)
        for relative in (
            "install.ps1", "share/rmux/package-layout.tsv", "share/rmux/conpty-runtime.json",
            "share/licenses/Microsoft.Windows.Console.ConPTY/LICENSE",
            "bin/conpty.dll", "libexec/rmux/conpty.dll",
            "bin/OpenConsole.exe", "libexec/rmux/OpenConsole.exe",
        ):
            with self.subTest(missing=relative):
                path = self.prefix / relative
                data = path.read_bytes()
                path.unlink()
                try:
                    with self.assertRaisesRegex(ValueError, "missing installed file"):
                        self.admit("0.11.0", 9)
                finally:
                    path.write_bytes(data)

    def test_each_historical_installed_payload_is_still_required(self):
        for source in ("bin/rmux.exe", "bin/rmux-daemon.exe", "libexec/rmux/rmux.exe",
                       "share/bash-completion/completions/rmux"):
            with self.subTest(missing=source):
                path = self.prefix / source
                data = path.read_bytes()
                path.unlink()
                try:
                    with self.assertRaisesRegex(ValueError, "installed package differs"):
                        self.admit()
                finally:
                    path.write_bytes(data)

    def test_legacy_installed_images_remain_bound_to_admitted_package_bytes(self):
        for source, destination in (
            ("rmux.exe", "bin/rmux.exe"),
            ("rmux-daemon.exe", "bin/rmux-daemon.exe"),
            ("libexec/rmux/rmux.exe", "libexec/rmux/rmux.exe"),
        ):
            with self.subTest(image=source):
                package = self.admit()
                original = (self.archive / source).read_bytes()
                try:
                    (self.prefix / destination).write_bytes(b"changed")
                    with self.assertRaisesRegex(ValueError, "installed package differs"):
                        package.verify()
                    (self.archive / source).write_bytes(b"changed")
                    with self.assertRaisesRegex(ValueError, "changed since migration admission"):
                        package.verify()
                finally:
                    (self.archive / source).write_bytes(original)
                    (self.prefix / destination).write_bytes(original)


if __name__ == "__main__":
    unittest.main()
