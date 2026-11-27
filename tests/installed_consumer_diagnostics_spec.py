"""IC-1 regressions execute the real runner with synthetic compiler messages."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from installed_consumers import Runner


class DiagnosticAttributionSpec(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "logs").mkdir()
        self.runner = Runner.__new__(Runner)
        self.runner.output = self.root
        self.runner.env = {"PATH": "/usr/bin:/bin"}
        self.runner.receipt = {"commands": []}
        self.source = self.root / "src/main.rs"
        self.package = "path+file:///consumer#ratatui-consumer@0.0.0"
        self.item = "ratatui_rmux::PaneWidget"
        self.message = {"$message_type": "diagnostic", "level": "error",
            "code": {"code": "E0432"}, "message": f"unresolved import `{self.item}`",
            "spans": [{"is_primary": True, "file_name": "src/main.rs", "line_start": 3}]}

    def cargo(self, message):
        return {"reason": "compiler-message", "package_id": self.package, "message": message}

    def invoke(self, stdout="", stderr="", accepted=False, code=101, package=True,
               item=None, diagnostic="E0432", tail="pass", timeout=3):
        payload = f"import sys; sys.stdout.write({stdout!r}); sys.stdout.flush(); sys.stderr.write({stderr!r}); sys.stderr.flush(); {tail}; sys.exit({code})"
        args = {"reject": item or self.item, "diagnostic": diagnostic,
                "diagnostic_source": self.source,
                "diagnostic_package": self.package if package else None, "timeout": timeout}
        if accepted:
            self.runner.run("synthetic", [sys.executable, "-c", payload], self.root, **args)
            self.assertEqual(self.runner.receipt["commands"][-1]["status"], "pass")
        else:
            with self.assertRaises((ValueError, subprocess.TimeoutExpired)):
                self.runner.run("synthetic", [sys.executable, "-c", payload], self.root, **args)

    def test_source_echo_does_not_identify_the_rejected_import(self):
        value = copy.deepcopy(self.message)
        value["message"] = "unresolved import `ratatui_rmux::PaneDriver`"
        frozen = Path(__file__).resolve().parent / "fixtures/installed-consumers/ratatui.rs"
        echo = frozen.read_text().splitlines()[2]
        value["rendered"] = f"error[E0432]: {value['message']}\n3 | {echo}\n"
        value["spans"][0]["text"] = [{"text": echo}]
        self.invoke(stdout=json.dumps(self.cargo(value)))

    def test_cross_stream_token_cannot_complete_an_error(self):
        value = copy.deepcopy(self.message)
        value["message"] = "unresolved import `unrelated::Missing`"
        self.invoke(stdout=self.item, stderr=json.dumps(value), package=False)
        self.invoke(stdout=json.dumps(self.cargo(value)), stderr=self.item)

    def test_code_and_item_must_be_in_the_same_error(self):
        wrong_code = {**self.message, "code": {"code": "E0308"}}
        wrong_item = {**self.message, "message": "unresolved import `unrelated::Missing`"}
        self.invoke(stdout="\n".join(json.dumps(self.cargo(m)) for m in (wrong_code, wrong_item)))

    def test_wrong_package_or_primary_source_is_rejected(self):
        self.invoke(stdout=json.dumps({**self.cargo(self.message), "package_id": "registry+other#ratatui-consumer@0.0.0"}))
        for spans in ([{"is_primary": True, "file_name": "other/main.rs"}],
                      [{"is_primary": False, "file_name": "src/main.rs"}], []):
            value = {**self.message, "spans": spans}
            self.invoke(stdout=json.dumps(self.cargo(value)))

    def test_notes_and_substring_items_are_not_errors_for_the_item(self):
        self.invoke(stdout=json.dumps(self.cargo({**self.message, "level": "note"})))
        self.invoke(stdout=json.dumps(self.cargo({**self.message,
                    "message": "unresolved import `ratatui_rmux::PaneWidgetOther`"})))
        parent = {**self.message, "message": "unresolved import `other::Missing`", "children": [self.message]}
        self.invoke(stdout=json.dumps(self.cargo(parent)))

    def test_direct_rustc_requires_its_diagnostic_stream(self):
        self.invoke(stderr=json.dumps(self.message), package=False, accepted=True)
        self.invoke(stdout=json.dumps(self.message), package=False)
        self.invoke(stderr=json.dumps(self.cargo(self.message)), package=False)

    def test_correct_cargo_import_private_struct_and_missing_module(self):
        for code, item, message in (
            ("E0432", self.item, self.message["message"]),
            ("E0603", "SessionName", "struct `SessionName` is private"),
            ("E0583", "cell", "file not found for module `cell`"),
            ("E0583", "widget", "file not found for module `widget`"),
        ):
            value = {**self.message, "code": {"code": code}, "message": message}
            self.invoke(stdout=json.dumps(self.cargo(value)), item=item, diagnostic=code, accepted=True)

    def test_empty_malformed_and_unstructured_failure_is_rejected(self):
        for value in ("", "{broken JSON", "[]", f"error[E0432]: {self.message['message']}"):
            self.invoke(stdout=value, stderr=value)

    def test_green_signal_and_timeout_do_not_pass_with_a_valid_diagnostic(self):
        value = json.dumps(self.cargo(self.message))
        self.invoke(stdout=value, code=0)
        self.invoke(stdout=value, tail="import os,signal; os.kill(os.getpid(),signal.SIGTERM)")
        self.invoke(stdout=value, tail="import time; time.sleep(5)", timeout=0.05)


if __name__ == "__main__":
    unittest.main()
