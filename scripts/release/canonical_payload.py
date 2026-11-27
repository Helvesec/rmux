"""Private cold targets and external provenance for unchanged canonical payloads."""
from __future__ import annotations

import os
from pathlib import Path
import re
import secrets

from independent_archive import digest, regular

MARKER = re.compile(rb'RMUX_RELEASE_IDENTITY_V1:([0-9a-f]{40}):END')
# Every candidate-identity graph marker version, including the locked V2 form.
INVOCATION_GRAPH = re.compile(rb'RMUX_BUILD_GRAPH_V[0-9]+:')


def embedded_commit(data, head):
    if set(MARKER.findall(data)) != {head.encode()}:
        raise ValueError('canonical embedded commit absent, ambiguous or different')
    if INVOCATION_GRAPH.search(data) or b'RMUX_BUILD_IDENTITY_V1:' in data:
        raise ValueError('canonical payload contains invocation identity injection')
    return head


def prepare(identity, source, head):
    source, _ = identity.source_identity(source, head)
    if Path(identity.git(source, 'rev-parse', '--show-toplevel')).resolve() != source:
        raise ValueError('canonical source must be the repository root')
    for key, value in os.environ.items():
        if value and (key.startswith('RMUX_IDENTITY_') or key in {
                'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_TARGET_DIR'}):
            raise ValueError('canonical build forbids inherited identity/target override: ' + key)
    target = identity.target_directory(source, head, secrets.token_hex(16))
    identity.target_identity(source, head, target)
    target.mkdir(parents=True, exist_ok=False)
    return source, target


def provenance(path, source, head, target, os_name):
    path = regular(path)
    if not path.is_relative_to(target):
        raise ValueError('canonical binary outside private target')
    before = path.stat()
    data = path.read_bytes()
    after = path.stat()
    if any(getattr(after, key) != getattr(before, key) for key in
           ('st_dev', 'st_ino', 'st_mode', 'st_size', 'st_mtime_ns', 'st_ctime_ns')):
        raise ValueError('canonical binary changed during verification')
    return dict(path=str(path), sha256=digest(data), size=len(data),
                commit=embedded_commit(data, head), source=str(source),
                os=os_name, target=str(target))
