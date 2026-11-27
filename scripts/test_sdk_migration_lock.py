"""Keep the standalone migration consumer locked to this checkout's packages."""

import unittest
from pathlib import Path

from toml_reader import loads


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "crates/rmux-sdk/tests/fixtures/sdk-migration"
LOCAL_PACKAGES = (
    "rmux-bootstrap", "rmux-ipc", "rmux-os", "rmux-proto", "rmux-sdk", "rmux-types",
)


def read_toml(path):
    return loads(path.read_text(encoding="utf-8"))


class MigrationLock(unittest.TestCase):
    def test_local_package_identities_match_path_manifests(self):
        workspace = read_toml(ROOT / "Cargo.toml")["workspace"]["package"]
        packages = read_toml(FIXTURE / "Cargo.lock")["package"]
        for name in LOCAL_PACKAGES:
            with self.subTest(package=name):
                manifest = read_toml(ROOT / "crates" / name / "Cargo.toml")["package"]
                version = manifest["version"]
                if isinstance(version, dict):
                    self.assertEqual(version, {"workspace": True})
                    version = workspace["version"]
                locked = [package for package in packages if package["name"] == name]
                self.assertEqual(len(locked), 1, f"one local identity required for {name}")
                self.assertNotIn("source", locked[0], f"{name} must resolve by path")
                self.assertEqual(locked[0]["version"], version)


if __name__ == "__main__":
    unittest.main()
