"""Compose section 11 from an exact projection and both required authorities.

Historical claims are checked on the source, current public structure and all
other selected commands on the cut. Receipts distinguish these authorities;
this does not claim that the unadapted historical suite passes on detached Git.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from public_cut import decode, git, require, verify


TOOL = Path(__file__).resolve().parent


def clean(repo, expected):
    require(git(repo, 'rev-parse', 'HEAD').decode().strip() == expected, 'wrong checkout HEAD')
    require(not git(repo, 'status', '--porcelain', '--untracked-files=normal'),
            'dirty checkout: ' + str(repo))


def command_mode():
    """Run the candidate registry in its own already authenticated context."""
    root = Path(sys.argv[2]).resolve()
    platform = sys.argv[3]
    sys.path.insert(0, str(root / 'scripts'))
    import candidate_identity
    import regression_registry
    candidate_identity.validate_context()
    require(candidate_identity.context()[0] == root, 'wrong registry execution root')
    commands = regression_registry.select(regression_registry.load_registry(), platform)
    require(sum(row['id'] == 'source-inventory-ratchets' for row in commands) == 1,
            'missing or duplicate inventory control')
    # The replaced command is fulfilled by BOTH source-history and public-tree
    # steps, each mandatory in replay(). No derived registry or cell is forged.
    commands = [row for row in commands if row['id'] != 'source-inventory-ratchets']
    os.environ['RMUX_REGRESSION_REGISTRY_ACTIVE'] = '1'
    return regression_registry.run_commands(commands)


def replay(args):
    cut, source = args.cut_repo.resolve(), args.source_repo.resolve()
    rule_path = args.cut_rule.resolve()
    output = args.cut_output.resolve()
    require(not output.exists(), 'receipt directory must be new')
    output.mkdir(parents=True)
    rule = decode(rule_path.read_bytes())
    native = {'darwin': 'macos', 'linux': 'linux', 'win32': 'windows'}.get(sys.platform)
    require(args.platform == native, 'native OS required for public cut replay')
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(('RMUX_IDENTITY_', 'CARGO_TARGET_')) or key in (
                'CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                'RMUX_REGRESSION_REGISTRY_ACTIVE', 'SOURCE_SYNTAX', 'SOURCE_RATCHET_REVISION',
                'RMUX', 'RMUX_PANE', 'TMUX', 'TMUX_PANE'):
            env.pop(key)
    env.update(PYTHONDONTWRITEBYTECODE='1', CARGO_BUILD_JOBS='4')
    receipt = dict(platform=native, steps=[], status='FAIL')

    def run(name, root, argv):
        start = time.time()
        print('[public-cut] ' + name, flush=True)
        with (output / (name + '.log')).open('w') as log:
            result = subprocess.run(argv, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        receipt['steps'].append(dict(name=name, root=str(root), argv=list(map(str, argv)),
                                    rc=result.returncode, elapsed_s=time.time() - start))
        require(result.returncode == 0, name + ' RED; see ' + str(output / (name + '.log')))

    try:
        clean(cut, rule['public']['commit'])
        clean(source, rule['source']['commit'])
        receipt['projection'] = verify(cut, rule, rule['public']['commit'], args.trunk)
        # Assert the source checkout really has the same authority just verified
        # through the cut's object store, including historical Git, not just files.
        verify(source, rule, rule['public']['commit'], args.trunk)
        (output / 'projection.json').write_text(json.dumps(receipt['projection'], indent=2) + '\n')
        run('source-history-inventory', source, [sys.executable,
            source / 'scripts/candidate_identity.py', 'with', '--', sys.executable,
            source / 'scripts/regression_registry_checks.py', 'source-inventory'])
        run('public-tree-inventory', cut, [sys.executable,
            cut / 'scripts/candidate_identity.py', 'with-manifest',
            cut / 'scripts/source-inventory/Cargo.toml', '--', sys.executable,
            TOOL / 'public_cut_inventory.py', cut, source, rule_path, output])
        run('public-registry', cut, [sys.executable, cut / 'scripts/candidate_identity.py',
            'with', '--', sys.executable, Path(__file__).resolve(),
            'commands', cut, native])
        clean(cut, rule['public']['commit'])
        clean(source, rule['source']['commit'])
        require(verify(cut, decode(rule_path.read_bytes()), rule['public']['commit'], args.trunk)
                == receipt['projection'], 'projection changed during replay')
        receipt['status'] = 'PASS'
        return 0
    finally:
        (output / 'section-11.json').write_text(json.dumps(receipt, indent=2) + '\n')


if __name__ == '__main__':
    require(sys.argv[1:2] == ['commands'], 'use regression_registry.py --cut-repo')
    sys.exit(command_mode())
