"""Native filesystem/layout fixtures only; these do not qualify installed wire cells."""
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from installed_wire_packages import InstalledPackage, layout, native_image_path


class PackageProfiles(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.package, self.prefix = self.root / "package", self.root / "prefix"
        self.variant = "windows-archive" if os.name == "nt" else "unix-archive"
        descriptor = layout.load()
        for entry, placement in layout.entries(descriptor, self.variant):
            path = self.package / placement["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(entry["id"].encode())
            path.chmod(int(placement.get("mode", "0755" if entry["role"] == "executable" else "0644"), 8))
            if placement.get("install"):
                target = self.prefix / placement["install"]
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target)
                target.chmod(path.stat().st_mode)
        metadata = {"package_layout": descriptor["variants"][self.variant]["package_layout"],
                    "rmux_version": "0.11.0", "migration_fixture": True}
        for root in (self.package, self.prefix):
            (root / "share/rmux/artifact-metadata.json").write_text(json.dumps(metadata))

    def admitted(self):
        return InstalledPackage(self.package, self.prefix, "0.11.0", 9, fixture=True)

    def test_native_profile_maps_archive_sources_to_installed_destinations(self):
        package = self.admitted()
        suffix = ".exe" if os.name == "nt" else ""
        self.assertEqual(package.cli, self.prefix / ("bin/rmux" + suffix))
        self.assertEqual([str(Path(image["path"]).relative_to(self.prefix)) for image in package.verify()["images"]],
                         [str(Path(p + suffix)) for p in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux")])
        if os.name == "nt":
            self.assertEqual(package.executables[0], ("rmux.exe", "bin/rmux.exe"))
            self.assertEqual(package.executables[1], ("rmux-daemon.exe", "bin/rmux-daemon.exe"))

    def test_replacing_both_package_and_installed_image_is_refused(self):
        package = self.admitted()
        source, installed = package.executables[1]
        for path in (self.package / source, self.prefix / installed):
            path.write_bytes(b"different bytes")
        with self.assertRaisesRegex(ValueError, "changed since migration admission"):
            package.verify()

    def test_relative_roots_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "absolute"):
            InstalledPackage(Path("package"), self.prefix, "0.11.0", 9, fixture=True)

    @unittest.skipUnless(os.name == "posix", "released v8 Unix archive layout")
    def test_released_v8_layout_has_no_installed_package_manifest(self):
        metadata = {"package_layout": "rmux-package-v2", "rmux_version": "0.10.0",
                    "migration_fixture": True}
        for root in (self.package, self.prefix):
            (root / "share/rmux/artifact-metadata.json").write_text(json.dumps(metadata))
            (root / "share/rmux/package-layout.tsv").unlink()
        package = InstalledPackage(self.package, self.prefix, "0.10.0", 8, fixture=True)
        self.assertEqual(len(package.verify()["images"]), 3)
        (self.prefix / "bin/rmux-daemon").write_bytes(b"changed installed daemon")
        with self.assertRaisesRegex(ValueError, "installed package differs"):
            package.verify()

    def test_canonical_candidate_identity_works_without_runtime_instrumentation(self):
        commit = "a" * 40
        targets = {"darwin": "aarch64-apple-darwin", "linux": "x86_64-unknown-linux-gnu",
                   "win32": "x86_64-pc-windows-msvc"}
        magic = {"darwin": b"\xcf\xfa\xed\xfe", "linux": b"\x7fELF", "win32": b"MZ"}[sys.platform]
        metadata = {"package_layout": "rmux-package-v3", "rmux_version": "0.11.0",
                    "git_commit": commit, "target": targets[sys.platform]}
        for root in (self.package, self.prefix):
            (root / "share/rmux/artifact-metadata.json").write_text(json.dumps(metadata))
        for entry, placement in layout.entries(layout.load(), self.variant):
            if entry["id"] in ("cli", "daemon", "helper"):
                for path in (self.package / placement["path"], self.prefix / placement["install"]):
                    path.write_bytes(magic + b"RMUX_RELEASE_IDENTITY_V1:" + commit.encode() + b":END")
        package = InstalledPackage(self.package, self.prefix, "0.11.0", 9, commit=commit)
        self.assertEqual(len(package.verify()["images"]), 3)
        for invalid in (magic + b"RMUX_RELEASE_IDENTITY_V1:" + b"b" * 40 + b":END",
                        b"not-native-RMUX_RELEASE_IDENTITY_V1:" + commit.encode() + b":END"):
            source, installed = package.executables[0]
            for path in (self.package / source, self.prefix / installed):
                path.write_bytes(invalid)
            with self.assertRaises(ValueError):
                InstalledPackage(self.package, self.prefix, "0.11.0", 9, commit=commit)

    def test_live_process_outside_installed_package_is_refused(self):
        image = native_image_path(os.getpid())
        self.assertTrue(image.is_absolute() and image.is_file())
        with self.assertRaisesRegex(ValueError, "outside its installed package"):
            self.admitted().verify_process(os.getpid())


if __name__ == "__main__":
    unittest.main()
