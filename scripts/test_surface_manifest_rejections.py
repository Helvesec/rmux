"""Adversarial witnesses for the source projection and its retained exports."""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import surface_manifest as manifest
from surface_sources import CORE, OUTER, PROTO, Sources
from surface_syntax import parse


ROOT = manifest.ROOT


class ProjectionRejections(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = manifest.project()

    def record(self, value, family):
        return next(r for r in value["entries"] if r["family"] == family and r["classification"] == "stable")

    def test_duplicate_orphan_unclassified_and_uncovered_are_rejected_in_every_family(self):
        for family in sorted(manifest.FAMILIES):
            for defect, message in (("duplicate", "duplicate"), ("orphan", "orphan"),
                                    ("unclassified", "unclassified"), ("uncovered", "without conformance")):
                with self.subTest(family=family, defect=defect):
                    value = copy.deepcopy(self.original)
                    row = self.record(value, family)
                    if defect == "duplicate":
                        value["entries"].append(copy.deepcopy(row))
                        value["entries"].sort(key=lambda r: (r["family"], r["id"]))
                    elif defect == "orphan":
                        row["id"] = "not-a-registered-surface"
                        value["entries"].sort(key=lambda r: (r["family"], r["id"]))
                    elif defect == "unclassified":
                        row["classification"] = "unreviewed"
                    else:
                        row["conformance"] = []
                    with self.assertRaisesRegex(ValueError, message):
                        manifest.check(value)

    def test_missing_identity_family_metadata_input_and_noncanonical_order_are_refused(self):
        for defect in ("identity", "family", "metadata", "input", "order"):
            with self.subTest(defect=defect):
                value = copy.deepcopy(self.original)
                if defect == "identity":
                    value["entries"].pop()
                elif defect == "family":
                    value["entries"] = [r for r in value["entries"] if r["family"] != "personalities"]
                elif defect == "metadata":
                    value["entries"][0]["metadata"] = {}
                elif defect == "input":
                    value["inputs"].pop(next(iter(value["inputs"])))
                else:
                    value["entries"].reverse()
                with self.assertRaises(ValueError):
                    manifest.check(value)

    def test_stable_test_reference_must_name_an_existing_nonignored_case(self):
        for defect in ("file", "case", "helper"):
            with self.subTest(defect=defect):
                value = copy.deepcopy(self.original)
                row = self.record(value, "capabilities")
                if defect == "file":
                    row["conformance"][0]["path"] = "tests/absent.rs"
                elif defect == "case":
                    row["conformance"][0]["case"] = "absent_case"
                else:
                    row["conformance"] = [dict(path="scripts/surface_manifest.py", case="project")]
                with self.assertRaisesRegex(ValueError, "orphan"):
                    manifest.check(value)

    def test_actual_export_round_trips_and_cli_propagates_each_rejection(self):
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="surface-export-", dir=audit) as directory:
            path = Path(directory) / "manifest.json"
            argv = [sys.executable, "-B", "scripts/surface_manifest.py", "--check", "--manifest", str(path)]
            path.write_text(manifest.canonical(self.original))
            result = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            for defect in ("duplicate", "orphan", "unclassified", "uncovered", "json-key"):
                with self.subTest(defect=defect):
                    value = copy.deepcopy(self.original)
                    row = self.record(value, "commands")
                    if defect == "duplicate":
                        value["entries"].insert(0, copy.deepcopy(row))
                    elif defect == "orphan":
                        row["sources"] = ["missing-registry.rs"]
                    elif defect == "unclassified":
                        row.pop("classification")
                    elif defect == "uncovered":
                        row["conformance"] = []
                    raw = manifest.canonical(value)
                    if defect == "json-key":
                        raw = raw.replace('"schema": 1', '"schema": 1, "schema": 1')
                    path.write_text(raw)
                    result = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                    self.assertIn("surface manifest:", result.stderr)

    @unittest.skipUnless(os.name == "posix", "Unix gate dispatch fixture")
    def test_linux_fast_gate_dispatches_both_checks_and_propagates_their_failures(self):
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="surface-gate-", dir=audit) as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            gate = scripts / "gate-unix-fast.sh"
            shutil.copyfile(ROOT / "scripts/gate-unix-fast.sh", gate)
            python = root / "python-stub"
            python.write_text('#!/bin/sh\ncase "$*" in\n'
                              '*surface_manifest.py*) printf "manifest\\n" >> "$SURFACE_CALLS"; '
                              '[ "$SURFACE_FAIL" != manifest ] || exit 37;;\n'
                              '*test_surface*) printf "tests\\n" >> "$SURFACE_CALLS"; exit 39;;\n'
                              'esac\nexit 0\n')
            python.chmod(0o755)
            for failure, expected, calls in (("manifest", 37, ["manifest"]),
                                             ("tests", 39, ["manifest", "tests"])):
                observed = root / failure
                result = subprocess.run(["/bin/bash", str(gate), "--platform", "linux",
                                         "--test-threads", "1"], cwd=root, capture_output=True, text=True,
                                        env=dict(os.environ, RMUX_IDENTITY_HEAD="dispatch-fixture",
                                                 RMUX_PYTHON=str(python), SURFACE_FAIL=failure,
                                                 SURFACE_CALLS=str(observed)))
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                self.assertEqual(observed.read_text().splitlines(), calls)


class SourceRejections(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = manifest.project()

    def setUp(self):
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="surface-source-", dir=audit)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in self.original["inputs"]:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)

    def replace(self, path, before, after, message):
        file = self.root / path
        original = file.read_text()
        self.assertIn(before, original)
        file.write_text(original.replace(before, after, 1))
        try:
            with self.assertRaisesRegex(ValueError, message):
                manifest.project(self.root)
        finally:
            file.write_text(original)

    def test_command_aliases_local_and_delegated_neighbors_have_one_identity(self):
        path = CORE + "command_schema.rs"
        self.replace(path, 'Some("attach")', 'Some("bind")', "duplicate")
        self.replace(path, '"choose-tree -s"', '"absent-target -s"', "orphan")
        self.replace(path, 'Some("attach"), tmux,', 'Some("attach"), unknown,', "unclassified")
        by_id = {r["id"]: r for r in self.original["entries"] if r["family"] == "commands"}
        self.assertEqual(by_id["choose-session"]["metadata"]["target"], "choose-tree")
        self.assertEqual(by_id["diagnose"]["metadata"]["kind"], "local")
        self.assertEqual(by_id["capabilities"]["metadata"]["kind"], "extension")

    def test_options_typed_named_only_and_alias_paths_are_closed(self):
        server = CORE + "options/registry/table/server.rs"
        display = CORE + "options/registry/table/window_display.rs"
        self.replace(server, "OptionName::Backspace,", "OptionName::Missing,", "orphan")
        self.replace(server, '"backspace",', '"buffer-limit",', "duplicate")
        self.replace(display, 'name: "tree-mode-preview-format",', 'name: "backspace",', "duplicate")
        self.replace(display, "wire_option: None,", "wire_option: Some(OptionName::Backspace),", "unclassified")
        named = next(r for r in self.original["entries"] if r["family"] == "options"
                     and r["id"] == "tree-mode-preview-format")
        self.assertIsNone(named["metadata"]["wire"])

    def test_formats_time_enum_and_provider_neighbors_are_closed(self):
        path = CORE + "formats/registry.rs"
        self.replace(path, "BufferCreated, ClientActivity,", "MissingKey, ClientActivity,", "orphan")
        self.replace(path, '"active_window_index"', '"alternate_on"', "duplicate")
        self.replace(path, "contexts!(Session | Window), Runtime", "contexts!(Session | Window), Unknown", "unclassified")
        self.replace(path, "contexts!(Session | Window)", "contexts!(MissingContext)", "orphan")
        self.replace(path, "Some(FormatVariable::SessionName)", "None", "orphan")

    def test_wire_tags_dtos_status_and_fixture_neighbors_are_closed(self):
        path = PROTO + "src/frame_kind/request_commands.rs"
        self.replace(path, "c2s(0),", "c2s(1),", "duplicate")
        self.replace(path, '"NewSessionRequest",', '"MissingRequest",', "orphan")
        self.replace(path, "        ACTIVE,", "        FUTURE,", "unclassified")
        self.replace(PROTO + "src/frame_kind.rs", "Request::NewSession(_) => c2s(0)",
                     "Request::NewSession(_) => c2s(300)", "orphan wire helper")
        fixture_dir = self.root / "tests/wire-fixtures/ledger-v1-current-wire"
        (fixture_dir / "unregistered.bin").write_bytes(b"unused fixture")
        with self.assertRaisesRegex(ValueError, "orphan wire fixture"):
            manifest.project(self.root)

    def test_all_six_ledger_sections_and_reserved_identities_are_projected(self):
        ledger = PROTO + "src/frame_kind.rs"
        rows = [r for r in self.original["entries"] if r["family"] == "wire"]
        sections = (("request_commands", "c2s", 0, 56),
                    ("request_control", "c2s", 57, 92),
                    ("request_sdk", "c2s", 93, 140),
                    ("response_commands", "s2c", 0, 53),
                    ("response_control", "s2c", 54, 78),
                    ("response_sdk", "s2c", 79, 112))
        for name, direction, first, last in sections:
            path = PROTO + "src/frame_kind/" + name + ".rs"
            with self.subTest(section=name):
                selected = [r for r in rows if path in r["sources"]]
                expected = {f"{direction}:{tag:05d}" for tag in range(first, last + 1)}
                if name.endswith("_sdk"):
                    expected.update(f"{direction}:{tag:05d}" for tag in (32766, 32767))
                self.assertEqual({r["id"] for r in selected}, expected)
                self.assertIn(path, self.original["inputs"])
                self.assertTrue(all(ledger in r["sources"] for r in selected))
                # A source mutation must flow into both metadata and input hashes.
                source = self.root / path
                original = source.read_text()
                row = selected[0]
                note = row["metadata"]["note"]
                self.assertIn('"' + note + '"', original)
                source.write_text(original.replace('"' + note + '"', '"changed ledger note"', 1))
                try:
                    changed = manifest.project(self.root)
                    observed = next(r for r in changed["entries"]
                                    if r["family"] == "wire" and r["id"] == row["id"])
                    self.assertEqual(observed["metadata"]["note"], "changed ledger note")
                    self.assertNotEqual(changed["inputs"][path], self.original["inputs"][path])
                    with self.assertRaisesRegex(ValueError, "stale"):
                        manifest.check(self.original, self.root)
                finally:
                    source.write_text(original)
                source.unlink()
                try:
                    with self.assertRaisesRegex(ValueError, "orphan source"):
                        manifest.project(self.root)
                finally:
                    source.write_text(original)
        absent = [r for r in rows if r["classification"] == "absent"]
        self.assertEqual({r["id"] for r in absent},
                         {f"{d}:{t}" for d in ("c2s", "s2c") for t in (32766, 32767)})
        for row in absent:
            self.assertEqual(row["metadata"]["status"], "RESERVED")
            self.assertIsNone(row["metadata"]["fixture"])
            value = copy.deepcopy(self.original)
            value["entries"].remove(row)
            with self.subTest(missing=row["id"]), self.assertRaisesRegex(ValueError, "missing surface"):
                manifest.check(value)

    def test_ledger_assembly_and_section_mutations_fail_closed(self):
        path = PROTO + "src/frame_kind.rs"
        for before, after, message in (
                ("&assemble_ledger()", "&other_ledger()", "unsupported wire ledger initializer"),
                ("len += LEDGER_SECTIONS[section].len();", "len += 1;", "unsupported wire ledger length"),
                ("entries[offset] = source[index];", "entries[offset] = source[0];", "unsupported wire ledger assembly"),
                ("    request_control::ENTRIES,\n", "", "orphan wire DTO"),
                ("request_control::ENTRIES,", "request_commands::ENTRIES,", "duplicate wire ledger section"),
                ("request_control::ENTRIES,", "missing::ENTRIES,", "orphan.*module"),
                ("request_control::ENTRIES,", "request_control::OTHER,", "unsupported wire ledger section"),
                ("mod request_control;", '#[path = "other.rs"] mod request_control;', "orphan.*module")):
            with self.subTest(mutation=after):
                self.replace(path, before, after, message)

    def test_literal_ledger_neighbor_preserves_all_metadata_and_absent_rows(self):
        path = self.root / (PROTO + "src/frame_kind.rs")
        names = ("request_commands", "request_control", "request_sdk",
                 "response_commands", "response_control", "response_sdk")
        bodies = [(path.with_suffix("") / (name + ".rs")).read_text()
                  .split("= &[", 1)[1].rsplit("];", 1)[0] for name in names]
        path.write_text(path.read_text().replace("&assemble_ledger()", "&[" + "".join(bodies) + "]"))
        literal = manifest.project(self.root)
        expected = copy.deepcopy(self.original["entries"])
        for row in expected:
            if row["family"] == "wire":
                row["sources"] = [p for p in row["sources"] if "/frame_kind/" not in p]
        self.assertEqual(literal["entries"], expected)

    def test_capability_modes_policy_and_cli_supplement_are_closed(self):
        path = PROTO + "src/capabilities/registry.rs"
        self.replace(path, '"scene.window.v1"', '"rpc.detached"', "duplicate")
        self.replace(path, "\n    Baseline {", "\n    Unclassified {", "unclassified")
        self.replace(path, "(Daemon, Bidirectional, Window,", "(Daemon, Bidirectional, MissingScope,", "orphan")
        self.replace("src/cli/capabilities.rs", '"scripting.json.v1"', '"rpc.detached"', "duplicate")
        self.replace("src/cli/scripting_contract.rs", '    "capabilities",', '    "absent-command",', "orphan")

    def test_workspace_facades_and_packaging_neighbors_are_closed(self):
        self.replace("Cargo.toml", '"crates/rmux-types",', '"crates/rmux-proto",', "duplicate")
        self.replace("Cargo.toml", '"crates/rmux-types",', '"crates/missing",', "orphan")
        self.replace("crates/rmux-sdk/Cargo.toml", 'web = []', 'web = ["regex"]', "unclassified facade")
        path = self.root / "scripts/packaging/rmux-package-layout.json"
        descriptor = json.loads(path.read_text())
        descriptor["entries"][0]["variants"]["unregistered"] = {"path": "unused"}
        path.write_text(json.dumps(descriptor))
        with self.assertRaisesRegex(ValueError, "orphan package variant"):
            manifest.project(self.root)

    def test_terminal_tables_consumers_and_feature_names_are_closed(self):
        self.replace(OUTER + "features.rs", "self.add_features(DEFAULT_MINTTY_FEATURES);",
                     "self.add_features(DEFAULT_MISSING_FEATURES);", "orphan")
        self.replace(OUTER + "defaults.rs", "const DEFAULT_MINTTY_FEATURES:",
                     "const DEFAULT_TMUX_FEATURES:", "duplicate")
        self.replace(OUTER + "defaults.rs", '    "256",', '    "missing-feature",', "orphan")

    def test_full_tiny_and_helper_personality_paths_agree(self):
        for path in ("src/tiny_main.rs", "src/tiny_main/helper.rs"):
            self.replace(path, 'eq_ignore_ascii_case("tmux")', 'eq_ignore_ascii_case("other")', "orphan personality")

    def test_comments_and_raw_strings_cannot_inject_registry_or_test_rows(self):
        path = self.root / (CORE + "command_schema.rs")
        path.write_text(path.read_text() + '\n/* nested /* comment */ ("bogus", None); */\n'
                        'const DECOY: &str = r###"commands { ("bogus", None); }"###;\n')
        projected = manifest.project(self.root)
        self.assertEqual(projected["entries"], self.original["entries"])
        self.assertNotEqual(projected["inputs"], self.original["inputs"])
        cases = manifest.rust_cases(parse('const X: &str = "#[test] fn fake() {}"; '
                                          '#[test] #[ignore] fn ignored() {} '
                                          '#[test] fn actual() {} fn helper() {}'))
        self.assertEqual(cases, ["actual"])
        for raw in ('/* unfinished', 'r##"unfinished', '"unfinished', "("):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                parse(raw)

    def test_conformance_source_removal_and_source_path_escape_are_refused(self):
        path = PROTO + "tests/capability_registry.rs"
        self.replace(path, "#[test]", "#[test]\n#[ignore]", "orphan")
        sources = Sources(self.root)
        for path in ("../Cargo.toml", str(ROOT / "Cargo.toml")):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "escapes"):
                sources.read(path)


if __name__ == "__main__":
    unittest.main()
