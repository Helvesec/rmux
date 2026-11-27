"""Approved 043 archive authority, shared by private and legacy gate contexts."""
import hashlib
import json
from pathlib import Path

HEAD = 'd2977a2ff564249d75dcbb53f73912af9fbcebe9'
SCRIPT_SHA256 = 'f99fa4b8ec57ce742baafe205bddf83542f701543d4e17bb122841ea20941a17'
MANIFEST_SHA256 = '42eace034eece187fd92df8a544a887733042f4c0ab09a194235a18a570f5295'


def validate(g, script, script_sha256, candidate_head):
    script = Path(script)
    # Trains unpack the approved archive beside HARNESS043.json. Do not search
    # arbitrary ancestors for an ENV-selected script or accept sibling copies.
    root = script.parent.parent
    manifest = root.parent / 'HARNESS043.json'
    if (root.name != 'harness043' or script != root / 'scripts/candidate_identity.py'
            or script.resolve(strict=True) != script
            or manifest.resolve(strict=True) != manifest):
        raise g.IdentityError('external identity harness path differs')
    context = g.support('candidate_identity_context')
    manifest_bytes = manifest.read_bytes()
    if hashlib.sha256(manifest_bytes).hexdigest() != MANIFEST_SHA256:
        raise g.IdentityError('external identity harness authority differs')
    record = json.loads(manifest_bytes)
    if record.get('head') != HEAD or candidate_head == HEAD:
        raise g.IdentityError('external identity harness HEAD differs or is not distinct')
    files = record['files']
    if (script_sha256 != SCRIPT_SHA256
            or files.get('scripts/candidate_identity.py') != SCRIPT_SHA256):
        raise g.IdentityError('external identity harness hash differs')
    for relative, expected in files.items():
        path = root / relative
        if (not path.is_relative_to(root) or path.resolve(strict=True) != path
                or context.direct_file_hash(path) != expected):
            raise g.IdentityError('external identity harness file differs')
    return True
