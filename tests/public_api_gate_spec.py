"""Fast selection tests: python3 -m unittest discover -s tests -p public_api_gate_spec.py."""

import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from public_api_policy import FACADES, cells, require_complete
from public_api_consumers import compiler_inputs

TARGET = "x86_64-unknown-linux-gnu"


def metadata():
    return {"workspace_members": list(FACADES), "packages": [
        {"name": name, "id": name, "features": copy.deepcopy(features),
         "targets": [{"name": name.replace("-", "_"), "kind": ["lib"]}],
         "dependencies": [{"name": "rmux-sdk", "kind": None, "uses_default_features": True,
                           "features": [], "optional": False, "target": None}]}
        for name, features in FACADES.items()
    ]}


class SelectionSpec(unittest.TestCase):
    def test_full_powerset_and_distinct_default_all_commands(self):
        result = cells(metadata(), TARGET)
        self.assertEqual(len(result), 13)
        sdk = [c for c in result if c.package == "rmux-sdk"]
        self.assertEqual([c.features for c in sdk[:8]], [
            (), ("crossterm",), ("regex",), ("web",),
            ("crossterm", "regex"), ("crossterm", "web"), ("regex", "web"),
            ("crossterm", "regex", "web"),
        ])
        self.assertEqual(sdk[6].flags, ["--no-default-features", "--features", "regex,web"])
        self.assertEqual(sdk[8].flags, [])
        self.assertEqual(sdk[9].flags, ["--all-features"])
        self.assertEqual(len({c.id for c in result}), 13)
        require_complete(result, [c.id for c in result])

    def test_missing_crate_feature_drift_or_library_is_not_green(self):
        variants = [metadata() for _ in range(5)]
        variants[0]["packages"].pop()
        variants[1]["packages"][0]["features"]["new_feature"] = []
        variants[2]["packages"][0]["features"]["default"] = []
        variants[3]["packages"][1]["targets"] = []
        variants[4]["packages"][1]["dependencies"][0]["uses_default_features"] = False
        for value in variants:
            with self.subTest(value=value), self.assertRaises(ValueError):
                cells(value, TARGET)

    def test_invalid_omitted_duplicated_and_empty_selection_fail(self):
        ids = [c.id for c in cells(metadata(), TARGET)]
        self.assertEqual(len(cells(metadata(), TARGET, list(reversed(ids)))), 13)
        for selection in ([], ids[:-1], ids + [ids[0]], ["rmux-proto/default"],
                          ids[:-1] + [ids[0]]):
            with self.subTest(selection=selection), self.assertRaises(ValueError):
                cells(metadata(), TARGET, selection)
        with self.assertRaises(ValueError):
            cells(metadata(), "made-up-target")

    def test_missing_execution_cannot_pass_cardinality(self):
        required = cells(metadata(), TARGET)
        ids = [c.id for c in required]
        for completed in ([], ids[:-1], ids[:-1] + [ids[0]]):
            with self.subTest(completed=completed), self.assertRaises(ValueError):
                require_complete(required, completed)

    def test_missing_or_ambiguous_compiler_artifact_fails(self):
        import json
        artifact = json.dumps({"reason": "compiler-artifact", "target": {"name": "rmux_sdk"},
                               "filenames": ["/owned/librmux_sdk.rmeta"]})
        for output in ("", artifact + "\n" + artifact):
            with self.subTest(output=output), self.assertRaises(ValueError):
                compiler_inputs(output, [("rmux_sdk", "rmux_sdk")], ".rmeta")

    def test_native_explicit_target_preserves_host_proc_macros(self):
        import json
        output = "\n".join(json.dumps({"reason": "compiler-artifact", "target": {"name": name},
                                       "filenames": [path]}) for name, path in [
            ("rmux_sdk", "/owned/target/native/debug/deps/libsdk.rmeta"),
            ("tokio_macros", "/owned/target/debug/deps/libtokio_macros.so"),
        ])
        command = compiler_inputs(output, [("rmux_sdk", "rmux_sdk")], ".rmeta")
        self.assertIn(f"dependency={Path('/owned/target/native/debug/deps')}", command)
        self.assertIn(f"dependency={Path('/owned/target/debug/deps')}", command)


if __name__ == "__main__":
    unittest.main()
