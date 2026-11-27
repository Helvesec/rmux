"""Reject inline/reopened alias tables before any affected Cargo operation."""

import json
import unittest

import test_publication_inert_alias as fixture


REOPEN = 'alias = {}\n[alias]\nxtask = "run --locked -p xtask --"\n'


class PublicationAliasReopenTest(fixture.PublicationInertAliasTest):
    def assert_reopen_blocked(self, boundary):
        expected = {
            "metadata": [],
            "first-publish": ["metadata"],
            "second-publish": ["metadata", "publish"],
        }[boundary]
        for name in ("config", "config.toml"):
            with self.subTest(boundary=boundary, name=name):
                self.args.target_dir = self.root / (boundary + "-" + name)
                self.calls = []
                config = self.config(name=name, content=(
                    REOPEN if boundary == "metadata" else fixture.ALIAS))

                def mutate(command, **kwargs):
                    result = self.cargo(command, **kwargs)
                    if len(self.calls) == len(expected):
                        config.write_text(REOPEN, encoding="utf-8")
                    return result

                error = None
                try:
                    self.execute(mutate)
                except ValueError as caught:
                    error = caught
                self.assertEqual([call[1] for call in self.calls], expected,
                                 "malformed TOML reached a forbidden Cargo operation")
                self.assertIsNotNone(error, "malformed TOML was accepted")
                self.assertIn("Cargo configuration", str(error))
                if not expected:
                    self.assertFalse(self.args.target_dir.exists())
                else:
                    report = json.loads((self.args.target_dir /
                                         "publication-dry-run.json").read_text())
                    self.assertEqual(report["status"], "interrupted")
                    self.assertEqual([item["name"] for item in report["results"]],
                                     ["a"] if boundary == "second-publish" else [])
            config.unlink()

    def test_inline_reopen_is_refused_before_metadata(self):
        self.assert_reopen_blocked("metadata")

    def test_inline_reopen_is_refused_before_first_publish(self):
        self.assert_reopen_blocked("first-publish")

    def test_inline_reopen_is_refused_before_second_publish(self):
        self.assert_reopen_blocked("second-publish")

    def test_only_one_explicit_table_and_assignment_are_allowed(self):
        invalid = [
            'alias = {xtask = "run --locked -p xtask --"}\n',
            'alias.xtask = "run --locked -p xtask --"\n',
            fixture.ALIAS + "[alias]\n",
            fixture.ALIAS + 'xtask = "run --locked -p xtask --"\n',
            REOPEN.replace("alias = {}", '"alias" = {}'),
            REOPEN.replace("alias = {}", "'alias' = {}"),
        ]
        for name in ("config", "config.toml"):
            for index, content in enumerate(invalid):
                with self.subTest(name=name, content=content):
                    self.args.target_dir = self.root / (name + str(index))
                    self.calls = []
                    config = self.config(name=name, content=content)
                    self.assert_refused()
                config.unlink()

    def test_comments_crlf_and_quoted_keys_and_values_remain_allowed(self):
        valid = [
            '# before\r\n [ "alias" ] # table\r\n\r\n'
            ' "xtask" = \'run --locked -p xtask --\' # value\r\n# after',
            "['alias']\n'xtask' = \"run --locked -p xtask --\"",
            '["al\\u0069as"]\n"x\\u0074ask" = "run --locked -p xtask \\u002d-"\n',
            '[alias]\nxtask = """\nrun --locked -p xtask --"""\n',
        ]
        for name in ("config", "config.toml"):
            for content in valid:
                with self.subTest(name=name, content=content):
                    config = self.config(name=name, content=content)
                    self.module.reject_cargo_configuration(self.source)
                    config.unlink()


if __name__ == "__main__":
    unittest.main()
