"""Retained package fixtures keep each invocation and its earlier evidence apart."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

import local_install_cycle_spec
import package_layout_spec
import unix_installer_spec


class RetainedFixtureIsolation(unittest.TestCase):
    def test_each_invocation_owns_a_fresh_tree_and_preserves_previous_bytes(self):
        label = "same" + os.sep + "retained-case"
        prefix = label.replace(os.sep, "-")
        for module in (package_layout_spec, unix_installer_spec):
            with self.subTest(helper=module.__name__):
                with tempfile.TemporaryDirectory() as directory:
                    parent = Path(directory) / "retained"
                    legacy = parent / prefix
                    legacy.mkdir(parents=True)
                    (legacy / "old-evidence").write_bytes(b"original\x00evidence\xff\n")
                    expected = {legacy / "old-evidence": b"original\x00evidence\xff\n"}
                    roots = []

                    def remember(root, cleanup):
                        self.assertIsNone(cleanup)
                        self.assertEqual(root.parent.resolve(), parent.resolve())
                        self.assertTrue(root.name.startswith(prefix + "-"), root)
                        self.assertNotIn(root, [legacy, *roots], "retained roots must be unique")
                        self.assertEqual(list(root.iterdir()), [], "new fixture must start empty")
                        for path, body in expected.items():
                            self.assertEqual(path.read_bytes(), body, path)
                        body = f"fixture {len(roots)}\n".encode() + bytes(range(256))
                        marker = root / "evidence"
                        marker.write_bytes(body)
                        expected[marker] = body
                        roots.append(root)

                    with mock.patch.dict(os.environ, RMUX_TEST_FIXTURE_ROOT=str(parent)):
                        # Repeated calls must not reopen either the old label path
                        # or the immediately preceding invocation's returned tree.
                        remember(*module.fixture_root(label))
                        remember(*module.fixture_root(label))

                        barrier = threading.Barrier(8)

                        def allocate(_):
                            barrier.wait(timeout=30)
                            return module.fixture_root(label)

                        with ThreadPoolExecutor(max_workers=8) as executor:
                            concurrent = list(executor.map(allocate, range(8)))
                        for root, cleanup in concurrent:
                            remember(root, cleanup)

                        # Separate interpreters cannot share an in-memory counter
                        # or lock as their only source of uniqueness.
                        code = (
                            "import importlib,json,os,sys; "
                            "root,cleanup=importlib.import_module(sys.argv[1]).fixture_root(sys.argv[2]); "
                            "assert cleanup is None; "
                            "print(json.dumps([str(root),os.getpid()]))"
                        )

                        def process(_):
                            result = subprocess.run(
                                [sys.executable, "-B", "-c", code, module.__name__, label],
                                cwd=Path(__file__).resolve().parent,
                                capture_output=True, text=True, timeout=30, check=True)
                            return json.loads(result.stdout)

                        with ThreadPoolExecutor(max_workers=4) as executor:
                            separate = list(executor.map(process, range(4)))
                        self.assertEqual(len({pid for _, pid in separate}), 4)
                        for root, _ in separate:
                            remember(Path(root), None)
                        self.assertEqual(set(parent.iterdir()), {legacy, *roots})
                        for path, body in expected.items():
                            self.assertEqual(path.read_bytes(), body, path)

                # The ordinary mode still owns a TemporaryDirectory finalizer.
                with mock.patch.dict(os.environ):
                    os.environ.pop("RMUX_TEST_FIXTURE_ROOT", None)
                    root, cleanup = module.fixture_root(label)
                    self.assertTrue(callable(cleanup))
                    try:
                        self.assertTrue(root.is_dir())
                        (root / "ordinary").write_bytes(b"temporary\n")
                        other, other_cleanup = module.fixture_root(label)
                        try:
                            self.assertNotEqual(root, other)
                            self.assertEqual(list(other.iterdir()), [])
                        finally:
                            other_cleanup()
                        self.assertFalse(other.exists())
                    finally:
                        cleanup()
                    self.assertFalse(root.exists())


class RetainedZipCollectionIsolation(unittest.TestCase):
    """ZipCollection obtains its retained tree from the shared fixture_root."""

    NAME = "test_windows_zip_fixture_collects_the_real_descriptor_population"

    def run_case(self):
        case = local_install_cycle_spec.ZipCollection(self.NAME)
        result = unittest.TestResult()
        case.run(result)
        self.assertEqual(result.errors + result.failures, [], case.id())
        self.assertEqual(result.testsRun, 1)
        return case.root

    def test_repeated_retained_runs_never_reuse_an_earlier_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory) / "retained"
            case_id = local_install_cycle_spec.ZipCollection(self.NAME).id()
            # A tree left by an earlier run under the legacy per-id path must
            # neither leak into the new check-tree nor be rewritten.
            legacy = parent / case_id
            stale = legacy / "windows-extracted" / "stale-package-file"
            stale.parent.mkdir(parents=True)
            stale.write_bytes(b"earlier run\n")
            with mock.patch.dict(os.environ, RMUX_TEST_FIXTURE_ROOT=str(parent)):
                first = self.run_case()
                archive = first / "windows.zip"
                before = (archive.stat().st_ino, archive.read_bytes())
                second = self.run_case()
            self.assertEqual(first.parent.resolve(), parent.resolve())
            self.assertEqual(second.parent.resolve(), parent.resolve())
            self.assertNotEqual(first, second)
            self.assertNotIn(legacy, (first, second))
            self.assertEqual(stale.read_bytes(), b"earlier run\n")
            self.assertEqual((archive.stat().st_ino, archive.read_bytes()), before)
            self.assertTrue((second / "windows.zip").is_file())

    def test_ordinary_run_removes_its_temporary_tree(self):
        with mock.patch.dict(os.environ):
            os.environ.pop("RMUX_TEST_FIXTURE_ROOT", None)
            root = self.run_case()
        self.assertFalse(root.exists())


if __name__ == "__main__":
    unittest.main()
