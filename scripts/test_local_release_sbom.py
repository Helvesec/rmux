"""Exercise local SBOM generation with real Cargo builds and package bytes."""
import copy
from contextlib import contextmanager
import io
import json
import ntpath
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/release"))
import release_sbom


def _remove_readonly(function, path, error):
    if not isinstance(error, PermissionError):
        raise error
    os.chmod(path, stat.S_IWRITE)
    function(path)


@contextmanager
def sbom_fixture_directory(*, dir):
    if os.name != "nt":
        with tempfile.TemporaryDirectory(dir=dir) as directory:
            yield directory
        return
    directory = tempfile.mkdtemp(dir=dir)
    try:
        yield directory
    finally:
        absolute = os.path.abspath(directory)
        if not absolute.startswith("\\\\?\\"):
            absolute = ("\\\\?\\UNC\\" + absolute[2:] if absolute.startswith("\\\\")
                        else "\\\\?\\" + absolute)
        shutil.rmtree(absolute, onexc=_remove_readonly)


class LocalSbomCleanup(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "Windows long-path cleanup")
    def test_fixture_removes_real_long_path_tree(self):
        audit = ROOT / ".rmux-audit/local-sbom-cleanup-tests"
        audit.mkdir(parents=True, exist_ok=True)
        extended = None
        try:
            with sbom_fixture_directory(dir=audit) as directory:
                extended = "\\\\?\\" + os.path.abspath(directory)
                leaf = Path(extended)
                while len(str(leaf)) <= 300:
                    leaf /= "incremental-fixture-segment"
                leaf.mkdir(parents=True)
                (leaf / "work-products.bin").write_bytes(b"fixture")
                self.assertGreater(len(str(leaf)[4:]), 260)
                print(f"real long-path fixture: {len(str(leaf)[4:])} characters", flush=True)
            self.assertFalse(os.path.exists(extended))
        finally:
            # The red run must also leave no owned fixture behind.
            if extended and os.path.exists(extended):
                shutil.rmtree(extended)

    @unittest.skipUnless(os.name == "nt", "Windows read-only cleanup")
    def test_fixture_removes_readonly_long_path_file(self):
        audit = ROOT / ".rmux-audit/local-sbom-cleanup-tests"
        audit.mkdir(parents=True, exist_ok=True)
        extended = None
        target = None
        try:
            with sbom_fixture_directory(dir=audit) as directory:
                extended = "\\\\?\\" + os.path.abspath(directory)
                leaf = Path(extended)
                while len(str(leaf)) <= 300:
                    leaf /= "readonly-fixture-segment"
                leaf.mkdir(parents=True)
                target = leaf / "work-products.bin"
                target.write_bytes(b"readonly fixture")
                os.chmod(target, stat.S_IREAD)
                self.assertFalse(target.stat().st_mode & stat.S_IWRITE)
            self.assertFalse(os.path.exists(extended))
        finally:
            if target and target.exists():
                os.chmod(target, stat.S_IWRITE)
            if extended and os.path.exists(extended):
                shutil.rmtree(extended)

    def test_fixture_cleans_up_when_body_raises(self):
        audit = ROOT / ".rmux-audit/local-sbom-cleanup-tests"
        audit.mkdir(parents=True, exist_ok=True)
        with self.assertRaisesRegex(RuntimeError, "fixture body"):
            with sbom_fixture_directory(dir=audit) as directory:
                (Path(directory) / "payload").write_bytes(b"fixture")
                raise RuntimeError("fixture body")
        self.assertFalse(os.path.exists(directory))

    @unittest.skipUnless(os.name == "nt", "Windows cleanup failure propagation")
    def test_fixture_propagates_persistent_cleanup_error(self):
        audit = ROOT / ".rmux-audit/local-sbom-cleanup-tests"
        audit.mkdir(parents=True, exist_ok=True)
        directory = None
        handle = None
        try:
            with self.assertRaises(PermissionError):
                with sbom_fixture_directory(dir=audit) as directory:
                    locked = Path(directory) / "locked-file"
                    locked.write_bytes(b"fixture")
                    handle = locked.open("rb")
            self.assertTrue(os.path.isdir(directory))
        finally:
            if handle:
                handle.close()
            if directory and os.path.exists(directory):
                shutil.rmtree(directory)

    def test_readonly_retry_propagates_persistent_error(self):
        error = PermissionError("still denied")
        retry = Mock(side_effect=error)
        with patch.object(os, "chmod") as chmod:
            with self.assertRaises(PermissionError) as raised:
                _remove_readonly(retry, "fixture", error)
        self.assertIs(raised.exception, error)
        retry.assert_called_once_with("fixture")
        chmod.assert_called_once_with("fixture", stat.S_IWRITE)

    def test_nonpermission_error_is_not_retried(self):
        error = OSError("persistent filesystem error")
        with patch.object(os, "chmod") as chmod:
            with self.assertRaises(OSError) as raised:
                _remove_readonly(None, "fixture", error)
        self.assertIs(raised.exception, error)
        chmod.assert_not_called()

    @unittest.skipUnless(os.name == "nt", "Windows cleanup path normalization")
    def test_windows_cleanup_changes_only_the_cleanup_path(self):
        cases = (
            (r"relative\fixture", r"C:\fixture-root\relative\fixture",
             r"\\?\C:\fixture-root\relative\fixture"),
            (r"relative\fixture", r"\\server\share\fixture-root\relative\fixture",
             r"\\?\UNC\server\share\fixture-root\relative\fixture"),
            (r"C:\fixture", r"C:\fixture", r"\\?\C:\fixture"),
            (r"\\server\share\fixture", r"\\server\share\fixture", r"\\?\UNC\server\share\fixture"),
            (r"\\?\C:\fixture", r"\\?\C:\fixture", r"\\?\C:\fixture"),
            (r"\\?\UNC\server\share\fixture", r"\\?\UNC\server\share\fixture",
             r"\\?\UNC\server\share\fixture"),
        )
        for name, absolute, expected in cases:
            with self.subTest(name=name, absolute=absolute):
                with patch.object(tempfile, "mkdtemp", return_value=name) as create:
                    with patch.object(shutil, "rmtree") as remove:
                        with patch.object(os.path, "abspath", return_value=absolute) as abspath:
                            with sbom_fixture_directory(dir="fixture-parent") as directory:
                                self.assertEqual(directory, name)
                        abspath.assert_called_once_with(name)
                create.assert_called_once_with(dir="fixture-parent")
                remove.assert_called_once_with(expected, onexc=_remove_readonly)

    def test_windows_cleanup_normalization_with_simulated_unc_cwd(self):
        # Simulate Windows path semantics; this is not native filesystem evidence.
        check = type(self).test_windows_cleanup_changes_only_the_cleanup_path
        check = getattr(check, "__wrapped__", check)
        with (patch.object(os, "name", "nt"),
              patch.object(os, "getcwd", return_value=r"\\server\share\fixture-root"),
              patch.object(os.path, "join", ntpath.join),
              patch.object(os.path, "abspath", ntpath.abspath)):
            check(self)

    @unittest.skipIf(os.name == "nt", "Unix TemporaryDirectory branch")
    def test_unix_keeps_temporary_directory_cleanup(self):
        audit = ROOT / ".rmux-audit/local-sbom-cleanup-tests"
        audit.mkdir(parents=True, exist_ok=True)
        with patch.object(tempfile, "TemporaryDirectory", wraps=tempfile.TemporaryDirectory) as factory:
            with sbom_fixture_directory(dir=audit) as directory:
                self.assertTrue(Path(directory).is_dir())
                self.assertFalse(directory.startswith("\\\\?\\"))
        factory.assert_called_once_with(dir=audit)
        self.assertFalse(os.path.exists(directory))


class LocalSbomAcceptance(unittest.TestCase):
    def test_current_local_outputs_have_nondev_sboms_and_reject_tampering(self):
        self.assertTrue(callable(getattr(release_sbom, "local_document", None)),
                        "current local packages need a reconstructible SBOM generator")
        audit = ROOT / ".rmux-audit/local-sbom-tests"
        audit.mkdir(parents=True, exist_ok=True)
        with sbom_fixture_directory(dir=audit) as directory:
            source = Path(directory)
            (source / "src/bin").mkdir(parents=True)
            (source / "src/main.rs").write_text("fn main() { leaf::value(); }\n")
            (source / "src/bin/rmux-daemon.rs").write_text("fn main() { leaf::value(); }\n")
            for name in ("leaf", "devonly"):
                (source / name / "src").mkdir(parents=True)
                (source / name / "src/lib.rs").write_text("pub fn value() {}\n")
                (source / name / "Cargo.toml").write_text(
                    f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\nlicense="MIT"\n')
            (source / "Cargo.toml").write_text(
                '[package]\nname="rmux"\nversion="0.1.0"\nedition="2021"\nlicense="MIT"\n'
                '[workspace]\n[dependencies]\nleaf={path="leaf"}\n'
                '[dev-dependencies]\ndevonly={path="devonly"}\n'
                '[features]\ndefault=["web"]\nweb=[]\ntiny-cli=[]\n')
            cargo = Path(shutil.which("cargo"))
            def run(argv):
                return subprocess.run(argv, cwd=source, check=True, capture_output=True).stdout
            run([str(cargo), "generate-lockfile", "--offline"])
            for args in (["init", "-q"], ["add", "Cargo.toml", "Cargo.lock", "src", "leaf", "devonly"],
                         ["-c", "user.name=Sidney Sissaoui", "-c", "user.email=shideneyu@gmail.com",
                          "commit", "-qm", "Local SBOM fixture"]):
                run(["git", *args])
            target = next(line.split(": ", 1)[1] for line in
                          run([str(cargo), "-vV"]).decode().splitlines() if line.startswith("host: "))
            sha = run(["git", "rev-parse", "HEAD"]).decode().strip()
            tree = run(["git", "rev-parse", "HEAD^{tree}"]).decode().strip()
            output = source / "target" / sha / sys.platform
            logs, payloads = {}, {}
            for role, binary, features in (("helper", "rmux", []),
                                            ("cli", "rmux", ["--features", "tiny-cli"]),
                                            ("daemon", "rmux-daemon", [])):
                log = source / (role + ".jsonl")
                log.write_bytes(run([str(cargo), "build", "--locked", "--offline", "--target", target,
                    "--target-dir", str(output), "--message-format=json", "--bin", binary, *features]))
                logs[role] = log
                payloads[role] = (output / target / "debug" / (binary + (".exe" if os.name == "nt" else ""))).read_bytes()
            assets = source / "assets"
            assets.mkdir()
            metadata = dict(git_commit=sha, git_dirty=False, target=target, configuration="debug",
                            package_layout="rmux-package-v3", skip_build=True, release_artifact=False)
            paths = {"cli": "bin/rmux", "helper": "libexec/rmux/rmux", "daemon": "bin/rmux-daemon"}
            for role, prefix in (("cli", "binary"), ("helper", "helper_binary"), ("daemon", "daemon_binary")):
                metadata.update({prefix + "_path": paths[role], prefix + "_bytes": len(payloads[role]),
                                 prefix + "_sha256": release_sbom.sbom.digest(payloads[role])})
            documents = {}
            for number in (1, 2):
                subject = assets / f"output-{number}.tar.gz"
                with tarfile.open(subject, "w:gz") as archive:
                    files = {paths[r]: value for r, value in payloads.items()}
                    files["share/rmux/artifact-metadata.json"] = json.dumps(metadata).encode()
                    files["README.txt"] = f"output {number}\n".encode()
                    for name, content in files.items():
                        info = tarfile.TarInfo("package/" + name)
                        info.size = len(content)
                        archive.addfile(info, io.BytesIO(content))
                document = release_sbom.local_document(source, subject, cargo, logs)
                self.assertEqual(document["payload_source"], dict(commit=sha, tree=tree))
                self.assertEqual({p["name"] for p in document["packages"]}, {"rmux", "leaf"})
                self.assertEqual({c["role"] for c in document["resolution_contexts"]}, set(paths))
                self.assertIn("tiny-cli", next(c for c in document["resolution_contexts"] if c["role"] == "cli")["features"])
                path = source / (subject.name + ".sbom.json")
                path.write_bytes(release_sbom.sbom.canonical(document))
                documents[subject.name] = path
            self.assertEqual(release_sbom.verify_local_sboms(assets, documents, source, cargo, logs), 2)
            first = next(iter(documents.values()))
            altered = copy.deepcopy(json.loads(first.read_bytes()))
            altered["packages"] = [p for p in altered["packages"] if p["name"] != "leaf"]
            first.write_bytes(release_sbom.sbom.canonical(altered))
            with self.assertRaisesRegex(ValueError, "SBOM differs"):
                release_sbom.verify_local_sboms(assets, documents, source, cargo, logs)
            with self.assertRaisesRegex(ValueError, "every package output"):
                release_sbom.verify_local_sboms(assets, {}, source, cargo, logs)


if __name__ == "__main__":
    unittest.main()
