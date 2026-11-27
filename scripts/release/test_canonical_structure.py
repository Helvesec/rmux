"""Python acceptance mirrors of the unchanged Rust release contracts.

These checks run without Cargo; the Rust assertions remain the authority.
"""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CanonicalStructureTest(unittest.TestCase):
    def test_build_preserves_seventeen_descriptions(self):
        rust = (ROOT / 'tests/release_canonical_build_spec.rs').read_text()
        self.assertIn('assert_eq!(build.matches("description:").count(), 17);', rust)
        build = (ROOT / '.github/actions/canonical-build/action.yml').read_text()
        self.assertEqual(build.count('description:'), 17)

    def test_shadow_preserves_two_permission_scopes(self):
        rust = (ROOT / 'tests/release_shadow_spec.rs').read_text()
        self.assertIn('assert_eq!(workflow.matches("permissions:").count(), 2);', rust)
        workflow = (ROOT / '.github/workflows/release-shadow.yml').read_text()
        self.assertEqual(workflow.count('permissions:'), 2)
        self.assertEqual(workflow.count('permissions: {}'), 1)

    def test_ci_preserves_exact_fast_main_description(self):
        required = 'description: Run the release-only delta after proving an exact fast main run'
        rust = (ROOT / 'tests/release_automation_spec.rs').read_text()
        self.assertIn('ci.contains("' + required + '")', rust)
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn(required, ci)


if __name__ == '__main__':
    unittest.main()
