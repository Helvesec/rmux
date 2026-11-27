"""Current-tree half of the public-cut inventory, under the cut's native guard.

Historical ledger/relocation canaries run separately on the bound source. This
half measures the actual public tree using its declarations and native parser;
no reference is resealed and no projected file is substituted during the scan.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

from public_cut import decode, git, require, verify


def main():
    cut, source, rule_path, output = map(Path, sys.argv[1:5])
    rule = decode(rule_path.read_bytes())
    verify(cut, rule, rule['public']['commit'], rule['trunk_anchor'])
    # Import the candidate's implementation, not a stale tool-checkout copy.
    sys.path.insert(0, str(cut / 'scripts'))
    import candidate_identity as identity
    identity.validate_context()
    root, head, target = identity.context()
    require(root == cut and head == rule['public']['commit'], 'wrong cut build identity')
    subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path',
                    str(cut / 'scripts/source-inventory/Cargo.toml'), '--jobs', '4'],
                   cwd=cut, check=True)
    binary = target / ('debug/rmux-source-syntax.exe' if sys.platform == 'win32'
                       else 'debug/rmux-source-syntax')
    proof = identity.verify_built(binary, root, head, target)
    selected = subprocess.check_output([sys.executable, str(cut / 'scripts/candidate_identity.py'),
                                       'check', str(binary)], cwd=cut, text=True).strip()
    sys.path.insert(0, str(cut / 'scripts/source-inventory'))
    from inventory import scan
    from model import read_tree
    from compare import semantic_population
    # Use the stricter original blacklist spellings, even if the public text of
    # that policy was rewritten. It is code from the verified source authority.
    import importlib.util
    spec = importlib.util.spec_from_file_location('original_policy',
                source / 'scripts/source-inventory/production_policy.py')
    policy_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(policy_module)
    reports = []
    for label, repo, revision in [('source', source, rule['source']['commit']),
                                  ('cut', cut, head)]:
        raw = git(repo, 'show', revision + ':scripts/source-inventory/declarations.json')
        report = scan(repo, revision, Path(selected), json.loads(raw), raw)
        (output / (label + '-inventory.json')).write_text(json.dumps(report) + '\n')
        require(not report['errors'], label + ' inventory errors')
        require(not any(item['production_ge_600'] for item in report['crates'].values()),
                label + ' production file exceeds size bound')
        reports.append(report)
    before, after = reports
    # compare() transports historical epochs along Git ancestry. A public cut
    # is deliberately not a descendant, so compare the measured populations
    # directly after verify() has established the different projection relation.
    def occurrences(rows):
        return sum(row.get('occurrences', 1) if isinstance(row, dict) else 1 for row in rows)
    increases = {key: occurrences(after['ratchets'][key]) - occurrences(rows)
                 for key, rows in before['ratchets'].items()
                 if occurrences(after['ratchets'][key]) > occurrences(rows)}
    old_attributes, _ = semantic_population(before)
    new_attributes, _ = semantic_population(after)
    delta = dict(literal_ratchet_increases=increases,
                 semantic_attribute_additions=list((new_attributes - old_attributes).items()),
                 production_size_violations=[path for row in after['crates'].values()
                                             for path in row['production_ge_600']])
    policy_module.check_ratchets(delta)
    require(not policy_module.scan_blacklist(after, read_tree(cut, head)),
            'public production blacklist violation')
    # The public rewrite may remove tests/docs/tooling, never silently reclassify
    # production or lose a Rust production consumer.
    def production(report):
        return {row['path']: {key: row.get(key) for key in
                ('counts', 'regions', 'consumers', 'semantic_attributes')}
                for row in report['files'] if row['counts'].get('production', 0)}
    require(production(before) == production(after),
            'public projection changes production classification or consumers')
    require(identity.verify_built(binary, root, head, target) == proof,
            'native parser identity changed')
    receipt = dict(cut=head, tree=after['tree'], files=len(after['files']),
                   errors=after['errors'], production=after['totals']['production'],
                   production_classification_equal=True, ratchets='PASS', blacklist='PASS',
                   size='PASS', parser=proof)
    (output / 'cut-inventory-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt), flush=True)


if __name__ == '__main__':
    main()
