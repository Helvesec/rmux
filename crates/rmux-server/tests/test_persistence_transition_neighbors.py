import pathlib
import tempfile
import unittest
from unittest import mock
from persistence_transition import validate_transition


class TransitionNeighbors(unittest.TestCase):
    def check_pair(self, transition, versions, valid=True, equal_bytes=False):
        with tempfile.TemporaryDirectory() as directory:
            paths = [pathlib.Path(directory).resolve(strict=True) / name for name in ("save", "restore")]
            paths[0].write_bytes(b"saving image")
            paths[1].write_bytes(b"saving image" if equal_bytes else b"restoring image")
            with mock.patch("persistence_transition.image_version", side_effect=versions) as query:
                if valid:
                    result = validate_transition(*paths, transition)
                    self.assertEqual(result["kind"], transition)
                    self.assertEqual(result["saving"]["version"], versions[0])
                    self.assertEqual(result["restoring"]["version"], versions[1])
                    self.assertEqual(query.call_count, 2)
                else:
                    with self.assertRaises(ValueError):
                        validate_transition(*paths, transition)
                    if equal_bytes or transition == "unrecognized":
                        query.assert_not_called()

    def test_explicit_version_directions_and_invalid_neighbors(self):
        for kind, versions in [("upgrade", [(0, 9, 0), (0, 10, 0)]),
                               ("rollback", [(0, 10, 0), (0, 9, 0)]),
                               ("same", [(0, 10, 0), (0, 10, 0)])]:
            with self.subTest(kind=kind):
                self.check_pair(kind, versions)
        for kind, versions in [("upgrade", [(0, 10, 0), (0, 9, 0)]),
                               ("rollback", [(0, 9, 0), (0, 10, 0)]),
                               ("upgrade", [(0, 10, 0), (0, 10, 0)]),
                               ("rollback", [(0, 10, 0), (0, 10, 0)]),
                               ("same", [(0, 9, 0), (0, 10, 0)])]:
            with self.subTest(kind=kind, versions=versions):
                self.check_pair(kind, versions, valid=False)
        self.check_pair("upgrade", [], valid=False, equal_bytes=True)
        self.check_pair("rollback", [], valid=False, equal_bytes=True)
        self.check_pair("unrecognized", [], valid=False)

    def test_legacy_invocation_requires_no_version_probe(self):
        with mock.patch("persistence_transition.image_version") as query:
            self.assertIsNone(validate_transition("not-read", "not-read", None))
            query.assert_not_called()

    def test_version_output_is_strict_and_numeric(self):
        from persistence_transition import image_version
        with mock.patch("persistence_transition.subprocess.check_output", return_value="rmux 0.10.0\n"):
            self.assertEqual(image_version(pathlib.Path("rmux")), (0, 10, 0))
        for text in ("rmux 0.10.0-beta", "tmux 3.7b", "rmux 00.10.0", "", "rmux 0.10.0\nextra"):
            with self.subTest(text=text), mock.patch("persistence_transition.subprocess.check_output", return_value=text):
                with self.assertRaises(ValueError):
                    image_version(pathlib.Path("rmux"))

    def test_image_changed_during_version_query_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            paths = [pathlib.Path(directory).resolve(strict=True) / name for name in ("save", "restore")]
            for index, path in enumerate(paths):
                path.write_text(str(index))
            def version(path):
                if path == paths[1]:
                    path.write_text("replaced")
                return (0, 9, 0) if path == paths[0] else (0, 10, 0)
            with mock.patch("persistence_transition.image_version", side_effect=version):
                with self.assertRaisesRegex(ValueError, "changed during qualification"):
                    validate_transition(*paths, "upgrade")


if __name__ == "__main__":
    unittest.main()
