"""Consume a received canonical RC record and Sigstore bundle; never publish."""
import json
from pathlib import Path
import sys
import subprocess

# release_evidence imports its release siblings by bare name.
sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))
from release_evidence import file_hash  # noqa: E402
from strict_json import read_json_object

WORKFLOW = 'Helvesec/rmux/.github/workflows/canonical-native-build.yml'


def frozen_file(plan, name):
    value, digest = plan.get(name), plan.get(name + '_sha256')
    if not isinstance(value, str) or not isinstance(digest, str):
        raise ValueError('RC authority requires received provenance inputs: ' + name)
    path = Path(value)
    if not path.is_absolute() or path.is_symlink() or not path.is_file() or file_hash(path) != digest:
        raise ValueError('RC provenance input differs from the frozen plan: ' + name)
    return path


def unchanged(provenance):
    for item in provenance['inputs']:
        path = Path(item['path'])
        if path.is_symlink() or not path.is_file() or file_hash(path) != item['sha256']:
            raise ValueError('RC provenance inputs changed during qualification')


def verify(plan, archive, checksums, output):
    record_path, bundle, gh = (frozen_file(plan, name) for name in
                              ('provenance_receipt', 'provenance_bundle', 'gh'))
    if record_path.name != 'canonical-build-record.json':
        raise ValueError('RC provenance requires the canonical build receipt')
    record = read_json_object(record_path, 'RC provenance receipt')
    if (record.get('schema_version') != 1 or record.get('repository_id') != 1239918790
            or record.get('source_git_sha') != plan['sha'] or record.get('release_kind') != 'rc'
            or record.get('planned_release_ref') != plan.get('release_ref')
            or record.get('platform') != {'key': 'windows-x86_64',
                'target_triple': 'x86_64-pc-windows-msvc', 'archive_format': 'zip'}
            or record.get('runner') != {'image': 'windows-latest', 'os': 'Windows',
                'arch': 'X64', 'environment': 'github-hosted'}):
        raise ValueError('RC hosted receipt identity differs from SHA/ref/platform')
    files = record.get('files')
    if not isinstance(files, list) or any(not isinstance(item, dict) for item in files):
        raise ValueError('RC provenance receipt has no package subjects')
    for subject, role in ((archive, 'archive'), (checksums, 'checksums')):
        matches = [item for item in files if item.get('role') == role]
        if (len(matches) != 1 or matches[0].get('path') != subject.name
                or matches[0].get('sha256') != file_hash(subject)
                or matches[0].get('size') != subject.stat().st_size):
            raise ValueError('RC provenance receipt package digest differs')
    evidence = {'source_sha': plan['sha'], 'release_ref': plan['release_ref'],
                'archive_sha256': plan['archive_sha256'], 'inputs': [
                    {'path': str(path), 'sha256': plan[name + '_sha256']} for name, path in
                    (('provenance_receipt', record_path), ('provenance_bundle', bundle), ('gh', gh),
                     ('archive', archive), ('checksums', checksums))]}
    # Authenticate the record itself: its signed files list binds the archive and
    # checksums. A local JSON claim of hosted origin is insufficient.
    command = [str(gh), 'attestation', 'verify', str(record_path), '--bundle', str(bundle),
               '--repo', 'Helvesec/rmux', '--signer-workflow', WORKFLOW,
               '--source-digest', plan['sha'], '--source-ref', 'refs/heads/main',
               '--deny-self-hosted-runners', '--format', 'json']
    result = subprocess.run(command, capture_output=True, text=True, timeout=180)
    if result.returncode:
        raise ValueError('RC hosted provenance attestation verification failed')
    try:
        values = json.loads(result.stdout)
    except (ValueError, TypeError) as error:
        raise ValueError('RC attestation verifier returned invalid JSON') from error
    if not isinstance(values, list) or len(values) != 1 or not isinstance(values[0], dict):
        raise ValueError('RC requires exactly one verified hosted attestation')
    verified = values[0].get('verificationResult')
    if not isinstance(verified, dict):
        raise ValueError('RC attestation lacks a verification result')
    statement, signature = verified.get('statement'), verified.get('signature')
    if (not isinstance(statement, dict) or not isinstance(signature, dict)
            or not isinstance(signature.get('certificate'), dict)
            or not isinstance(verified.get('verifiedTimestamps'), list)
            or not verified['verifiedTimestamps']):
        raise ValueError('RC attestation lacks signed verification evidence')
    subject = {'name': record_path.name, 'digest': {'sha256': plan['provenance_receipt_sha256']}}
    subjects = statement.get('subject')
    if (not isinstance(subjects, list) or subjects.count(subject) != 1
            or any(item != subject for item in subjects if isinstance(item, dict)
                   and item.get('name') == record_path.name)):
        raise ValueError('RC verified subject differs from the received receipt bytes')
    unchanged(evidence)
    (output / 'provenance-verification.json').write_text(result.stdout, encoding='utf-8')
    return evidence
