"""Exercise the real SDK smoke cleanup after its primary binary resolution fails."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import uuid

import candidate_identity as identity


def main():
    source, head, target = identity.context()
    identity.source_identity(source, head)
    code = source / 'crates/rmux-sdk/tests/smoke_v1.rs'
    receipts = (json.loads(path.read_text()) for path in (target / 'identity-builds').glob('*.json'))
    built = next(record for record in receipts
                 if record.get('kind') == 'rust-test' and Path(record['input']) == code)
    token = uuid.uuid4().hex
    out = target / 'cleanup-regression' / token
    out.mkdir(parents=True)
    # The existing SDK smoke explicitly requires this /tmp prefix.
    root = Path('/tmp') / ('rmux-sdk-v1-smoke-identity-' + token)
    root.mkdir()
    (root / 'daemon.sock').touch()
    canary = out / 'entered'
    fallback = out / 'rmux'
    fallback.write_text('#!/bin/sh\nprintf entered > ' + shlex.quote(str(canary)) + '\nexit 77\n')
    fallback.chmod(0o755)
    cargo = out / 'cargo'
    cargo.write_text('#!/bin/sh\nexit 77\n')
    cargo.chmod(0o755)
    env = dict(os.environ, CARGO=str(cargo), RMUX_SDK_SMOKE_ROOT=str(root),
               PATH=str(out) + os.pathsep + os.environ['PATH'])
    command = [sys.executable, str(source / 'scripts/candidate_identity.py'), 'run', built['path'],
               'daemon_backed_sdk_happy_path_cleans_tmp_socket_lock_daemon_and_child',
               '--exact', '--nocapture', '--test-threads=1']
    result = subprocess.run(command, cwd=source, env=env, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=30)
    (out / 'test.log').write_bytes(result.stdout)
    record = dict(head=head, parent=built['path'], parent_sha256=built['sha256'],
                  exit=result.returncode, unbound_cleanup_entered=canary.exists(),
                  owned_root_removed=not root.exists())
    (out / 'result.json').write_text(json.dumps(record, sort_keys=True) + '\n')
    print(json.dumps(record, sort_keys=True))
    assert result.returncode == 101 and not root.exists(), 'primary failure/cleanup fixture invalid'
    assert not canary.exists(), 'unbound cleanup fallback entered after primary resolution failed'


if __name__ == '__main__':
    main()
