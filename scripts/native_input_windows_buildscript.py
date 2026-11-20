"""Bind a Windows build-script launch to its completed unit and active owner."""
import json
from pathlib import Path
import re


def qualified(guard, executable, source, head, target, env):
    token = env.get('RMUX_IDENTITY_BUILD_SESSION')
    if not token:
        return False
    if not re.fullmatch('[0-9a-f]{32}', token):
        raise guard.IdentityError('build-script launch session is not canonical')
    units = guard.support('candidate_identity_units')
    producer = units.verify(guard, executable, source, head, target)
    if producer['kind'] != 'build-script':
        return False
    receipt = target / 'identity-build-sessions' / (token + '.json')
    try:
        if receipt.is_symlink() or receipt.resolve(strict=True) != receipt:
            raise guard.IdentityError('redirected build-script launch session')
        owner = json.loads(receipt.read_text())
        if (owner['head'] != head or owner['source'] != str(source)
                or owner['producer'] != producer):
            raise guard.IdentityError('build-script launch differs from its observed owner')
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise guard.IdentityError('build-script launch has no observed owner') from error
    # This is the compiler-observed producer, not the transport wrapper. Its
    # completed unit includes locked inputs and the embedded transitive graph.
    # The generated-output observation follows this launch; it cannot precede it.
    guard.verify(executable, source, head, target)
    return True
