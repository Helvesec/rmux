#!/usr/bin/env python3
"""Negative canaries for the real external cutoff gate (no product mutation)."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("cutoff", ROOT / "scripts/bench/check-perf-cutoff.py")
cutoff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cutoff)


class CutoffCanaries(unittest.TestCase):
    def test_current_product_is_clean(self):
        self.assertGreater(cutoff.check_product_cutoff()["files_checked"], 1)

    def test_escaped_manifest_markers_cannot_restore_retired_controls(self):
        manifests = ("Cargo.toml", "crates/rmux-pty/Cargo.toml")
        markers = (cutoff.INITIAL_ENV, cutoff.LEGACY_ENV,
                   "initial_trace", "initial-trace", "perf-instrument")
        for name in manifests:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "Cargo.toml").write_text('[features]\n')
                manifest = root / name
                manifest.parent.mkdir(parents=True, exist_ok=True)
                manifest.write_text('[features]\n"safe\\u002dfeature" = []\n')
                cutoff.check_product_cutoff(root)
                for marker in markers:
                    escaped = f"\\u{ord(marker[0]):04x}" + marker[1:]
                    declarations = (
                        f'[features]\n"{escaped}" = []\n',
                        f'[dependencies]\nfixture = {{version="1", features=["{escaped}"]}}\n',
                    )
                    for declaration in declarations:
                        with self.subTest(manifest=name, declaration=declaration):
                            manifest.write_text(declaration)
                            self.assertNotIn(marker.encode(), manifest.read_bytes())
                            with self.assertRaisesRegex(RuntimeError, "product instrumentation exposure"):
                                cutoff.check_product_cutoff(root)

    def test_root_build_script_cannot_restore_retired_instrumentation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nname = "fixture"\nbuild = "build.rs"\n')
            script = root / "build.rs"
            script.write_text("fn main() {}\n")
            cutoff.check_product_cutoff(root)
            for marker in (cutoff.INITIAL_ENV, cutoff.LEGACY_ENV, "perf-instrument"):
                with self.subTest(marker=marker):
                    script.write_text(f'fn main() {{ let _ = "{marker}"; }}\n')
                    with self.assertRaisesRegex(RuntimeError, "product instrumentation exposure.*build.rs"):
                        cutoff.check_product_cutoff(root)

    def test_reintroduced_exposures_fail_the_actual_gate(self):
        mutations = {
            "crates/rmux-pty/src/lib.rs": "pub mod initial_trace;",
            "crates/rmux-server/src/listener.rs": 'let _ = std::env::var("RMUX_EXAMPLE_INITIAL_TRACE_CONFIG");',
            "crates/rmux-server/Cargo.toml": '[features]\ninitial-trace-fixture = []\n',
            "crates/rmux-pty/Cargo.toml": '[features]\nperf-instrument = []\n',
            "resources/initial-trace-fixture.json": "{}",
        }
        for name, content in mutations.items():
            with self.subTest(exposure=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "Cargo.toml").write_text('[features]\n')
                cutoff.check_product_cutoff(root)
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
                with self.assertRaisesRegex(RuntimeError, "product instrumentation exposure"):
                    cutoff.check_product_cutoff(root)


if __name__ == "__main__":
    unittest.main()
