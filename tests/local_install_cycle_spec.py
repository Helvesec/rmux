"""Local archive file cycles; fixture processes are not RMUX wire/native proof.

Reuses the packaged installer fixtures. Real tar/ZIP containers are retained
when RMUX_TEST_FIXTURE_ROOT is set. RC/channel publication and the v8/v9 client
contract require the release campaign; these tests never credit those cells.
"""
import hashlib
import gzip
import json
import os
import shutil
import shlex
import subprocess
import sys
import tarfile
import unittest
import zipfile

from unix_installer_spec import InstallerCase, ROOT, fixture_root, write


def snapshot(prefix):
    return {str(p.relative_to(prefix)): (p.read_bytes(), p.stat().st_mode & 0o7777)
            for p in prefix.rglob("*") if p.is_file()}


@unittest.skipUnless(os.name == "posix", "Unix installer file cycles")
class LocalInstallCycle(InstallerCase):
    def package(self, name, version, obsolete=False):
        archive = self.archive(name)
        dispatcher = b'#!/bin/sh\nexec "$(dirname "$0")/../libexec/rmux/rmux" "$@"\n'
        helper = (f'#!/bin/sh\ncase "${{1:-}}" in\n'
                  f'--help) echo "usage: rmux fixture {version}" ;;\n'
                  f'--version) echo "rmux fixture {version}" ;;\n'
                  '*) exit 42 ;;\nesac\n').encode()
        daemon = f'#!/bin/sh\n# fixture daemon {version}\nexec sleep 120\n'.encode()
        for path, body in (("bin/rmux", dispatcher), ("libexec/rmux/rmux", helper),
                           ("bin/rmux-daemon", daemon)):
            write(archive / path, body, "0755")
        manifest = archive / "share/rmux/package-layout.tsv"
        if obsolete:
            write(archive / "share/rmux/obsolete", b"owned previous asset\n", "0644")
            with manifest.open("a") as output:
                output.write("share/rmux/obsolete\t0644\tshare/rmux/obsolete\trequired\tobsolete\tfile\n")
        sums = "".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(archive)}\n"
                       for p in sorted(archive.rglob("*")) if p.is_file() and p.name != "SHA256SUMS.txt")
        write(archive / "SHA256SUMS.txt", sums.encode(), "0644")
        packed = self.root / (name + ".tar.gz")
        with tarfile.open(packed, "w:gz") as output:
            output.add(archive, arcname="rmux")
        extracted = self.root / (name + "-extracted")
        with tarfile.open(packed) as source:
            source.extractall(extracted)
        return extracted / "rmux"

    def check_installed(self, archive, prefix, version):
        env = dict(os.environ, PATH=f"{prefix / 'bin'}:/usr/bin:/bin")
        self.assertEqual(shutil.which("rmux", path=env["PATH"]), str(prefix / "bin/rmux"))
        self.assertEqual(shutil.which("rmux-daemon", path=env["PATH"]), str(prefix / "bin/rmux-daemon"))
        result = subprocess.run(["rmux", "--version"], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"rmux fixture {version}\n")
        checked = subprocess.run([str(ROOT / "scripts/packaging/layout"),
                                  "--for-package", str(archive),
                                  "check-installed", "--variant", "unix-archive",
                                  "--package-root", str(archive), "--prefix", str(prefix)],
                                 capture_output=True, text=True)
        self.assertEqual(checked.returncode, 0, checked.stderr)

    def test_archived_cycle_restores_obsolete_files_and_preserves_running_fixture(self):
        old = self.package("previous", "0.10.0")
        new = self.package("candidate", "0.11.0", obsolete=True)
        # States label file-install stimuli. The live fixture is deliberately
        # not credited as a daemon protocol or session-preservation test.
        for state in ("absent", "idle", "active", "incompatible"):
            with self.subTest(state=state):
                prefix = self.root / ("cycle-" + state)
                installed = self.install(old, prefix)
                self.assertEqual(installed.returncode, 0, installed.stderr)
                self.check_installed(old, prefix, "0.10.0")
                write(prefix / "share/rmux/local-only", b"keep local\n", "0600")
                process = None
                if state != "absent":
                    process = subprocess.Popen([str(prefix / "bin/rmux-daemon")])
                try:
                    before = snapshot(prefix)
                    failed = self.install(new, prefix, environment={
                        "RMUX_INSTALL_TEST_FAIL_AT": "after-replace-asset"})
                    self.assertNotEqual(failed.returncode, 0)
                    self.assertEqual(snapshot(prefix), before)
                    upgraded = self.install(new, prefix)
                    self.assertEqual(upgraded.returncode, 0, upgraded.stderr)
                    self.check_installed(new, prefix, "0.11.0")
                    upgraded_files = snapshot(prefix)
                    failed = self.install(old, prefix, "--previous-package-root", str(new),
                                          environment={"RMUX_INSTALL_TEST_FAIL_AT": "after-remove-owned"})
                    self.assertNotEqual(failed.returncode, 0)
                    self.assertIn("after-remove-owned", failed.stderr)
                    self.assertEqual(snapshot(prefix), upgraded_files)
                    restored = self.install(old, prefix, "--previous-package-root", str(new))
                    self.assertEqual(restored.returncode, 0, restored.stderr)
                    self.check_installed(old, prefix, "0.10.0")
                    self.assertFalse((prefix / "share/rmux/obsolete").exists())
                    # Running old images are untouched by file replacement.
                    if process is not None:
                        self.assertIsNone(process.poll())
                    removed = self.install(old, prefix, "--uninstall")
                    self.assertEqual(removed.returncode, 0, removed.stderr)
                    self.assertEqual(snapshot(prefix), {"share/rmux/local-only": (b"keep local\n", 0o600)})
                    self.assertFalse(any(p.name.startswith(".rmux-") for p in prefix.rglob("*")))
                    repeat = self.install(old, prefix, "--uninstall")
                    self.assertNotEqual(repeat.returncode, 0)
                finally:
                    if process is not None:
                        process.terminate()
                        process.wait(timeout=5)

    def test_removal_refuses_changed_bytes_modes_links_missing_files_and_aliases(self):
        archive = self.package("owned", "0.10.0")
        for mutation in ("bytes", "mode", "symlink", "directory-link", "missing", "prefix-link"):
            with self.subTest(mutation=mutation):
                prefix = self.root / mutation
                result = self.install(archive, prefix)
                self.assertEqual(result.returncode, 0, result.stderr)
                owned = prefix / "share/man/man1/rmux.1"
                if mutation == "bytes":
                    owned.write_bytes(b"locally edited\n")
                elif mutation == "mode":
                    owned.chmod(0o600)
                elif mutation == "symlink":
                    owned.unlink()
                    owned.symlink_to(archive / "share/man/man1/rmux.1")
                elif mutation == "directory-link":
                    share = prefix / "share/man"
                    share.rename(prefix / "share/man-real")
                    share.symlink_to("man-real", target_is_directory=True)
                elif mutation == "missing":
                    owned.unlink()
                else:
                    actual = prefix.with_name(prefix.name + "-real")
                    prefix.rename(actual)
                    prefix.symlink_to(actual, target_is_directory=True)
                before = snapshot(prefix)
                refused = self.install(archive, prefix, "--uninstall")
                self.assertNotEqual(refused.returncode, 0)
                self.assertEqual(snapshot(prefix), before)
        prefix = self.root / "arguments"
        self.assertEqual(self.install(archive, prefix).returncode, 0)
        before = snapshot(prefix)
        for args in (("--uninstall", "--no-verify"),
                     ("--uninstall", "--previous-package-root", str(archive)),
                     ("--previous-package-root", ""),
                     ("--previous-package-root", str(archive), "--no-verify")):
            result = self.install(archive, prefix, *args)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(snapshot(prefix), before)
        for alias in (str(prefix) + "/../arguments", str(prefix) + "/", str(prefix) + "//bin"):
            result = self.install(archive, alias, "--uninstall")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(snapshot(prefix), before)

    def test_explicit_rollback_refuses_unowned_collisions_and_new_symlink_parents(self):
        new = self.package("new-layout", "0.11.0")
        old = self.package("old-layout", "0.10.0", obsolete=True)
        prefix = self.root / "rollback"
        self.assertEqual(self.install(new, prefix).returncode, 0)
        write(prefix / "share/rmux/obsolete", b"user-owned\n", "0644")
        before = snapshot(prefix)
        result = self.install(old, prefix, "--previous-package-root", str(new))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unowned file", result.stderr)
        self.assertEqual(snapshot(prefix), before)
        (prefix / "share/rmux/obsolete").unlink()
        outside = self.root / "outside"
        outside.mkdir()
        (prefix / "extra").symlink_to(outside, target_is_directory=True)
        (old / "share/rmux/obsolete").rename(old / "obsolete")
        manifest = old / "share/rmux/package-layout.tsv"
        manifest.write_text(manifest.read_text().replace(
            "share/rmux/obsolete\t0644\tshare/rmux/obsolete", "obsolete\t0644\textra/obsolete"))
        before = snapshot(prefix)
        result = self.install(old, prefix, "--previous-package-root", str(new))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symbolic link", result.stderr)
        self.assertEqual(snapshot(prefix), before)
        self.assertEqual(list(outside.iterdir()), [])

    def test_simulated_tap_keeps_the_archive_layout_and_owned_uninstall(self):
        from unix_installer_spec import layout
        archive = self.package("tap", "0.11.0")
        descriptor = layout.load()
        prefix = self.root / "tap-cellar"
        entries = {entry["id"]: placement
                   for entry, placement in layout.entries(descriptor, "unix-archive")}
        # Use the real formula mapping; simulate its local file placements,
        # without touching Homebrew's registry, Cellar, links or user prefix.
        formula = subprocess.run([str(ROOT / "scripts/packaging/layout"), "homebrew-install"],
                                 capture_output=True, text=True, check=True).stdout
        for item in descriptor["downstream"]["homebrew"]["install_map"]:
            placement = entries[item["entry"]]
            self.assertIn('"' + placement["path"] + '"', formula)
            target = prefix / placement["install"]
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(archive / placement["path"], target)
        for entry, placement in layout.entries(descriptor, "unix-archive"):
            destination = placement.get("install")
            if destination and not (prefix / destination).exists():
                target = prefix / destination
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(archive / placement["path"], target)
        self.check_installed(archive, prefix, "0.11.0")
        result = self.install(archive, prefix, "--uninstall")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(snapshot(prefix), {})

    @unittest.skipUnless(shutil.which("dpkg-deb"), "Debian collection requires native dpkg-deb")
    def test_deb_fixture_collects_the_package_payload_without_system_installation(self):
        from package_layout_spec import DEBIAN
        staged = self.root / "deb-staged"
        for path, mode in DEBIAN.items():
            write(staged / path, (path + " fixture\n").encode(), mode)
        write(staged / "DEBIAN/control", b"Package: rmux-install-cycle-fixture\nVersion: 0.10.0\n"
              b"Architecture: all\nMaintainer: Fixture <fixture@example.invalid>\nDescription: local fixture\n", "0644")
        artifact = self.root / "fixture.deb"
        subprocess.run([shutil.which("dpkg-deb"), "--root-owner-group", "--build",
                        str(staged), str(artifact)], capture_output=True, check=True)
        collected = self.root / "deb-collected"
        subprocess.run([shutil.which("dpkg-deb"), "--extract", str(artifact), str(collected)],
                       capture_output=True, check=True)
        self.assertEqual(snapshot(collected), {path: ((path + " fixture\n").encode(), int(mode, 8))
                                              for path, mode in DEBIAN.items()})
        checked = subprocess.run([str(ROOT / "scripts/packaging/layout"), "check-tree",
                                  "--variant", "debian", "--root", str(collected)],
                                 capture_output=True, text=True)
        self.assertEqual(checked.returncode, 0, checked.stderr)

    @unittest.skipUnless(os.environ.get("RMUX_TEST_RPMBUILD") or shutil.which("rpmbuild"),
                         "RPM collection requires native rpmbuild")
    def test_rpm_fixture_collects_with_the_existing_payload_reader(self):
        from package_layout_spec import RPM
        from package_rpm_repro_spec import rpm_header, rpm_cpio
        staged = self.root / "rpm-staged"
        for path, mode in RPM.items():
            write(staged / path, (path + " fixture\n").encode(), mode)
        top = self.root / "rpm-build"
        spec = self.root / "fixture.spec"
        files = "\n".join(f"%attr({mode},root,root) /{path}" for path, mode in RPM.items())
        spec.write_text("%global __os_install_post %{nil}\n%global _binary_payload w9.gzdio\n"
                        "Name: rmux-install-cycle-fixture\nVersion: 0.10.0\nRelease: 1\n"
                        "Summary: Local fixture\nLicense: MIT\nBuildArch: noarch\n"
                        "%description\nLocal fixture only.\n%install\nmkdir -p %{buildroot}\n"
                        f"cp -a {shlex.quote(str(staged))}/. %{{buildroot}}/\n%files\n{files}\n")
        program = os.environ.get("RMUX_TEST_RPMBUILD") or shutil.which("rpmbuild")
        result = subprocess.run([program, "-bb", "--define", "_rpmformat 4",
                                 "--define", f"_topdir {top}", str(spec)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        artifact, = top.glob("RPMS/**/*.rpm")
        raw = artifact.read_bytes()
        self.assertEqual(raw[:6], b"\xed\xab\xee\xdb\x03\x00")
        signature = rpm_header(raw, 96, 62)
        header = rpm_header(raw, (signature["end"] + 7) // 8 * 8, 63)
        # Collection does not assert the Linux reproducibility oracle's gzip
        # OS byte on macOS. Header and cpio population checks remain shared.
        payload = gzip.decompress(raw[header["end"]:])
        entries = rpm_cpio(payload)
        collected = self.root / "rpm-collected"
        for entry in entries[:-1]:
            mode, size = entry["fields"][1], entry["fields"][6]
            offset = entry["data_offset"]
            write(collected / entry["name"].removeprefix("./"),
                  payload[offset:offset + size], f"{mode & 0o7777:04o}")
        self.assertEqual(snapshot(collected), {path: ((path + " fixture\n").encode(), int(mode, 8))
                                              for path, mode in RPM.items()})

    @unittest.skipUnless(shutil.which("cargo"), "crate fixture requires native Cargo")
    def test_crate_fixture_local_install_upgrade_failed_upgrade_rollback_uninstall(self):
        prefix = self.root / "cargo-prefix"
        packages = []
        for version in ("0.10.0", "0.11.0"):
            package = self.root / ("crate-" + version)
            write(package / "src/main.rs", f'fn main() {{ println!("fixture {version}"); }}\n'.encode(), "0644")
            bins = "\n".join(f'[[bin]]\nname = "{name}"\npath = "src/main.rs"'
                             for name in ("rmux", "rmux-full", "rmux-daemon"))
            (package / "Cargo.toml").write_text('[package]\nname = "rmux-install-cycle-fixture"\n'
                f'version = "{version}"\nedition = "2021"\nlicense = "MIT"\n'
                f'[workspace]\n{bins}\n')
            cargo = shutil.which("cargo")
            subprocess.run([cargo, "package", "--offline", "--no-verify", "--allow-dirty",
                            "--manifest-path", str(package / "Cargo.toml")], capture_output=True, check=True)
            artifact, = (package / "target/package").glob("*.crate")
            collected = package / "collected"
            with tarfile.open(artifact) as source:
                source.extractall(collected)
            unpacked, = collected.iterdir()
            self.assertEqual((unpacked / "src/main.rs").read_bytes(), (package / "src/main.rs").read_bytes())
            # The registry is simulated through the source fixture's isolated
            # workspace. Its source bytes are the bytes just collected above;
            # Cargo's normalized archive manifest remains untouched.
            packages.append(package)
        def install(package, good=True):
            result = subprocess.run([cargo, "install", "--offline", "--locked", "--force",
                                     "--path", str(package), "--root", str(prefix)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode == 0, good, result.stdout + result.stderr)
        def versions(version):
            for name in ("rmux", "rmux-full", "rmux-daemon"):
                result = subprocess.run([str(prefix / "bin" / name)], capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout, f"fixture {version}\n")
        install(packages[0]); versions("0.10.0")
        install(packages[1]); versions("0.11.0")
        source = packages[0] / "src/main.rs"
        original = source.read_bytes()
        source.write_bytes(b"invalid Rust fixture\n")
        install(packages[0], good=False); versions("0.11.0")
        source.write_bytes(original)
        install(packages[0]); versions("0.10.0")
        write(prefix / "bin/local-only", b"keep local\n", "0644")
        subprocess.run([cargo, "uninstall", "--root", str(prefix), "rmux-install-cycle-fixture"],
                       capture_output=True, check=True)
        self.assertEqual([p.name for p in (prefix / "bin").iterdir()], ["local-only"])


class ZipCollection(unittest.TestCase):
    def setUp(self):
        self.root, cleanup = fixture_root(self.id())
        if cleanup is not None:
            self.addCleanup(cleanup)

    def test_windows_zip_fixture_collects_the_real_descriptor_population(self):
        # ZIP collection is portable; executing these byte sentinels as PE
        # binaries would be a false native proof and is never attempted.
        descriptor = json.loads((ROOT / "scripts/packaging/rmux-package-layout.json").read_text())
        archive = self.root / "windows.zip"
        expected = {}
        with zipfile.ZipFile(archive, "w") as output:
            for entry in descriptor["entries"]:
                placement = entry["variants"].get("windows-archive")
                if placement:
                    path = placement["path"]
                    expected[path] = (entry["id"] + " fixture\n").encode()
                    output.writestr(path, expected[path])
        extracted = self.root / "windows-extracted"
        with zipfile.ZipFile(archive) as source:
            source.extractall(extracted)
        result = subprocess.run([sys.executable, str(ROOT / "scripts/packaging/layout.py"), "check-tree",
                                 "--variant", "windows-archive", "--root", str(extracted)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({p.relative_to(extracted).as_posix(): p.read_bytes()
                          for p in extracted.rglob("*") if p.is_file()}, expected)


if __name__ == "__main__":
    unittest.main()
