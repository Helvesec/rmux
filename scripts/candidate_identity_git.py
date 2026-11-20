"""Pin the Unix verifier's Git independently of the consumer's PATH."""
import hashlib
import json
import os
from pathlib import Path
import shutil


def program(g):
    if not os.environ.get('RMUX_IDENTITY_TARGET'):
        selected = shutil.which('git')
        return str(Path(selected).resolve(strict=True)) if selected else None
    source, head, target = g.context()
    try:
        try:
            record = json.loads((target / 'identity-context.json').read_text())
        except FileNotFoundError:
            return legacy_program(g, head)
        for key, value in (('RMUX_IDENTITY_SOURCE', str(source)),
                           ('RMUX_IDENTITY_HEAD', head),
                           ('RMUX_IDENTITY_TARGET', str(target))):
            if record[key] != value:
                raise g.IdentityError('qualified Git context differs')
        if 'git' not in record:
            return legacy_program(g, head)
        binding = record['git']
        path = Path(binding['path'])
        if (not path.is_absolute() or path.resolve(strict=True) != path
                or not path.is_file() or not os.access(path, os.X_OK)
                or hashlib.sha256(path.read_bytes()).hexdigest() != binding['sha256']):
            raise g.IdentityError('qualified Git executable differs')
        return str(path)
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise g.IdentityError('qualified Git binding missing or invalid') from error


def legacy_program(g, head):
    # Only the already-approved 043 archive predates tool bindings. Current
    # contexts with deleted bindings must never fall back to the consumer PATH.
    script = Path(os.environ['RMUX_IDENTITY_SCRIPT'])
    digest = hashlib.sha256(script.read_bytes()).hexdigest()
    g.support('candidate_identity_harness').validate(g, script, digest, head)
    selected = shutil.which('git')
    return str(Path(selected).resolve(strict=True)) if selected else None
