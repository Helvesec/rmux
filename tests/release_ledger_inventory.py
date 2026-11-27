"""Regression witnesses for the C-D32 ledger/inventory boundary."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "ledger", ROOT / "scripts/check-tmux-release-ledger.py"
)
LEDGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LEDGER)


def table(body):
    return 'pub(super) const LIST_COMMAND_SIGNATURES: &[(&str, &str)] = &[\n' + body + '\n];\n'


class PaneInventoryContract(unittest.TestCase):
    def check_contract(self, classification, names, deferred):
        inventory = table("\n".join(f'    ("{name}", "usage"),' for name in names))
        return LEDGER.validate_pane_inventory(
            {"C-D32": {"inventory": classification, "deferred": deferred}}, inventory
        )

    def test_bounded_public_commands_with_deferred_subfeatures(self):
        self.assertIsNone(self.check_contract(
            "bounded", ["new-pane", "set-pane-placement"],
            ["floating geometry defaults/percentages", "floating Empty/key"],
        ))

    def test_deferred_public_command_is_rejected(self):
        error = self.check_contract(
            "deferred", ["new-pane"], ["floating-pane", "new-pane"]
        )
        self.assertIn("deferred pane commands are advertised", error)

    def test_bounded_requires_each_public_command(self):
        for remaining in ["new-pane", "set-pane-placement"]:
            with self.subTest(remaining=remaining):
                self.assertIn("commands are missing", self.check_contract(
                    "bounded", [remaining], ["floating layout serialization"]
                ))

    def test_bounded_cannot_defer_whole_commands_or_aliases(self):
        for whole in ["new-pane", "newp", "floating-pane", "set-pane-placement"]:
            with self.subTest(whole=whole):
                self.assertIn("not whole commands", self.check_contract(
                    "bounded", ["new-pane", "set-pane-placement"], [whole]
                ))

    def test_deferred_unadvertised_commands_are_valid(self):
        self.assertIsNone(self.check_contract(
            "deferred", ["new-session"], ["floating-pane", "new-pane"]
        ))

    def test_deferred_still_names_whole_deferred_features(self):
        self.assertIn("must defer", self.check_contract("deferred", [], []))

    def test_missing_or_unclassified_c_d32_is_rejected(self):
        self.assertIn("must be bounded or deferred", LEDGER.validate_pane_inventory({}, table("")))
        self.assertIn("must be bounded or deferred", self.check_contract("none", [], []))

    def test_comments_and_usage_text_do_not_advertise_commands(self):
        inventory = (
            '// ("new-pane", "usage"),\n'
            '/*\n    ("new-pane", "usage"),\n*/\n'
            '    ("new-session", "new-pane floating"),'
        )
        self.assertEqual(LEDGER.pane_inventory_entries(table(inventory)),
                         [("new-session", "new-pane floating")])
        self.assertIsNone(LEDGER.validate_pane_inventory(
            {"C-D32": {"inventory": "deferred", "deferred": ["floating-pane", "new-pane"]}},
            table(inventory),
        ))
        self.assertIn("commands are missing", LEDGER.validate_pane_inventory(
            {"C-D32": {"inventory": "bounded", "deferred": []}}, table(inventory)
        ))

    def test_reviewer_annotations_preserve_every_name_and_usage(self):
        source = LEDGER.COMMAND_INVENTORY.read_text(encoding="utf-8")
        expected = LEDGER.pane_inventory_entries(source)
        declaration = "pub(super) const LIST_COMMAND_SIGNATURES: &[(&str, &str)] = &["
        variants = [
            source.replace(declaration, "#[allow(dead_code)]\n" + declaration),
            source.replace(declaration, declaration + " // inventory entries"),
            source.replace("    (\n", "    (/* command */\n", 1),
        ]
        for variant in variants:
            with self.subTest(variant=variant[:100]):
                self.assertNotEqual(variant, source)
                self.assertEqual(LEDGER.pane_inventory_entries(variant), expected)

    def test_nested_comments_and_commented_declarations_are_trivia(self):
        source = ('/* outer /* nested */ ("new-pane", "absent"), */\n'
                  '// const LIST_COMMAND_SIGNATURES: &[(&str, &str)] = &[];\n'
                  + table('("new-session", "usage"), // tail\n'
                          '/* ("set-pane-placement", "absent"), */')
                  + '// after table\n')
        self.assertEqual(LEDGER.pane_inventory_entries(source), [("new-session", "usage")])

    def test_trivia_between_declaration_tokens_and_delimiters(self):
        source = table('("new-session", "usage") /* before comma */,')
        source = source.replace("pub(super)", "/* lead */ pub /* visibility */ (super)")
        source = source.replace(": &[(&str, &str)] = &[",
                                ": /* type */ & [ (& str, & str) ] = /* value */ & [")
        source = source.replace("];", "/* close */ ] /* semicolon */ ; // tail")
        self.assertEqual(LEDGER.pane_inventory_entries(source), [("new-session", "usage")])

    def test_comment_and_attribute_markers_in_strings_are_preserved(self):
        usage = 'https://example/*literal*/ #[cfg(any())] const LIST_COMMAND_SIGNATURES'
        source = table(f'("new-session", "{usage}"),')
        self.assertEqual(LEDGER.pane_inventory_entries(source), [("new-session", usage)])

    def test_escaped_strings_are_not_lexical_trivia(self):
        source = table(r'("new-session", "quote: \" // /* \\ \n \r \t \0"),')
        self.assertEqual(LEDGER.pane_inventory_entries(source),
                         [("new-session", 'quote: " // /* \\ \n \r \t \0')])

    def test_unterminated_or_unsupported_lexical_constructs_fail_closed(self):
        for source in [table('/* unterminated'), table('("new-pane", "unterminated),'),
                       table('("new-pane", r#"raw /* string"#),'),
                       '#[cfg_attr(any(), allow(dead_code))]\n' + table(""),
                       '#[custom_attribute]\n' + table(""),
                       table("") + table("")]:
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    LEDGER.pane_inventory_entries(source)

    def test_helper_layout_does_not_change_inventory(self):
        source = LEDGER.COMMAND_INVENTORY.read_text(encoding="utf-8")
        expression = "|(command_name, usage)| (*command_name == name).then_some(*usage)"
        block = "|(command_name, usage)| {\n (*command_name == name).then_some(*usage)\n }"
        variant = source.replace(expression, block)
        self.assertNotEqual(variant, source)
        self.assertEqual(LEDGER.pane_inventory_entries(variant), LEDGER.pane_inventory_entries(source))


    def test_real_inventory_is_exhaustively_read(self):
        inventory = LEDGER.COMMAND_INVENTORY.read_text(encoding="utf-8")
        # Extraction must succeed for the real file, including a genuinely
        # deferred table. Presence/absence belongs to the classification tests.
        LEDGER.pane_inventory_names(inventory)

    def test_flattened_real_inventory_is_rejected_in_both_classifications(self):
        inventory = LEDGER.COMMAND_INVENTORY.read_text(encoding="utf-8")
        prefix, body = inventory.split(" = &[", 1)
        flat = " ".join(body.splitlines())
        for separator in ["", "\n"]:
            witness = prefix + " = &[" + separator + flat
            for classification in ["bounded", "deferred"]:
                with self.subTest(separator=separator, classification=classification):
                    error = LEDGER.validate_pane_inventory(
                        {"C-D32": {"inventory": classification,
                                   "deferred": ["floating-pane", "new-pane"]}}, witness
                    )
                    self.assertIn("inventory extraction failed", error or "")

    def test_unparsed_entries_are_rejected_even_after_a_valid_entry(self):
        for unsupported in ['include!("commands.rs"),',
                            '("new-pane", concat!("a", "b")),',
                            '#[cfg(unix)] ("new-pane", "usage"),',
                            '("new-pane", "usage"), ("set-pane-placement", "usage"),']:
            for classification in ["bounded", "deferred"]:
                with self.subTest(unsupported=unsupported, classification=classification):
                    error = LEDGER.validate_pane_inventory(
                        {"C-D32": {"inventory": classification, "deferred": []}},
                        table('("attach-session", "usage"),\n' + unsupported),
                    )
                    self.assertIn("inventory extraction failed", error or "")

    def test_conditional_or_computed_prologue_is_not_silently_ignored(self):
        for prefix in ['#[cfg(any())]\n', 'make_commands!();\n']:
            for classification in ["bounded", "deferred"]:
                error = LEDGER.validate_pane_inventory(
                    {"C-D32": {"inventory": classification, "deferred": []}},
                    prefix + table('("attach-session", "usage"),'),
                )
                self.assertIn("inventory extraction failed", error or "")

    def test_multiline_tuples_remain_supported(self):
        self.assertEqual(LEDGER.pane_inventory_names(table(
            '(\n "new-pane",\n "usage",\n),\n("set-pane-placement", "usage"),'
        )), {"new-pane", "set-pane-placement"})


class RegistryReferenceContract(unittest.TestCase):
    # Renames in 787c86fa: preserve the evidence owner for each contract.
    RELOCATED_EVIDENCE = (
        ("C-D51", "crates/rmux-client/src/attach_windows/input.rs",
         "crates/rmux-client/src/attach/windows/input.rs"),
        ("C-D52", "crates/rmux-server/src/handler_client_runtime.rs",
         "crates/rmux-server/src/handler/client_runtime_support.rs"),
        ("C-D55", "crates/rmux-server/src/handler_pane/attached_input/bracketed_paste.rs",
         "crates/rmux-server/src/handler/pane_support/pane_attached_input/bracketed_paste.rs"),
        ("C-D92", "crates/rmux-server/src/handler_pane/attached_input/terminal_response.rs",
         "crates/rmux-server/src/handler/pane_support/pane_attached_input/terminal_response.rs"),
    )

    def entries(self):
        payload = LEDGER.toml_reader.loads(LEDGER.LEDGER.read_text(encoding="utf-8"))
        return {entry["id"]: entry for entry in payload["entry"]}

    def test_relocated_evidence_cites_the_exact_contract_successor(self):
        entries = self.entries()
        for identity, old, successor in self.RELOCATED_EVIDENCE:
            with self.subTest(identity=identity):
                self.assertIn(successor, entries[identity]["evidence"])
                self.assertNotIn(old, entries[identity]["evidence"])

    def test_validator_accepts_tracked_evidence_successors(self):
        for identity, _old, successor in self.RELOCATED_EVIDENCE:
            with self.subTest(identity=identity):
                self.assertEqual(LEDGER.validate_reference(successor),
                                 (Path(successor), None))

    def test_validator_rejects_obsolete_and_misspelled_evidence_paths(self):
        for identity, old, successor in self.RELOCATED_EVIDENCE:
            for invalid in (old, successor + ".missing"):
                with self.subTest(identity=identity, reference=invalid):
                    with self.assertRaisesRegex(ValueError, "reference points to a missing path"):
                        LEDGER.validate_reference(invalid)

    def test_real_registry_references_resolve(self):
        LEDGER.load_entries()

    def test_retired_trailing_hash_evidence_remains_tracked(self):
        for reference in self.entries()["C-D36"]["evidence"]:
            with self.subTest(reference=reference):
                LEDGER.validate_reference(reference)

    def test_real_product_divergence_references_are_current(self):
        count, error = LEDGER.validate_product_tests(self.entries())
        self.assertIsNone(error)
        self.assertGreater(count, 0)


if __name__ == "__main__":
    unittest.main()
