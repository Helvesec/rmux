"""Acceptance of independent source-file/queue replay in the bounded runner."""
import unittest

from bounded import planned_observations


class IndependentRoutesAcceptance(unittest.TestCase):
    def test_source_and_queue_are_required_observations(self):
        # These evaluations must reach the oracle comparison. A runner which
        # silently drops either route cannot establish the five-route claim.
        contract = {
            'steps': [
                {'kind': 'route', 'id': 'source-evaluation',
                 'route': 'source-file',
                 'argv': ['display-message', '-p', '-t', 'alpha:0.0',
                          '#{session_name}|#{pane_index}']},
                {'kind': 'route', 'id': 'queue-evaluation',
                 'route': 'queue',
                 'argv': ['display-message', '-p', '-t', 'alpha:0.0',
                          '#{session_name}|#{pane_index}']},
            ],
        }
        self.assertEqual(
            [step['id'] for step in planned_observations(contract)],
            ['source-evaluation', 'queue-evaluation'],
            'source-file and queue evaluations must not disappear from replay',
        )


if __name__ == '__main__':
    unittest.main()
