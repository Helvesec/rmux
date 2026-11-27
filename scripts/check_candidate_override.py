"""Exercise the existing drop-in test with an unbound selected override."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import uuid

import candidate_identity as identity


def main():
    source, head, target = identity.context()
    identity.source_identity(source, head)
    active = Path(os.environ['CARGO_TARGET_DIR'])
    cargo = shutil.which(os.environ.get('CARGO', 'cargo'))
    if not cargo:
        raise identity.IdentityError('Cargo unavailable')
    subprocess.run([cargo, 'test', '--locked', '--offline', '--test',
                    'cli_dropin_prefix', '--no-run'], check=True)
    parents = [p for p in (active / 'debug/deps').glob('cli_dropin_prefix-*')
               if p.is_file() and (p.suffix == '.exe' if os.name == 'nt'
                                   else p.suffix != '.d' and os.access(p, os.X_OK))]
    if len(parents) != 1:
        raise identity.IdentityError('drop-in test executable selection ambiguous')
    parent = identity.verify_built(parents[0], source, head, target)
    out = target / 'override-regression' / uuid.uuid4().hex
    out.mkdir(parents=True)
    canary = out / 'entered'
    foreign = out / ('foreign.cmd' if os.name == 'nt' else 'foreign.sh')
    foreign.write_text('@echo off\r\necho entered>>"' + str(canary) + '"\r\nexit /b 0\r\n'
                       if os.name == 'nt' else '#!/bin/sh\nprintf entered >> '
                       + shlex.quote(str(canary)) + '\nexit 0\n')
    foreign.chmod(0o755)
    case = ('doctor_and_setup_help_project_global_prefixes_live' if os.name == 'nt'
            else 'doctor_and_setup_help_accept_opaque_native_prefix_values_live')
    env = dict(os.environ, RMUX_TEST_DROPIN_BINARY=str(foreign))
    result = subprocess.run([sys.executable, str(source / 'scripts/candidate_identity.py'),
                             'run', parent['path'], case, '--exact', '--nocapture',
                             '--test-threads=1'], cwd=source, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
    (out / 'test.log').write_bytes(result.stdout)
    record = dict(head=head, parent=parent, case=case, exit=result.returncode,
                  foreign_entered=canary.exists())
    (out / 'result.json').write_text(json.dumps(record, sort_keys=True) + '\n')
    print(json.dumps(record, sort_keys=True))
    assert result.returncode == 101 and not canary.exists(), 'unbound override executed'
    assert b'candidate executable identity' in result.stdout, 'rejection was not identity'


if __name__ == '__main__':
    main()
