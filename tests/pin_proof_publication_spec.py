"""INT331 publication preserves original authority and rejects broken provenance."""
from contextlib import contextmanager
import hashlib
import io
import json
import os
from pathlib import Path, PureWindowsPath
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import pin_proof_publication as publication


def borrow_objects(repository, objects):
    # Git reads alternates as bytes: a Windows text-mode write would end the path
    # with CR and Git would look for '.../objects\r'.
    (repository / ".git/objects/info/alternates").write_bytes(os.fsencode(objects) + b"\n")


def original_bytes(name):
    # A Git object path always uses '/', whatever the host path flavour.
    return subprocess.check_output(["git", "-C", str(ROOT), "show",
        publication.ORIGIN + ":" + (publication.DIRECTORY / name).as_posix()])


@contextmanager
def windows_text_newlines():
    """Translate '\n' to '\r\n' on untranslated text writes, as Python does on Windows."""
    real = io.open

    def translating(file, mode="r", buffering=-1, encoding=None, errors=None, newline=None,
                    *args, **kwargs):
        if "b" not in mode and newline is None and set(mode) & set("wax+"):
            newline = "\r\n"
        return real(file, mode, buffering, encoding, errors, newline, *args, **kwargs)

    with patch("io.open", translating), patch("builtins.open", translating):
        yield


class PinProofPublicationTests(unittest.TestCase):
    def setUp(self):
        audit = ROOT / "target/pin-proof-publication-tests"
        audit.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Sidney Sissaoui")
        self.git("config", "user.email", "shideneyu@gmail.com")
        self.git("config", "commit.gpgsign", "false")
        self.objects = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse",
                                                "--path-format=absolute", "--git-path", "objects"],
                                               text=True).strip()
        borrow_objects(self.root, self.objects)
        self.directory = self.root / publication.DIRECTORY
        self.directory.mkdir(parents=True)
        for name in publication.SOURCES:
            (self.directory / name).write_bytes(publication.archived_bytes(publication.locator(name), ROOT))
        self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.PIPE)

    def commit(self):
        self.git("add", ".")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "synthetic publication fixture")

    def publish(self):
        publication.write(self.root)
        return self.root / publication.CHAIN

    @contextmanager
    def windows_paths(self):
        directory = PureWindowsPath(publication.DIRECTORY.as_posix())
        local = publication._local
        # Adapt only filesystem reads to the host; Git receives production argv.
        with patch.object(publication, "DIRECTORY", directory), \
                patch.object(publication, "CHAIN", directory / "publications.json"), \
                patch.object(publication, "_local", side_effect=lambda repository, path, *args:
                    local(repository, Path(path.as_posix()), *args)):
            yield

    def test_windows_publication_log_discovers_real_history(self):
        self.publish()
        self.commit()
        commit = self.git("rev-parse", "HEAD").strip()
        expected = publication.CHAIN.as_posix()
        with self.windows_paths(), patch.object(publication, "_git", wraps=publication._git) as git:
            history = publication._history(self.root)
            self.assertEqual(git.call_args.args[-1], expected)
            self.assertEqual(history, [commit])

    def test_windows_publication_show_reads_real_history_object(self):
        self.publish()
        self.commit()
        commit = self.git("rev-parse", "HEAD").strip()
        expected = commit.decode() + ":" + publication.CHAIN.as_posix()
        # Isolate show from discovery: a broken log must not hide this defect.
        with self.windows_paths(), patch.object(publication, "_history", return_value=[commit]), \
                patch.object(publication, "_git", wraps=publication._git) as git:
            self.assertEqual(publication.validate(self.root), publication.document())
            self.assertIn(unittest.mock.call(self.root, "show", expected), git.call_args_list)

    def test_windows_publication_rev_parse_binds_all_original_blobs(self):
        self.publish()
        expected = [publication.ORIGIN + ":" + (publication.DIRECTORY / name).as_posix()
                    for name in publication.SOURCES]
        # No chain commit: validate reaches rev-parse independently of log/show.
        with self.windows_paths(), patch.object(publication, "_history", return_value=[]), \
                patch.object(publication, "_git", wraps=publication._git) as git:
            self.assertEqual(publication.validate(self.root), publication.document())
            self.assertEqual([call.args[2] for call in git.call_args_list
                              if call.args[1] == "rev-parse"], expected)

    def test_windows_real_originals_read_from_git_object_paths(self):
        # The real-publication check reads each original with its own git show;
        # on Windows str(DIRECTORY / name) has backslashes and Git finds no blob.
        with self.windows_paths():
            for name in publication.SOURCES:
                self.assertEqual(original_bytes(name),
                                 publication.archived_bytes(publication.locator(name), ROOT))

    def test_windows_publication_rejects_removed_and_rewritten_history(self):
        chain = self.publish()
        good = chain.read_bytes()
        self.commit()
        with self.windows_paths():
            publication.validate(self.root)
            chain.unlink()
            for reader in (publication.proof_bytes, publication.write):
                with self.subTest(reader=reader.__name__), self.assertRaisesRegex(
                        ValueError, "chain removed"):
                    reader(self.root)
            chain.write_bytes(publication.encode({"schema": publication.SCHEMA, "entries": []}))
            self.commit()
            chain.write_bytes(good)
            self.commit()
            with self.assertRaisesRegex(ValueError, "history rewritten or removed"):
                publication.validate(self.root)

    def test_generation_preserves_originals_and_is_idempotent(self):
        original = publication.proof_bytes(self.root)
        chain = self.publish()
        self.assertEqual(publication.proof_bytes(self.root), original)
        before = {p.name: p.read_bytes() for p in self.directory.iterdir()}
        publication.write(self.root)
        self.assertEqual({p.name: p.read_bytes() for p in self.directory.iterdir()}, before)
        self.assertEqual(json.loads(chain.read_bytes()), publication.document())
        private = self.root / publication.ORIGINALS / publication.ATTRIBUTION
        for name, source in publication.SOURCES.items():
            raw = (private / name).read_bytes()
            self.assertEqual(len(raw), source["bytes"])
            self.assertEqual(hashlib.sha256(raw).hexdigest(), source["sha256"])
            self.assertEqual(raw, publication.archived_bytes(publication.locator(name), ROOT))
            self.assertNotEqual((self.directory / name).read_bytes(), raw)
        self.assertEqual(json.loads(original)["reviewer_role"], "R2 independent scope proof")
        self.assertEqual(json.loads(chain.read_bytes())["entries"][0]["publisher"], "S1")

    def test_each_declared_locator_rejects_corruption_deletion_and_escape(self):
        self.publish()
        for name in publication.SOURCES:
            path = self.directory / name
            good = path.read_bytes()
            for mode in ("corrupt", "missing", "escape"):
                with self.subTest(artifact=name, mode=mode):
                    if mode == "corrupt":
                        path.write_bytes(good + b"tampered")
                    else:
                        path.unlink()
                        if mode == "escape":
                            path.symlink_to(ROOT / "README.md")
                    with self.assertRaises(ValueError):
                        publication.proof_bytes(self.root)
                    if path.is_symlink():
                        path.unlink()
                    path.write_bytes(good)
        publication.validate(self.root)

    def test_chain_cannot_be_removed_resealed_or_rewritten_in_committed_history(self):
        chain = self.publish()
        good = chain.read_bytes()
        self.commit()
        for mode in ("missing", "tail", "author", "source", "hash", "duplicate"):
            with self.subTest(mode=mode):
                if mode == "missing":
                    chain.unlink()
                else:
                    value = json.loads(good)
                    if mode == "tail":
                        value["entries"] = []
                    elif mode == "author":
                        value["entries"][0]["publisher"] = "R2"
                    elif mode == "source":
                        value["entries"][0]["artifacts"][0]["source"]["sha256"] = "0" * 64
                    elif mode == "hash":
                        value["entries"][0]["sha256"] = "0" * 64
                    chain.write_bytes(publication.encode(value))
                    if mode == "duplicate":
                        chain.write_bytes(good.replace(b'"entries":', b'"entries": [], "entries":', 1))
                with self.assertRaises(ValueError):
                    publication.proof_bytes(self.root)
                chain.write_bytes(good)
        chain.write_bytes(publication.encode({"schema": publication.SCHEMA, "entries": []}))
        self.commit()
        chain.write_bytes(good)
        self.commit()
        with self.assertRaisesRegex(ValueError, "history rewritten or removed"):
            publication.proof_bytes(self.root)

    def test_generator_refuses_to_replace_changed_original(self):
        path = self.directory / "PROOF.json"
        path.write_bytes(path.read_bytes() + b"tampered")
        with self.assertRaisesRegex(ValueError, "original differs"):
            publication.write(self.root)
        self.assertFalse((self.root / publication.CHAIN).exists())

    def test_alternates_survive_windows_text_newlines(self):
        # On Windows a text-mode alternates file ends with CR: Git looks for
        # '.../objects\r' and every history lookup reports it unavailable.
        probe = self.root / "probe"
        with windows_text_newlines():
            probe.write_text("probe\n")
            borrow_objects(self.root, self.objects)
        # The emulation must reach Path.write_text, or this test proves nothing.
        self.assertEqual(probe.read_bytes(), b"probe\r\n")
        probe.unlink()
        self.publish()
        self.commit()
        self.assertEqual(publication.validate(self.root), publication.document())

    def test_missing_historical_objects_fail_closed(self):
        self.publish()
        (self.root / ".git/objects/info/alternates").unlink()
        with self.assertRaises(ValueError):
            publication.proof_bytes(self.root)

    def test_real_publication_authenticates_each_original_and_keeps_pin_chain_exact(self):
        publication.validate(ROOT)
        for name in publication.SOURCES:
            self.assertEqual(original_bytes(name), publication.archived_bytes(publication.locator(name), ROOT))
        path = "benches/perf/qualification/pin-revisions.json"
        self.assertEqual((ROOT / path).read_bytes(), subprocess.check_output([
            "git", "-C", str(ROOT), "show", "3dd786b0b5975d9449993ed1e746d4bf805a14b3:" + path]))


if __name__ == "__main__":
    unittest.main()
