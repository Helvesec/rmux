"""L18.2 acceptance: future 1.x support, pre-1.0 scope and private reporting.

Run with python3 -B tests/security_support_policy_spec.py. RMUX_SECURITY_POLICY
may point to a frozen SECURITY.md snapshot for the unchanged red/green replay.
Policy authority: acceptance item L18.2 and the security-support publication
scope decision D71.
"""
import os
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
POLICY = Path(os.environ.get("RMUX_SECURITY_POLICY", ROOT / "SECURITY.md"))


class SecuritySupportPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.policy = POLICY.read_text(encoding="utf-8")

    def section(self, title):
        sections = re.findall(
            rf"^## {re.escape(title)}\n(.*?)(?=^## |\Z)",
            self.policy, re.MULTILINE | re.DOTALL,
        )
        self.assertEqual(len(sections), 1, f"missing or duplicate {title} section")
        return " ".join(sections[0].split())

    def test_future_stable_1x_supports_current_and_previous_minor(self):
        support = self.section("Supported versions")
        self.assertRegex(
            support,
            r"For future stable 1\.x releases, security fixes .*?"
            r"two most recent minor release lines: the current minor and the previous minor\.",
        )

    def test_deprecation_notice_is_two_minors_or_six_months(self):
        support = self.section("Supported versions")
        self.assertRegex(
            support,
            r"Deprecations .*?announced at least two minor releases or six months before removal\.",
        )

    def test_pre_1_0_and_planned_0_11_do_not_claim_qualified_semver(self):
        support = self.section("Supported versions")
        self.assertRegex(
            support,
            r"Pre-1\.0 releases, including the planned 0\.11 series, "
            r"do not have a qualified SemVer 1\.0 compatibility contract\.",
        )

    def test_pre_1_0_retains_latest_release_without_backport_promise(self):
        support = self.section("Supported versions")
        self.assertRegex(
            support,
            r"Security fixes for pre-1\.0 releases .*?go into the latest release; "
            r"update to the newest version before you report\.",
        )
        self.assertRegex(
            support,
            r"1\.x support window .*?does not imply a two-minor backport commitment for 0\.x releases\.",
        )

    def test_private_reporting_url_and_contact_remain_available(self):
        reporting = self.section("Reporting a vulnerability")
        for required in (
            "Report security bugs through GitHub private vulnerability reporting:",
            "https://github.com/Helvesec/rmux/security/advisories/new",
            "You can also email security@rmux.io.",
            "Do not open a public issue for a security bug.",
            "the steps to reproduce it, and the version you tested",
        ):
            with self.subTest(required=required):
                self.assertIn(required, reporting)


if __name__ == "__main__":
    unittest.main()
