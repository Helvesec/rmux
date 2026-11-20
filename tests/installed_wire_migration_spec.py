"""Installed migration evidence must be executable, attributable and complete."""
import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import installed_consumer_diagnostics as diagnostics


class MigrationEvidence(unittest.TestCase):
    def test_refusal_requires_diagnostic_exit_restart_and_installed_origin(self):
        validate = getattr(diagnostics, "validate_wire_refusal", None)
        self.assertIsNotNone(validate, "installed wire migration has no refusal evidence validator")
        client = {"path": "/private/prefix/bin/rmux", "sha256": "a" * 64,
                  "installed_path": "/private/prefix/bin/rmux", "installed_sha256": "a" * 64}
        record = {"client": client, "command": [client["path"], "-S", "/private/socket", "list-sessions"],
                  "returncode": 1, "stderr": "rmux: incompatible protocol; run old rmux -S /private/socket kill-server; sessions will be lost",
                  "diagnostic": "incompatible protocol", "expected_returncode": 1,
                  "restart": {"command": ["/private/old/bin/rmux", "-S", "/private/socket", "kill-server"],
                              "returncode": 0, "explicit": True, "sessions_lost_acknowledged": True},
                  "before": {"pid": 123, "sessions": ["kept"]},
                  "after_refusal": {"pid": 123, "sessions": ["kept"]}}
        self.assertIsNone(validate(record))
        for key in ("stderr", "returncode", "restart"):
            with self.subTest(missing=key):
                broken = copy.deepcopy(record)
                del broken[key]
                with self.assertRaises(ValueError):
                    validate(broken)
        for key, value in (("path", "/workspace/target/debug/rmux"), ("sha256", "b" * 64)):
            with self.subTest(origin=key):
                broken = copy.deepcopy(record)
                broken["client"][key] = value
                with self.assertRaises(ValueError):
                    validate(broken)
        for key, value in (("returncode", 0), ("stderr", ""), ("after_refusal", {"pid": 124, "sessions": []})):
            with self.subTest(invalid=key):
                broken = copy.deepcopy(record)
                broken[key] = value
                with self.assertRaises(ValueError):
                    validate(broken)


if __name__ == "__main__":
    unittest.main()
