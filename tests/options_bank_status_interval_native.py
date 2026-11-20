"""Replay the bank's frozen FIRST/SECOND/FIRST timer assertions natively.

Usage: python3 tests/options_bank_status_interval_native.py CONTRACT OUTPUT
The supplied per-option contract pins the native oracle and current source.
"""
from pathlib import Path
import sys

import options_bank_linux as bank


def main(contract_path, output):
    contract = bank.load(contract_path)
    assert contract['default_recipe']['names'] == ['status-interval']
    result = bank.execute(contract_path, 'oracle', Path(output), 'per-option')
    receipt = bank.load(Path(output) / 'RESULT.json')
    assert receipt['closure']['status'] == 'PASS', receipt['closure']
    assert result == 0, receipt.get('error')
    effects = [row['screen_observation'] for row in receipt['commands']
               if row['role'] in ('status-interval:effect',
                                  'status-interval:second-effect',
                                  'status-interval:restored-effect')]
    assert effects == [[dict(text=text, row=23)]
                       for text in ('FIRST', 'SECOND', 'FIRST')], effects


if __name__ == '__main__':
    main(*sys.argv[1:])
