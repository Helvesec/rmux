#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python(source: &str) -> Output {
    Command::new("python3")
        .arg("-c")
        .arg(source)
        .current_dir(repo_root())
        .output()
        .expect("run downstream payload fixture")
}

fn assert_fixture(source: &str) {
    let output = python(source);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const PRELUDE: &str = r#"
import copy
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile

sys.path.insert(0, 'scripts/release')
from downstream_payload import collect_files, validate_payload_document

SOURCE = 'a' * 40
RELEASE = {
    'id': 7,
    'ref': 'v1.2.3',
    'intent_id': 'intent:payload',
    'kind': 'stable',
    'tag_object_sha': 'b' * 40,
    'immutable': True,
}

def artifact(name, artifact_id):
    return {
        'artifact_id': artifact_id,
        'name': name,
        'archive_digest': 'sha256:' + 'c' * 64,
        'size_in_bytes': 10,
    }

def write(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def fixture(root):
    root = pathlib.Path(root)
    payload_dir = root / 'payload'
    payload_dir.mkdir()
    (payload_dir / 'rmux.rb').write_text('class Rmux < Formula; end\n', encoding='utf-8')
    predicate_path = root / 'publication-receipt-predicate.json'
    predicate = {
        'schema_version': 1,
        'predicate_type': 'https://rmux.io/attestations/release-publication-receipt/v1',
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': {
            **RELEASE,
            'created_at': '2026-07-20T00:00:00Z',
            'published_at': '2026-07-20T00:01:00Z',
        },
        'candidate': {
            'schema_version': 1,
            'status': 'shadow-non-authoritative',
            'repository_id': 1239918790,
            'source_git_sha': SOURCE,
            'candidate_run_id': 90,
            'candidate_run_attempt': 1,
            'manifest_run_id': 91,
            'manifest_run_attempt': 1,
            'manifest_workflow_id': 316223904,
            'manifest_workflow_path': '.github/workflows/release-shadow.yml',
            'manifest_artifact_id': 92,
            'manifest_artifact_digest': 'sha256:' + 'd' * 64,
            'manifest_sha256': 'e' * 64,
            'manifest_created_at': '2026-07-20T00:00:00Z',
            'manifest_expires_at': '2026-07-22T00:00:00Z',
        },
        'policy_audit': {},
        'authorization': {},
        'receipt': {
            'run_id': 100,
            'run_attempt': 1,
            'workflow_id': 316435347,
            'workflow_path': '.github/workflows/release-receipt.yml',
        },
        'verified_at': '2026-07-20T00:02:00Z',
        'asset_count': 2,
        'assets': [
            {'id': 1, 'name': 'SHA256SUMS', 'role': 'checksums', 'size': 10, 'digest': 'f' * 64},
            {'id': 2, 'name': 'rmux.tar.gz', 'role': 'archive', 'size': 10, 'digest': '1' * 64},
        ],
        'sha256sums_sha256': '2' * 64,
    }
    write(predicate_path, predicate)
    reference_path = root / 'receipt-reference.json'
    reference = {
        'schema_version': 1,
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'receipt': predicate['receipt'],
        'predicate_bundle': artifact(f'rmux-publication-receipt-{SOURCE}-7', 10),
        'predicate_sha256': digest(predicate_path),
        'envelope_bundle': artifact(f'rmux-publication-receipt-envelope-{SOURCE}-7', 11),
        'envelope_sha256': '3' * 64,
        'attestation': {
            'attestation_id': 'attestation-1',
            'bundle_file': 'publication-receipt.sigstore.json',
            'bundle_sha256': '4' * 64,
        },
        'verified_at': '2026-07-20T00:02:00Z',
    }
    write(reference_path, reference)
    producer_path = root / 'producer.json'
    producer = {
        'run_id': 100,
        'run_attempt': 1,
        'workflow_id': 316435347,
        'workflow_path': '.github/workflows/release-receipt.yml',
        'job_workflow_path': '.github/workflows/release-downstream-prepare.yml',
        'runner_group_id': 0,
        'runner_group_name': 'GitHub Actions',
        'runner_image': 'ubuntu-22.04',
    }
    write(producer_path, producer)
    metadata_path = root / 'artifact.json'
    metadata = {
        'artifact_id': 101,
        'name': f'rmux-downstream-homebrew_tap-payload-{SOURCE}-7',
        'digest': 'sha256:' + '5' * 64,
        'size_in_bytes': 27,
        'created_at': '2026-07-20T00:03:00Z',
        'updated_at': '2026-07-20T00:03:01Z',
        'expires_at': '2026-07-27T00:03:00Z',
        'run_id': 100,
        'source_git_sha': SOURCE,
    }
    write(metadata_path, metadata)
    document = root / 'channel-payload.json'
    subject = root / 'downstream-channel-payload-subject.json'
    common = [
        '--receipt-reference', str(reference_path),
        '--receipt-predicate', str(predicate_path),
        '--producer', str(producer_path),
        '--artifact-metadata', str(metadata_path),
        '--channel', 'homebrew_tap',
        '--payload-dir', str(payload_dir),
        '--file', 'homebrew-formula=rmux.rb',
        '--created-at', '2026-07-20T00:04:00Z',
    ]
    return {
        'root': root,
        'payload_dir': payload_dir,
        'reference': reference,
        'producer_path': producer_path,
        'document': document,
        'subject': subject,
        'common': common,
    }

def run_cli(arguments):
    return subprocess.run(
        [sys.executable, 'scripts/release/downstream_payload.py', *arguments],
        check=False,
        text=True,
        capture_output=True,
    )
"#;

#[test]
fn exact_payload_and_subject_round_trip() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    value = fixture(directory)
    created = run_cli(['create', *value['common'], '--output', str(value['document']), '--subject-output', str(value['subject'])])
    if created.returncode != 0:
        raise SystemExit(created.stderr)
    verified = run_cli(['verify', *value['common'], '--document', str(value['document']), '--subject', str(value['subject'])])
    if verified.returncode != 0:
        raise SystemExit(verified.stderr)
    subject = json.loads(value['subject'].read_text(encoding='utf-8'))
    if subject['payload_sha256'] != digest(value['document']):
        raise SystemExit('payload subject did not bind the exact document')
"#
    ));
}

#[test]
fn payload_rejects_producer_receipt_and_expiry_drift() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    value = fixture(directory)
    created = run_cli(['create', *value['common'], '--output', str(value['document']), '--subject-output', str(value['subject'])])
    if created.returncode != 0:
        raise SystemExit(created.stderr)
    payload = json.loads(value['document'].read_text(encoding='utf-8'))
    mutations = []
    forged = copy.deepcopy(payload)
    forged['producer']['workflow_id'] = 1
    mutations.append(forged)
    forged = copy.deepcopy(payload)
    forged['source_evidence']['receipt_predicate_sha256'] = '9' * 64
    mutations.append(forged)
    forged = copy.deepcopy(payload)
    forged['retention_expires_at'] = '2026-07-28T00:03:00Z'
    mutations.append(forged)
    for forged in mutations:
        try:
            validate_payload_document(
                forged,
                channel='homebrew_tap',
                source_sha=SOURCE,
                receipt_predicate_sha256=value['reference']['predicate_sha256'],
                release=RELEASE,
            )
        except ValueError:
            pass
        else:
            raise SystemExit('forged downstream payload was accepted')
"#
    ));
}

#[test]
fn payload_root_rejects_symlinks_and_unlisted_files() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    value = fixture(directory)
    (value['payload_dir'] / 'extra').symlink_to(value['payload_dir'] / 'rmux.rb')
    rejected = run_cli(['create', *value['common'], '--output', str(value['document']), '--subject-output', str(value['subject'])])
    if rejected.returncode == 0 or 'regular files' not in rejected.stderr:
        raise SystemExit(f'symlink payload was not rejected: {rejected.stderr}')
"#
    ));
}

#[test]
fn payload_collection_supports_multiarch_roles_and_rejects_duplicate_names() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    root = pathlib.Path(directory)
    names = [
        'rmux_0.9.0_amd64.deb',
        'rmux_0.9.0_arm64.deb',
        'rmux-0.9.0-1.x86_64.rpm',
        'rmux-0.9.0-1.aarch64.rpm',
    ]
    for name in names:
        (root / name).write_bytes(name.encode('ascii'))
    files = collect_files(root, [
        'debian=rmux_0.9.0_amd64.deb',
        'debian=rmux_0.9.0_arm64.deb',
        'rpm=rmux-0.9.0-1.x86_64.rpm',
        'rpm=rmux-0.9.0-1.aarch64.rpm',
    ])
    if [item['name'] for item in files] != sorted(names):
        raise SystemExit('multiarch payload files are not sorted')
    roles = [item['role'] for item in files]
    if roles.count('debian') != 2 or roles.count('rpm') != 2:
        raise SystemExit('multiarch payload roles were collapsed')
    try:
        collect_files(root, [
            'debian=rmux_0.9.0_amd64.deb',
            'rpm=rmux_0.9.0_amd64.deb',
        ])
    except ValueError:
        pass
    else:
        raise SystemExit('duplicate payload filename was accepted')
"#
    ));
}
