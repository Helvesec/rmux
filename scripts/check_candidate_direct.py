"""Reject an unbound direct Cargo CLI selection in the actual Rust child test."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import uuid

import candidate_identity as identity


def main():
    if os.name == 'nt':
        raise identity.IdentityError('this executable canary requires a Unix host')
    source, head, target = identity.context()
    identity.source_identity(source, head)
    code = source / 'tests/candidate_identity_children.rs'
    receipts = (json.loads(path.read_text()) for path in (target / 'identity-builds').glob('*.json'))
    parent = next(item for item in receipts
                  if item.get('kind') == 'rust-test' and Path(item['input']) == code)
    binary = target / 'debug/rmux'
    original = identity.verify_built(binary, source, head, target)
    token = uuid.uuid4().hex
    out = target / 'direct-regression' / token
    out.mkdir(parents=True)
    backup = out / 'original-rmux'
    canary = out / 'entered'
    payload = ('#!/bin/sh\nprintf entered > ' + shlex.quote(str(canary)) + '\nexit 77\n').encode()
    command = [sys.executable, str(source / 'scripts/candidate_identity.py'), 'run', parent['path'],
               'cli_selected_daemon_without_identity_is_rejected_before_execution',
               '--exact', '--nocapture', '--test-threads=1']
    binary.rename(backup)
    try:
        binary.write_bytes(payload)
        binary.chmod(0o755)
        result = subprocess.run(command, cwd=source, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=30)
        (out / 'test.log').write_bytes(result.stdout)
    finally:
        if binary.read_bytes() != payload:
            raise identity.IdentityError('canary path changed; original retained without overwriting')
        binary.unlink()
        backup.rename(binary)
    assert identity.file_hash(binary) == original['sha256'], 'original CLI not restored'
    record = dict(head=head, parent=parent['path'], parent_sha256=parent['sha256'],
                  original_cli=original, exit=result.returncode, canary_entered=canary.exists(),
                  original_restored=True)
    (out / 'result.json').write_text(json.dumps(record, sort_keys=True) + '\n')
    print(json.dumps(record, sort_keys=True))
    assert result.returncode == 101, 'the negative Rust test must fail before running the CLI'
    assert not canary.exists(), 'direct unbound Cargo CLI entered before identity rejection'


if __name__ == '__main__':
    main()
