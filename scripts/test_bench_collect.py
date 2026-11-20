"""Exercise the real collector/renderer boundary using private synthetic artifacts."""

import csv
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
REPORT = Path(".rmux-audit/benchmarks/report.md")
ASSETS = Path(".rmux-audit/benchmarks/assets")
GUARD = "benchmark output must be outside source content or under .rmux-audit"
NO_JSON = "no benchmark JSON artifacts found in target/benchmarks"


class CollectorRenderer(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".rmux-audit" / "bench-collect-tests"
        audit.mkdir(parents=True, exist_ok=True)
        # Retain fixtures for inspection; never write results into the real docs.
        self.fixture = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=audit))
        self.repo = self.fixture / "repo"
        for relative in ("scripts/bench/collect.py", "scripts/bench/render.py",
                         "docs/benchmarks.md"):
            target = self.repo / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)
        self.original_docs = self.docs_snapshot()
        self.source = self.repo / "target/benchmarks/linux.json"
        self.source.parent.mkdir(parents=True)
        self.source.write_text(json.dumps({
            "platform": {"id": "linux", "name": "Synthetic collector fixture"},
            "tools": ["rmux"],
            "operations": [{"id": "list_sessions_default", "metrics": {
                "rmux": {"p50_ms": 1.25, "p95_ms": 1.5, "samples_ms": [1.0, 1.5]}
            }}],
        }), encoding="utf-8")

    def docs_snapshot(self):
        return {str(path.relative_to(self.repo)): path.read_bytes()
                for path in (self.repo / "docs").rglob("*") if path.is_file()}

    def tearDown(self):
        self.assertEqual(self.docs_snapshot(), self.original_docs)
        self.assertFalse((self.repo / "docs/benchmarks").exists())

    def collect(self, *args):
        command = [sys.executable, "scripts/bench/collect.py", "--local-platform", "none", *args]
        result = subprocess.run(command, cwd=self.repo, capture_output=True,
                                text=True, timeout=20)
        receipt = {"command": command, "cwd": str(self.repo), "exit": result.returncode,
                   "stdout": result.stdout, "stderr": result.stderr}
        (self.fixture / "receipt.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
        return result

    def assert_rendered(self, result, report, assets):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        text = report.read_text(encoding="utf-8")
        self.assertIn("Synthetic collector fixture", text)
        self.assertIn("List sessions", text)
        self.assertIn("1.250 ms", text)
        rows = list(csv.DictReader(io.StringIO((assets / "linux.csv").read_text(encoding="utf-8"))))
        self.assertEqual([row["sample_ms"] for row in rows], ["1.0", "1.5"])
        self.assertTrue(all(row["tool"] == "rmux" and row["operation"] == "list_sessions_default"
                            for row in rows))
        self.assertEqual(list(assets.iterdir()), [assets / "linux.csv"])

    def assert_no_rendered_files(self):
        self.assertFalse((self.repo / REPORT).exists())
        self.assertFalse((self.repo / ASSETS).exists())
        self.assertEqual(list(self.repo.rglob("*.csv")), [])

    def test_omitted_destinations_render_private_markdown_and_raw_csv(self):
        self.assert_rendered(self.collect(), self.repo / REPORT, self.repo / ASSETS)

    def test_only_output_override_keeps_assets_private(self):
        report = self.fixture / "external-report.md"
        self.assert_rendered(self.collect("--output", str(report)), report, self.repo / ASSETS)
        self.assertFalse((self.repo / REPORT).exists())

    def test_only_asset_override_keeps_report_private(self):
        assets = self.fixture / "external-assets"
        self.assert_rendered(self.collect("--asset-dir", str(assets)), self.repo / REPORT, assets)
        self.assertFalse((self.repo / ASSETS).exists())

    def test_both_external_destinations_are_accepted(self):
        report, assets = self.fixture / "external-report.md", self.fixture / "external-assets"
        self.assert_rendered(self.collect("--output", str(report), "--asset-dir", str(assets)),
                             report, assets)
        self.assert_no_rendered_files()

    def test_docs_output_is_refused_before_writes(self):
        result = self.collect("--output", "docs/benchmarks.md", "--asset-dir", str(self.fixture / "assets"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(GUARD, result.stderr)
        self.assertFalse((self.fixture / "assets").exists())
        self.assert_no_rendered_files()

    def test_docs_assets_are_refused_before_writes(self):
        report = self.fixture / "external-report.md"
        result = self.collect("--output", str(report), "--asset-dir", "docs/benchmarks")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(GUARD, result.stderr)
        self.assertFalse(report.exists())
        self.assert_no_rendered_files()

    def test_no_render_does_not_invoke_renderer(self):
        result = self.collect("--no-render")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assert_no_rendered_files()

    def test_dry_run_prints_renderer_without_writing(self):
        result = self.collect("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("scripts/bench/render.py target/benchmarks/linux.json", result.stdout)
        self.assert_no_rendered_files()

    def test_absent_json_fails_before_renderer(self):
        self.source.unlink()
        result = self.collect()
        self.assertEqual(result.returncode, 1)
        self.assertIn(NO_JSON, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assert_no_rendered_files()

    def test_absent_json_still_fails_in_dry_run(self):
        self.source.unlink()
        result = self.collect("--dry-run")
        self.assertEqual(result.returncode, 1)
        self.assertIn(NO_JSON, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assert_no_rendered_files()

    def test_no_render_accepts_absent_json(self):
        self.source.unlink()
        result = self.collect("--no-render")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assert_no_rendered_files()


if __name__ == "__main__":
    unittest.main()
