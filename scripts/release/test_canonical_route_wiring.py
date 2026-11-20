"""Offline wiring regressions; these do not execute Actions or native builds."""
from pathlib import Path
import re
import textwrap
import unittest

from workflow_run_input_boundary import validate_no_direct_input_expressions

ROOT = Path(__file__).resolve().parents[2]
ACTION = '.github/actions/premerge-rehearsal/action.yml'


def read(path):
    return (ROOT / path).read_text()


def steps(text, indent=6):
    return [textwrap.dedent(block).rstrip() for block in
            re.split(r'(?=^' + ' ' * indent + r'- )', text, flags=re.M)[1:]]


class CanonicalRouteWiringTest(unittest.TestCase):
    def test_all_five_builds_pass_the_profile_through_step_environment(self):
        workflow = read('.github/workflows/canonical-native-build.yml')
        jobs = re.split(r'(?=^  [\w-]+:\n)', workflow, flags=re.M)
        calls = [step for job in jobs for step in steps(job)
                 if 'uses: ./.github/actions/canonical-build\n' in step]
        self.assertEqual(len(calls), 5)
        for call in calls:
            with self.subTest(call=call.splitlines()[0]):
                self.assertIn('env:\n    RMUX_CANDIDATE_PROFILE: ${{ inputs.candidate_profile }}', call)
                self.assertNotIn('candidate-profile:', call)
                self.assertIn('expected-source-sha: ${{ inputs.expected_source_sha }}', call)
        action = read('.github/actions/canonical-build/action.yml')
        self.assertNotIn('candidate-profile:', action)
        self.assertNotRegex(action, r'(?m)^\s+RMUX_CANDIDATE_PROFILE:')
        self.assertIn('case "${RMUX_CANDIDATE_PROFILE:-canonical}" in\n'
                      '          canonical) test "$GITHUB_REF" = refs/heads/main ;;\n'
                      '          premerge-rehearsal-v1) test "$GITHUB_REF" = refs/heads/release/0.11.0 ;;\n'
                      '          *) exit 1 ;;\n        esac', action)
        for guard in ('test "$GITHUB_EVENT_NAME" = workflow_dispatch',
                      'test "$GITHUB_RUN_ATTEMPT" = 1',
                      'test "$GITHUB_SHA" = "$RMUX_EXPECTED_SOURCE_SHA"',
                      'test "$(git rev-parse HEAD)" = "$RMUX_EXPECTED_SOURCE_SHA"'):
            self.assertIn(guard, action)

    def test_shadow_routes_are_exclusive_inside_one_read_only_job(self):
        workflow = read('.github/workflows/release-shadow.yml')
        jobs = workflow.split('\njobs:\n', 1)[1]
        self.assertEqual(re.findall(r'^  ([\w-]+):$', jobs, re.M), ['seal-candidate'])
        header = jobs.split('    steps:', 1)[0]
        self.assertNotIn('    if:', header)
        self.assertIn('    permissions:\n      actions: read\n      contents: read\n', header)
        self.assertIn('RMUX_CANDIDATE_PROFILE: ${{ inputs.candidate_profile }}', header)
        blocks = steps(workflow)
        self.assertEqual(len(blocks), 13)
        for step in blocks[:2]:
            self.assertNotIn('\n  if:', step)
        self.assertIn('case "$RMUX_CANDIDATE_PROFILE" in\n'
                      '      canonical) test "$GITHUB_REF" = refs/heads/main ;;\n'
                      '      premerge-rehearsal-v1) test "$GITHUB_REF" = refs/heads/release/0.11.0 ;;\n'
                      '      *) exit 1 ;;\n    esac', blocks[0])
        self.assertIn('fetch-depth: 2', blocks[1])
        self.assertIn('persist-credentials: false', blocks[1])
        for step in blocks[2:-1]:
            self.assertIn("\n  if: inputs.candidate_profile == 'canonical'\n", step)
        self.assertIn("if: inputs.candidate_profile == 'premerge-rehearsal-v1'", blocks[-1])
        self.assertIn('uses: ./.github/actions/premerge-rehearsal', blocks[-1])
        self.assertIn('RMUX_REHEARSAL_STAGE: seal', blocks[-1])
        self.assertIn('RMUX_REHEARSAL_PERMIT: ${{ vars.RMUX_PREMERGE_REHEARSAL_PERMIT }}', blocks[-1])

    def test_rehearsal_sealing_and_simulation_share_the_same_verifier(self):
        action = read(ACTION)
        wrapper = read('.github/workflows/premerge-rehearsal.yml')
        self.assertEqual(len(steps(wrapper)), 3)
        self.assertIn('uses: ./.github/actions/premerge-rehearsal', steps(wrapper)[-1])
        self.assertIn('RMUX_REHEARSAL_STAGE: ${{ inputs.stage }}', wrapper)
        self.assertIn('RMUX_CANDIDATE_PROFILE: premerge-rehearsal-v1', wrapper)
        self.assertIn('fetch-depth: 2', wrapper)
        promotion = read('.github/workflows/release-promotion-simulation.yml')
        self.assertIn('uses: ./.github/workflows/premerge-rehearsal.yml', promotion)
        self.assertIn('stage: simulate', promotion)
        self.assertNotIn('inputs.', action)
        blocks = steps(action, 4)
        self.assertIn('test "$RMUX_CANDIDATE_PROFILE" = premerge-rehearsal-v1', blocks[0])
        self.assertIn("seal) test \"$GITHUB_WORKFLOW\" = 'Release candidate shadow sealer' ;;", blocks[0])
        self.assertIn("simulate) test \"$GITHUB_WORKFLOW\" = 'Release promotion simulation' ;;", blocks[0])
        self.assertIn('*) exit 1 ;;', blocks[0])
        self.assertIn('prepare-candidate-route.py', blocks[1])
        self.assertNotIn('\n  if:', blocks[1])
        self.assertIn('--permit-output "$RMUX_REHEARSAL_PERMIT_FILE"', blocks[1])
        for step in blocks:
            if '\n  run:' in step:
                self.assertIn('\n  shell: bash\n', step)
        self.assertEqual(action.count("if: env.RMUX_REHEARSAL_STAGE == 'simulate'"), 3)
        self.assertEqual(action.count("if: env.RMUX_REHEARSAL_STAGE == 'seal'"), 1)
        for field in ('candidate_manifest_run_id', 'candidate_manifest_artifact_id',
                      'candidate_manifest_artifact_digest', 'candidate_manifest_sha256'):
            self.assertIn('${{ inputs.' + field + ' }}', wrapper)
        for marker in ('--kind candidate', '--expected-head-branch release/0.11.0',
                       '--candidate-profile "$RMUX_CANDIDATE_PROFILE"',
                       '--rehearsal-permit "$RMUX_REHEARSAL_PERMIT_FILE"',
                       'verify-candidate-artifacts.py verify-downloaded',
                       'verify-candidate-attestations.sh', 'candidate-manifest.py verify',
                       'premerge-simulation.py', '--manifest-sha256 "$RMUX_MANIFEST_SHA256"'):
            self.assertIn(marker, action)

    def test_shared_rehearsal_action_has_no_publication_or_input_injection(self):
        paths = [ROOT / ACTION, ROOT / '.github/workflows/release-shadow.yml',
                 ROOT / '.github/workflows/premerge-rehearsal.yml']
        validate_no_direct_input_expressions(paths)
        for path in paths:
            for forbidden in ('secrets:', 'secrets: inherit', 'contents: write',
                              'actions: write', 'id-token:', 'attestations: write',
                              'packages: write', 'release-promote.yml',
                              'sign-and-push-release-tag.sh', 'gh release', 'git push',
                              'GITHUB_ENV', 'continue-on-error'):
                with self.subTest(path=path, forbidden=forbidden):
                    self.assertNotIn(forbidden, path.read_text())


if __name__ == '__main__':
    unittest.main()
