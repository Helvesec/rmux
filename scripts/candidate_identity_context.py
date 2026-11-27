"""Validate active candidate identity context bindings for gate entrypoints."""
import hashlib
import json
import os
from pathlib import Path
import shutil


def direct_file_hash(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_context_bindings(env, target, runner_key):
    keys = ('RMUX_IDENTITY_SOURCE', 'RMUX_IDENTITY_HEAD', 'RMUX_IDENTITY_TARGET',
            'RMUX_IDENTITY_PYTHON', 'RMUX_IDENTITY_SCRIPT',
            'RMUX_IDENTITY_SCRIPT_SHA256', 'RMUX_IDENTITY_RUSTC',
            'RMUX_IDENTITY_RUSTDOC', 'CARGO_TARGET_DIR', 'RUSTC_WRAPPER',
            'RUSTDOC', runner_key)
    record = {key: env[key] for key in keys if key in env}
    if os.name != 'nt':
        program = Path(shutil.which('git')).resolve(strict=True)
        record['git'] = {'path': str(program), 'sha256': direct_file_hash(program)}
    (target / 'identity-context.json').write_text(json.dumps(record, sort_keys=True) + '\n')


def context_bindings(g, source, head, target, gate=False):
    try:
        record = json.loads((target / 'identity-context.json').read_text())
    except FileNotFoundError as error:
        return legacy_context_bindings(g, source, head, target, error)
    except (OSError, json.JSONDecodeError) as error:
        raise g.IdentityError('identity context binding invalid') from error
    if not isinstance(record, dict):
        raise g.IdentityError('identity context binding invalid')
    for key, value in (('RMUX_IDENTITY_SOURCE', str(source)),
                       ('RMUX_IDENTITY_HEAD', head),
                       ('RMUX_IDENTITY_TARGET', str(target))):
        if record.get(key) != value or os.environ.get(key) != value:
            raise g.IdentityError('candidate identity context differs')
    for key in ('RMUX_IDENTITY_PYTHON', 'RMUX_IDENTITY_SCRIPT',
                'RMUX_IDENTITY_SCRIPT_SHA256', 'RMUX_IDENTITY_RUSTC',
                'RMUX_IDENTITY_RUSTDOC'):
        if not record.get(key) or os.environ.get(key) != record[key]:
            raise g.IdentityError('candidate identity context differs')
    if os.environ.get('CARGO_TARGET_DIR') != record.get('CARGO_TARGET_DIR'):
        raise g.IdentityError('candidate identity context differs')
    active_script = Path(record['RMUX_IDENTITY_SCRIPT'])
    if gate and active_script != source / 'scripts/candidate_identity.py':
        find_harness_manifest(g, active_script, record['RMUX_IDENTITY_SCRIPT_SHA256'], head)
    return record


def legacy_context_bindings(g, source, head, target, original_error):
    active = os.environ.get('RMUX_IDENTITY_SCRIPT')
    if not active:
        raise g.IdentityError('identity context binding missing') from original_error
    try:
        active_script = Path(active)
        if active_script.resolve(strict=True) != active_script:
            raise g.IdentityError('identity implementation path differs')
    except OSError as error:
        raise g.IdentityError('identity implementation differs') from error
    script_sha256 = direct_file_hash(active_script)
    candidate_script = source / 'scripts' / 'candidate_identity.py'
    if active_script == candidate_script.resolve(strict=True):
        tracked_clean_file(g, active_script, source, head)
    elif not find_harness_manifest(g, active_script, script_sha256, head):
        raise g.IdentityError('external identity harness manifest missing')
    record = {key: os.environ.get(key) for key in (
        'RMUX_IDENTITY_SOURCE', 'RMUX_IDENTITY_HEAD', 'RMUX_IDENTITY_TARGET',
        'RMUX_IDENTITY_PYTHON', 'RMUX_IDENTITY_SCRIPT', 'RMUX_IDENTITY_RUSTC',
        'RMUX_IDENTITY_RUSTDOC', 'CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTDOC')}
    record['RMUX_IDENTITY_SCRIPT_SHA256'] = script_sha256
    for key, value in record.items():
        if key != 'RUSTDOC' and not value:
            raise g.IdentityError('candidate identity context differs')
    if record['RMUX_IDENTITY_SOURCE'] != str(source) or record['RMUX_IDENTITY_HEAD'] != head:
        raise g.IdentityError('candidate identity context differs')
    if record['RMUX_IDENTITY_TARGET'] != str(target) or record['CARGO_TARGET_DIR'] != str(target):
        raise g.IdentityError('candidate identity context differs')
    return record


def find_harness_manifest(g, active_script, script_sha256, candidate_head):
    return g.support('candidate_identity_harness').validate(
        g, active_script, script_sha256, candidate_head)


def tracked_clean_file(g, path, source, head):
    try:
        path = Path(path).resolve(strict=True)
    except OSError as error:
        raise g.IdentityError('build input is not tracked by pinned HEAD') from error
    if not path.is_relative_to(source):
        raise g.IdentityError('build input outside pinned source tree')
    relative = path.relative_to(source).as_posix()
    try:
        expected = g.git(source, 'rev-parse', head + ':' + relative)
        actual = g.git(source, 'hash-object', '--path=' + relative, str(path))
    except g.subprocess.CalledProcessError as error:
        raise g.IdentityError('build input is not tracked by pinned HEAD') from error
    if actual != expected:
        raise g.IdentityError('build input differs from pinned tree')
